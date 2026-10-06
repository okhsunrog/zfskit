use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::process::{ExitStatus, Output, Stdio};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use zeroize::{Zeroize, Zeroizing};

/// A planned command invocation. Owned, builder-style.
///
/// This is the value type the `CommandRunner` trait operates on. `RealRunner`
/// converts it to a `tokio::process::Command` at call time; `RecordingRunner`
/// uses it as a `HashMap` key for fixture lookup. Including stdin in the value
/// is deliberate: tests need to distinguish responses for the same `(program,
/// args)` invoked with different stdin (e.g., correct vs wrong passphrase).
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct Cmd {
    program: OsString,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    secret_stdin: bool,
}

impl Cmd {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            stdin: None,
            secret_stdin: false,
        }
    }

    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Plain stdin payload. Visible in `Display`/`Debug`.
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self.secret_stdin = false;
        self
    }

    /// Stdin payload redacted from `Display` (length still shown). Use for
    /// passphrases and other secrets that must not leak into logs.
    pub fn stdin_secret(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self.secret_stdin = true;
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }
    pub fn args_list(&self) -> &[OsString] {
        &self.args
    }
    pub fn stdin_bytes(&self) -> Option<&[u8]> {
        self.stdin.as_deref()
    }
}

impl Drop for Cmd {
    fn drop(&mut self) {
        if self.secret_stdin {
            if let Some(stdin) = &mut self.stdin {
                stdin.zeroize();
            }
        }
    }
}

impl fmt::Debug for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("Cmd");
        debug
            .field("program", &self.program)
            .field("args", &self.args);
        match (&self.stdin, self.secret_stdin) {
            (Some(bytes), true) => {
                debug.field("stdin", &format_args!("<redacted: {} bytes>", bytes.len()))
            }
            (Some(bytes), false) => debug.field("stdin", bytes),
            (None, _) => debug.field("stdin", &Option::<&[u8]>::None),
        };
        debug.finish()
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program.to_string_lossy())?;
        for a in &self.args {
            write!(f, " {}", a.to_string_lossy())?;
        }
        match (&self.stdin, self.secret_stdin) {
            (Some(b), true) => write!(f, " <secret stdin: {} bytes>", b.len())?,
            (Some(b), false) => write!(f, " <stdin: {} bytes>", b.len())?,
            (None, _) => {}
        }
        Ok(())
    }
}

impl fmt::Debug for ChildHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildHandle")
            .field("stdin", &self.stdin.is_some())
            .field("stdout", &self.stdout.is_some())
            .field("stderr", &self.stderr.is_some())
            .finish()
    }
}

/// An owned handle to a running child process. Returned by
/// [`CommandRunner::spawn`].
///
/// The stdio streams are boxed trait objects so that both `RealRunner` (real
/// `tokio::process::Child` handles) and `RecordingRunner` (in-memory cursors)
/// can return the same type.
///
/// **Callers must consume `stdout`/`stderr` before (or concurrently with)
/// calling `wait()`** to avoid deadlocking on the OS pipe buffer.
pub struct ChildHandle {
    /// Writable stdin. `None` if stdin was not piped (or for mock handles
    /// where writes are silently discarded via `tokio::io::sink()`).
    pub stdin: Option<Box<dyn AsyncWrite + Unpin + Send>>,
    /// Readable stdout. For `zfs send`, this carries the byte stream.
    pub stdout: Option<Box<dyn AsyncRead + Unpin + Send>>,
    /// Readable stderr. For `zfs recv`, read this to detect resume tokens.
    pub stderr: Option<Box<dyn AsyncRead + Unpin + Send>>,
    inner: ChildWaiter,
}

enum ChildWaiter {
    Process(tokio::process::Child),
    Mock(i32),
}

impl ChildHandle {
    fn from_process(mut child: tokio::process::Child) -> Self {
        let stdin = child
            .stdin
            .take()
            .map(|s| -> Box<dyn AsyncWrite + Unpin + Send> { Box::new(s) });
        let stdout = child
            .stdout
            .take()
            .map(|s| -> Box<dyn AsyncRead + Unpin + Send> { Box::new(s) });
        let stderr = child
            .stderr
            .take()
            .map(|s| -> Box<dyn AsyncRead + Unpin + Send> { Box::new(s) });
        Self {
            stdin,
            stdout,
            stderr,
            inner: ChildWaiter::Process(child),
        }
    }

    /// Creates a mock handle for use in `RecordingRunner` tests. Writes to
    /// `stdin` are silently discarded; `stdout` and `stderr` replay the given
    /// bytes.
    pub(crate) fn mock(stdout: Vec<u8>, stderr: Vec<u8>, exit_code: i32) -> Self {
        Self {
            stdin: Some(Box::new(tokio::io::sink())),
            stdout: Some(Box::new(std::io::Cursor::new(stdout))),
            stderr: Some(Box::new(std::io::Cursor::new(stderr))),
            inner: ChildWaiter::Mock(exit_code),
        }
    }

    /// Wait for the child process to exit and return its exit status. Callers
    /// must finish reading (or drop) `stdout`/`stderr` before calling this, or
    /// run the reads concurrently on separate tasks, to avoid pipe deadlocks.
    pub async fn wait(mut self) -> io::Result<ExitStatus> {
        // A retained stdin can keep a receiver waiting for EOF, while retained
        // stdout/stderr pipes can keep a producer blocked on a full buffer.
        // Streams explicitly taken by the caller are unaffected.
        drop(self.stdin.take());
        drop(self.stdout.take());
        drop(self.stderr.take());
        match self.inner {
            ChildWaiter::Process(mut c) => c.wait().await,
            ChildWaiter::Mock(code) => Ok(mock_exit_status(code)),
        }
    }

    /// Send SIGKILL to the child process without waiting for it to exit.
    /// Pair with [`Self::wait`] to reap. No-op for mock handles.
    ///
    /// Used by callers that need to abort an in-flight `zfs send` or
    /// `zfs recv` when their own cancellation token fires. Cancellation
    /// surfaces as a normal non-zero exit status from `wait()`; there is
    /// no dedicated "cancelled" error variant — interpret the result
    /// against your own cancellation state.
    pub fn start_kill(&mut self) -> io::Result<()> {
        match &mut self.inner {
            ChildWaiter::Process(c) => c.start_kill(),
            ChildWaiter::Mock(_) => Ok(()),
        }
    }
}

#[cfg(unix)]
fn mock_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}

// async-trait adds #[must_use] to methods returning an already-must-use
// boxed Future; Clippy 1.99 diagnoses these generated attributes.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, cmd: Cmd) -> Result<Output, io::Error>;

    /// Spawn the command and return a [`ChildHandle`] with piped stdio. The
    /// `Cmd.stdin` payload (if any) is ignored — use `child_handle.stdin` to
    /// write after spawning. The default implementation returns an
    /// `io::ErrorKind::Unsupported` error; override in runners that support
    /// streaming.
    async fn spawn(&self, cmd: Cmd) -> Result<ChildHandle, io::Error> {
        let _ = cmd;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "spawn not implemented for this CommandRunner",
        ))
    }
}

pub struct RealRunner;

/// Spawn `command` with `bytes` piped to stdin, collecting stdout/stderr.
/// The stdin feed runs concurrently with output collection — writing the
/// whole payload before draining the pipes would deadlock once the child
/// fills its stdout buffer while still reading stdin. Write errors are
/// deliberately dropped: a child that exits before consuming all of
/// stdin (e.g. `load-key` rejecting a passphrase) yields BrokenPipe,
/// and that must not mask the child's own exit status and stderr.
async fn run_with_stdin(
    mut command: tokio::process::Command,
    bytes: Vec<u8>,
) -> Result<Output, io::Error> {
    let bytes = Zeroizing::new(bytes);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take();
    let feed = async {
        if let Some(mut s) = stdin.take() {
            let _ = s.write_all(&bytes).await;
            let _ = s.shutdown().await;
        }
    };
    let (_, output) = tokio::join!(feed, child.wait_with_output());
    output
}

#[async_trait::async_trait]
impl CommandRunner for RealRunner {
    async fn run(&self, cmd: Cmd) -> Result<Output, io::Error> {
        let stdin_bytes = cmd.stdin.clone();
        let mut command = tokio::process::Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .env("LC_ALL", "C")
            .kill_on_drop(true);

        match stdin_bytes {
            None => command.output().await,
            Some(bytes) => run_with_stdin(command, bytes).await,
        }
    }

    async fn spawn(&self, cmd: Cmd) -> Result<ChildHandle, io::Error> {
        let mut command = tokio::process::Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // If the ChildHandle is dropped without an explicit start_kill +
            // wait, kill the child rather than leaking a long-running
            // zfs send/recv subprocess.
            .kill_on_drop(true);
        let child = command.spawn()?;
        Ok(ChildHandle::from_process(child))
    }
}

/// Connection target for [`SshCommandRunner`].
#[derive(Debug, Clone)]
pub struct SshTarget {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshTarget {
    pub fn new(user: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            user: user.into(),
            host: host.into(),
            port,
        }
    }

    /// Parse `[user@]host[:port]`. User defaults to `root`, port to 22.
    /// IPv6 literals need brackets to carry a port (`[::1]:2222`); a bare
    /// address (`::1`, `fe80::1`) is taken whole as the host with port 22
    /// — naive `rsplit(':')` would otherwise eat the last address group.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (user, rest) = match s.split_once('@') {
            Some((u, r)) => (u.to_string(), r),
            None => ("root".to_string(), s),
        };
        let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
            let Some((h, after)) = bracketed.split_once(']') else {
                return Err("unclosed '[' in IPv6 literal".into());
            };
            let port = match after {
                "" => 22,
                _ => {
                    let p = after
                        .strip_prefix(':')
                        .ok_or_else(|| format!("unexpected `{after}` after ']'"))?;
                    p.parse().map_err(|e| format!("port `{p}`: {e}"))?
                }
            };
            (h.to_string(), port)
        } else if rest.matches(':').count() > 1 {
            (rest.to_string(), 22)
        } else {
            match rest.rsplit_once(':') {
                Some((h, p)) => {
                    let port: u16 = p.parse().map_err(|e| format!("port `{p}`: {e}"))?;
                    (h.to_string(), port)
                }
                None => (rest.to_string(), 22),
            }
        };
        let target = Self { user, host, port };
        target.validate()?;
        Ok(target)
    }

    fn validate(&self) -> Result<(), String> {
        if self.user.is_empty() {
            return Err("empty user".into());
        }
        if self.user.starts_with('-') {
            return Err("SSH user cannot begin with '-'".into());
        }
        if self.host.is_empty() {
            return Err("empty host".into());
        }
        if self.port == 0 {
            return Err("SSH port must not be zero".into());
        }
        Ok(())
    }
}

/// Single-quote-escape one shell token. `'` becomes `'\''`.
fn shell_quote(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn quote_cmdline(cmd: &Cmd) -> io::Result<String> {
    let program = cmd.program.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH command program is not valid UTF-8",
        )
    })?;
    // Error classification depends on the stable English diagnostics emitted
    // by the C locale. Prefixing the remote command works even when sshd does
    // not accept locale environment forwarding.
    let mut s = format!("LC_ALL=C {}", shell_quote(program));
    for a in &cmd.args {
        let a = a.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH command argument is not valid UTF-8",
            )
        })?;
        s.push(' ');
        s.push_str(&shell_quote(a));
    }
    Ok(s)
}

/// SSH-dispatching runner. Wraps each `Cmd` in `ssh user@host -- '<quoted>'`,
/// forwards stdin transparently, returns the remote process's exit code,
/// stdout, and stderr verbatim.
///
/// Intended for integration tests against a throw-away VM so that real `zfs`
/// operations never touch the host's pools. Production applications should
/// run zfskit on the ZFS host and provide their own transport boundary.
///
/// Caveats:
/// - SSH client returns exit code 255 on connection/auth failure. The remote
///   command can also return 255 legitimately; the two cases are
///   indistinguishable.
/// - The runner shells out to the system `ssh` binary (and `sshpass` when a
///   password is configured). Both must be in `PATH`.
/// - `LogLevel=ERROR` and `UserKnownHostsFile=/dev/null` suppress most SSH
///   client diagnostics, but a small amount of SSH-side text may still mix
///   into the captured stderr.
#[derive(Clone)]
pub struct SshCommandRunner {
    target: SshTarget,
    /// When set, dispatch via `sshpass -e ssh ...`. The password is passed in
    /// `SSHPASS`, never argv, and zeroized when this runner is dropped.
    password: Option<Zeroizing<String>>,
}

impl SshCommandRunner {
    pub fn new(target: SshTarget) -> Self {
        Self {
            target,
            password: None,
        }
    }

    /// Construct from `ZFSKIT_SSH_TARGET=[user@]host[:port]`. Optionally
    /// reads `ZFSKIT_SSH_PASSWORD` for password auth via sshpass.
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("ZFSKIT_SSH_TARGET")
            .map_err(|_| "ZFSKIT_SSH_TARGET not set".to_string())?;
        let target = SshTarget::parse(&raw)?;
        let password = std::env::var("ZFSKIT_SSH_PASSWORD")
            .ok()
            .map(Zeroizing::new);
        Ok(Self { target, password })
    }

    pub fn with_password(mut self, pw: impl Into<String>) -> Self {
        self.password = Some(Zeroizing::new(pw.into()));
        self
    }

    /// Build `(program, args)` for the local process that wraps `cmd` in ssh.
    /// Extracted so tests can assert on the constructed argv without spawning.
    fn build_local_argv(&self, cmd: &Cmd) -> io::Result<(String, Vec<String>)> {
        self.target
            .validate()
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let mut argv: Vec<String> = Vec::new();
        let program = if self.password.is_some() {
            argv.extend(["-e".into(), "ssh".into()]);
            "sshpass".to_string()
        } else {
            "ssh".to_string()
        };
        argv.extend([
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
        ]);
        if self.password.is_some() {
            argv.extend([
                "-o".into(),
                "PreferredAuthentications=password".into(),
                "-o".into(),
                "PubkeyAuthentication=no".into(),
            ]);
        } else {
            argv.extend(["-o".into(), "BatchMode=yes".into()]);
        }
        argv.extend(["-p".into(), self.target.port.to_string()]);
        argv.push(format!("{}@{}", self.target.user, self.target.host));
        argv.push("--".into());
        argv.push(quote_cmdline(cmd)?);
        Ok((program, argv))
    }

    fn build_command(&self, cmd: &Cmd) -> io::Result<tokio::process::Command> {
        let (program, argv) = self.build_local_argv(cmd)?;
        let mut local = tokio::process::Command::new(program);
        local.args(argv).env("LC_ALL", "C").kill_on_drop(true);
        if let Some(password) = &self.password {
            local.env("SSHPASS", password.as_str());
        }
        Ok(local)
    }
}

#[async_trait::async_trait]
impl CommandRunner for SshCommandRunner {
    async fn run(&self, cmd: Cmd) -> Result<Output, io::Error> {
        let local = self.build_command(&cmd)?;
        let stdin_bytes = cmd.stdin_bytes().map(<[u8]>::to_vec);
        match stdin_bytes {
            None => {
                let mut local = local;
                local.output().await
            }
            Some(bytes) => run_with_stdin(local, bytes).await,
        }
    }

    async fn spawn(&self, cmd: Cmd) -> Result<ChildHandle, io::Error> {
        let mut local = self.build_command(&cmd)?;
        local
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = local.spawn()?;
        Ok(ChildHandle::from_process(child))
    }
}

/// Fixture record for spawn responses in `RecordingRunner`.
struct SpawnFixture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
}

/// Keyed-by-`Cmd` mock runner. Tests call `record(cmd, stdout, stderr, code)`
/// to install a fixture; calls without a matching fixture return an
/// `io::ErrorKind::NotFound` naming the unmatched command.
///
/// Keyed (rather than sequenced) lookup makes refactors that reorder calls
/// non-breaking. Keying on the whole `Cmd` (including stdin) lets tests
/// distinguish, e.g., `load-key` with correct vs wrong passphrase.
pub struct RecordingRunner {
    responses: HashMap<Cmd, Output>,
    spawn_responses: HashMap<Cmd, SpawnFixture>,
}

impl RecordingRunner {
    pub fn new() -> Self {
        Self {
            responses: HashMap::new(),
            spawn_responses: HashMap::new(),
        }
    }

    pub fn record(mut self, cmd: Cmd, stdout: Vec<u8>, stderr: Vec<u8>, exit_code: i32) -> Self {
        self.responses
            .insert(cmd, make_output(stdout, stderr, exit_code));
        self
    }

    /// Install a spawn fixture: when `spawn(cmd)` is called, returns a
    /// `ChildHandle` whose stdout/stderr replay the given bytes and whose
    /// `wait()` returns the given exit code. Stdin writes are discarded.
    pub fn record_spawn(
        mut self,
        cmd: Cmd,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: i32,
    ) -> Self {
        self.spawn_responses.insert(
            cmd,
            SpawnFixture {
                stdout,
                stderr,
                exit_code,
            },
        );
        self
    }
}

impl Default for RecordingRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl CommandRunner for RecordingRunner {
    async fn run(&self, cmd: Cmd) -> Result<Output, io::Error> {
        match self.responses.get(&cmd) {
            Some(out) => Ok(clone_output(out)),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("RecordingRunner: no fixture for `{cmd}`"),
            )),
        }
    }

    async fn spawn(&self, cmd: Cmd) -> Result<ChildHandle, io::Error> {
        match self.spawn_responses.get(&cmd) {
            Some(f) => Ok(ChildHandle::mock(
                f.stdout.clone(),
                f.stderr.clone(),
                f.exit_code,
            )),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("RecordingRunner: no spawn fixture for `{cmd}`"),
            )),
        }
    }
}

#[cfg(unix)]
fn make_output(stdout: Vec<u8>, stderr: Vec<u8>, code: i32) -> Output {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw((code & 0xff) << 8),
        stdout,
        stderr,
    }
}

fn clone_output(out: &Output) -> Output {
    Output {
        status: out.status,
        stdout: out.stdout.clone(),
        stderr: out.stderr.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_builder_basic() {
        let cmd = Cmd::new("zfs").arg("list").args(["-j", "-p"]);
        assert_eq!(cmd.program(), "zfs");
        assert_eq!(cmd.args_list(), &["list", "-j", "-p"]);
        assert!(cmd.stdin_bytes().is_none());
    }

    #[test]
    fn cmd_stdin_visible_in_display() {
        let cmd = Cmd::new("zfs").arg("recv").stdin(b"data".to_vec());
        assert_eq!(format!("{cmd}"), "zfs recv <stdin: 4 bytes>");
    }

    #[test]
    fn cmd_secret_stdin_redacted() {
        let cmd = Cmd::new("zfs")
            .args(["load-key", "tank"])
            .stdin_secret(b"hunter2".to_vec());
        assert_eq!(
            format!("{cmd}"),
            "zfs load-key tank <secret stdin: 7 bytes>"
        );
        let debug = format!("{cmd:?}");
        assert!(debug.contains("<redacted: 7 bytes>"));
        assert!(!debug.contains("104, 117, 110, 116, 101, 114, 50"));
    }

    #[test]
    fn cmd_eq_hash_includes_stdin() {
        let a = Cmd::new("zfs").arg("load-key").stdin_secret(b"a".to_vec());
        let b = Cmd::new("zfs").arg("load-key").stdin_secret(b"b".to_vec());
        let a2 = Cmd::new("zfs").arg("load-key").stdin_secret(b"a".to_vec());
        assert_ne!(a, b);
        assert_eq!(a, a2);
    }

    #[tokio::test]
    async fn recording_runner_returns_fixture() {
        let runner =
            RecordingRunner::new().record(Cmd::new("echo").arg("hi"), b"hi\n".to_vec(), vec![], 0);
        let out = runner.run(Cmd::new("echo").arg("hi")).await.unwrap();
        assert_eq!(out.stdout, b"hi\n");
        assert!(out.status.success());
    }

    #[tokio::test]
    async fn recording_runner_distinguishes_stdin() {
        let runner = RecordingRunner::new()
            .record(
                Cmd::new("zfs")
                    .args(["load-key", "tank"])
                    .stdin_secret(b"correct".to_vec()),
                vec![],
                vec![],
                0,
            )
            .record(
                Cmd::new("zfs")
                    .args(["load-key", "tank"])
                    .stdin_secret(b"wrong".to_vec()),
                vec![],
                b"wrong key".to_vec(),
                1,
            );

        let ok = runner
            .run(
                Cmd::new("zfs")
                    .args(["load-key", "tank"])
                    .stdin_secret(b"correct".to_vec()),
            )
            .await
            .unwrap();
        assert!(ok.status.success());

        let bad = runner
            .run(
                Cmd::new("zfs")
                    .args(["load-key", "tank"])
                    .stdin_secret(b"wrong".to_vec()),
            )
            .await
            .unwrap();
        assert!(!bad.status.success());
        assert_eq!(bad.stderr, b"wrong key");
    }

    #[tokio::test]
    async fn recording_runner_unmatched_returns_not_found() {
        let runner = RecordingRunner::new();
        let err = runner
            .run(Cmd::new("zfs").arg("list"))
            .await
            .expect_err("unmatched call must error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("zfs list"));
    }

    #[tokio::test]
    async fn spawn_fixture_streams_stdout_and_waits() {
        use tokio::io::AsyncReadExt;

        let runner = RecordingRunner::new().record_spawn(
            Cmd::new("zfs").args(["send", "tank/data@snap1"]),
            b"stream-data".to_vec(),
            vec![],
            0,
        );
        let mut handle = runner
            .spawn(Cmd::new("zfs").args(["send", "tank/data@snap1"]))
            .await
            .unwrap();
        let mut buf = Vec::new();
        handle
            .stdout
            .as_mut()
            .unwrap()
            .read_to_end(&mut buf)
            .await
            .unwrap();
        assert_eq!(buf, b"stream-data");
        let status = handle.wait().await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn spawn_unmatched_returns_not_found() {
        let runner = RecordingRunner::new();
        let err = runner
            .spawn(Cmd::new("zfs").args(["send", "tank/data@snap1"]))
            .await
            .expect_err("unmatched spawn must error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn shell_quote_simple() {
        assert_eq!(shell_quote("zfs"), "'zfs'");
        assert_eq!(shell_quote("tank/data@snap1"), "'tank/data@snap1'");
    }

    #[test]
    fn shell_quote_with_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_with_spaces_and_metachars() {
        assert_eq!(shell_quote("a b $c `d` ; e"), "'a b $c `d` ; e'");
    }

    #[test]
    fn ssh_target_parse_full() {
        let t = SshTarget::parse("alice@example.com:2222").unwrap();
        assert_eq!(t.user, "alice");
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 2222);
    }

    #[test]
    fn ssh_target_parse_defaults() {
        let t = SshTarget::parse("example.com").unwrap();
        assert_eq!(t.user, "root");
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 22);
    }

    #[test]
    fn ssh_target_parse_user_only() {
        let t = SshTarget::parse("bob@host").unwrap();
        assert_eq!(t.user, "bob");
        assert_eq!(t.port, 22);
    }

    #[test]
    fn ssh_target_parse_host_port_only() {
        let t = SshTarget::parse("host:2225").unwrap();
        assert_eq!(t.user, "root");
        assert_eq!(t.host, "host");
        assert_eq!(t.port, 2225);
    }

    #[test]
    fn ssh_target_parse_rejects_empty_host() {
        assert!(SshTarget::parse("user@:22").is_err());
    }

    #[test]
    fn ssh_target_parse_rejects_invalid_user_and_port() {
        assert!(SshTarget::parse("@host").is_err());
        assert!(SshTarget::parse("-oProxyCommand=bad@host").is_err());
        assert!(SshTarget::parse("host:0").is_err());
    }

    #[test]
    fn ssh_target_parse_bare_ipv6_takes_whole_as_host() {
        let t = SshTarget::parse("root@::1").unwrap();
        assert_eq!(t.host, "::1");
        assert_eq!(t.port, 22);
        let t = SshTarget::parse("fe80::1").unwrap();
        assert_eq!(t.host, "fe80::1");
        assert_eq!(t.port, 22);
    }

    #[test]
    fn ssh_target_parse_bracketed_ipv6_with_port() {
        let t = SshTarget::parse("alice@[::1]:2222").unwrap();
        assert_eq!(t.user, "alice");
        assert_eq!(t.host, "::1");
        assert_eq!(t.port, 2222);
        let t = SshTarget::parse("[fe80::1]").unwrap();
        assert_eq!(t.host, "fe80::1");
        assert_eq!(t.port, 22);
    }

    #[test]
    fn ssh_target_parse_rejects_malformed_ipv6_brackets() {
        assert!(SshTarget::parse("[::1").is_err());
        assert!(SshTarget::parse("[::1]junk").is_err());
        assert!(SshTarget::parse("[::1]:notaport").is_err());
    }

    #[tokio::test]
    async fn real_runner_stdin_larger_than_pipe_buffer_does_not_deadlock() {
        // `cat` echoes stdin to stdout; with a payload well past the 64 KiB
        // pipe buffer, feeding stdin to completion before draining stdout
        // would deadlock. run_with_stdin drives both concurrently.
        let payload = vec![b'x'; 1 << 20];
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            RealRunner.run(Cmd::new("cat").stdin(payload.clone())),
        )
        .await
        .expect("must not deadlock")
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), payload.len());
    }

    #[tokio::test]
    async fn real_runner_child_ignoring_stdin_still_reports_output() {
        // `true` exits without reading stdin; the resulting BrokenPipe on
        // the feed side must not mask the child's exit status.
        let out = RealRunner
            .run(Cmd::new("true").stdin(vec![b'x'; 1 << 20]))
            .await
            .unwrap();
        assert!(out.status.success());
    }

    #[tokio::test]
    async fn real_runner_forces_stable_c_locale() {
        let out = RealRunner
            .run(Cmd::new("sh").args(["-c", "printf %s \"$LC_ALL\""]))
            .await
            .unwrap();
        assert_eq!(out.stdout, b"C");
    }

    #[test]
    fn ssh_runner_argv_pubkey_mode() {
        let r = SshCommandRunner::new(SshTarget::new("root", "localhost", 2225));
        let (prog, argv) = r
            .build_local_argv(&Cmd::new("zfs").args(["list", "-j"]))
            .unwrap();
        assert_eq!(prog, "ssh");
        assert_eq!(
            argv,
            vec![
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "BatchMode=yes",
                "-p",
                "2225",
                "root@localhost",
                "--",
                "LC_ALL=C 'zfs' 'list' '-j'",
            ]
        );
    }

    #[test]
    fn ssh_runner_argv_password_mode_uses_sshpass() {
        let r = SshCommandRunner::new(SshTarget::new("root", "localhost", 2225))
            .with_password("super-secret");
        let (prog, argv) = r.build_local_argv(&Cmd::new("echo").arg("ok")).unwrap();
        assert_eq!(prog, "sshpass");
        assert_eq!(&argv[0..2], &["-e", "ssh"]);
        assert!(!argv.iter().any(|arg| arg.contains("super-secret")));
        assert!(argv.contains(&"PreferredAuthentications=password".to_string()));
        assert!(argv.contains(&"PubkeyAuthentication=no".to_string()));
        assert!(!argv.contains(&"BatchMode=yes".to_string()));
        assert_eq!(argv.last().unwrap(), "LC_ALL=C 'echo' 'ok'");
    }

    #[test]
    fn ssh_runner_argv_quotes_arg_with_single_quote() {
        let r = SshCommandRunner::new(SshTarget::new("root", "h", 22));
        let (_, argv) = r
            .build_local_argv(&Cmd::new("sh").args(["-c", "echo it's me"]))
            .unwrap();
        assert_eq!(
            argv.last().unwrap(),
            "LC_ALL=C 'sh' '-c' 'echo it'\\''s me'"
        );
    }

    #[tokio::test]
    async fn spawn_mock_stdin_accepts_writes() {
        use tokio::io::AsyncWriteExt;

        let runner = RecordingRunner::new().record_spawn(
            Cmd::new("zfs").args(["recv", "tank/data"]),
            vec![],
            vec![],
            0,
        );
        let mut handle = runner
            .spawn(Cmd::new("zfs").args(["recv", "tank/data"]))
            .await
            .unwrap();
        // Writes succeed (data is discarded by tokio::io::sink()).
        handle
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"fake-stream-data")
            .await
            .unwrap();
        let status = handle.wait().await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn real_runner_spawn_start_kill_aborts_long_running_child() {
        // sleep 30 — would block the test for half a minute if start_kill
        // didn't actually terminate it.
        let mut handle = RealRunner.spawn(Cmd::new("sleep").arg("30")).await.unwrap();
        handle.start_kill().expect("start_kill returns Ok");
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), handle.wait())
            .await
            .expect("wait completes promptly after start_kill")
            .expect("wait returns a status");
        assert!(!status.success(), "killed child must report non-success");
    }

    #[tokio::test]
    async fn wait_closes_untaken_stdin_before_waiting() {
        let handle = RealRunner.spawn(Cmd::new("cat")).await.unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait())
            .await
            .expect("wait must close retained stdin")
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn real_runner_run_is_killed_when_future_is_cancelled() {
        let marker = std::env::temp_dir().join(format!("zfskit-cancel-{}", std::process::id()));
        let script = format!("echo $$ > {}; sleep 30", marker.display());
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            RealRunner.run(Cmd::new("sh").args(["-c", &script])),
        )
        .await;
        assert!(
            result.is_err(),
            "command should still be running at timeout"
        );
        let pid: u32 = std::fs::read_to_string(&marker)
            .expect("child wrote pid")
            .trim()
            .parse()
            .unwrap();
        let mut gone = false;
        for _ in 0..20 {
            if !std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let _ = std::fs::remove_file(marker);
        assert!(gone, "cancelled run child {pid} remained alive");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_runner_rejects_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let runner = SshCommandRunner::new(SshTarget::new("root", "localhost", 22));
        let error = runner
            .run(Cmd::new("zfs").arg(OsString::from_vec(vec![0xff])))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn real_runner_kill_on_drop_terminates_child() {
        // Spawn a sleep, capture its pid, drop the handle without wait.
        // kill_on_drop should cause tokio to reap it. We poll /proc to confirm.
        let pid = {
            let handle = RealRunner.spawn(Cmd::new("sleep").arg("30")).await.unwrap();
            // Pull pid from the underlying tokio Child via the inner enum.
            match &handle.inner {
                ChildWaiter::Process(c) => c.id().expect("child has a pid"),
                ChildWaiter::Mock(_) => unreachable!("RealRunner produces Process variant"),
            }
        };
        // Give tokio a moment to fire the kill + reap.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
        assert!(!alive, "dropped child (pid {pid}) should be killed");
    }

    #[tokio::test]
    async fn mock_handle_start_kill_is_noop() {
        let runner =
            RecordingRunner::new().record_spawn(Cmd::new("echo").arg("ok"), vec![], vec![], 0);
        let mut handle = runner.spawn(Cmd::new("echo").arg("ok")).await.unwrap();
        handle.start_kill().expect("mock start_kill is Ok");
    }
}
