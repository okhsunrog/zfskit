use std::ffi::OsString;
use std::path::PathBuf;

use super::PoolSearchSource;
use crate::error::{ZfsError, classify_stderr};
use crate::names::PoolName;
use crate::runner::{Cmd, CommandRunner};

/// Options for importing one pool. Advanced recovery/rewind and missing-log
/// modes are intentionally not exposed: they can discard committed data.
#[derive(Default, Clone, Debug)]
pub struct ImportOptions {
    /// `-f`: force import even if the pool appears in use by another system.
    pub force: bool,
    /// `-N`: do not mount filesystems after import.
    pub no_mount: bool,
    /// `-R`: alternate root; also sets `cachefile=none` unless overridden.
    pub altroot: Option<PathBuf>,
    /// Repeated `-o property=value` pool properties applied at import time.
    pub properties: Vec<(String, String)>,
    /// Search configuration shared with discovery.
    pub search_source: PoolSearchSource,
    /// `-l`: load keys for encrypted datasets ZFS attempts to mount.
    /// Incompatible with `no_mount` (`-N`). Prompt keylocations require input; this option
    /// supplies no passphrases or interactive handling. Prefer explicit
    /// dataset key-loading APIs when input needs to be controlled.
    pub load_keys: bool,
}

impl ImportOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn force(mut self) -> Self {
        self.force = true;
        self
    }

    pub fn no_mount(mut self) -> Self {
        self.no_mount = true;
        self
    }

    pub fn altroot(mut self, path: impl Into<PathBuf>) -> Self {
        self.altroot = Some(path.into());
        self
    }

    pub fn property(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.push((name.into(), value.into()));
        self
    }

    /// Thin wrapper over the import-only `readonly=on` pool property.
    pub fn readonly(self) -> Self {
        self.property("readonly", "on")
    }

    pub fn search_source(mut self, source: PoolSearchSource) -> Self {
        self.search_source = source;
        self
    }

    pub fn load_keys(mut self) -> Self {
        self.load_keys = true;
        self
    }

    /// Arguments preserve native path bytes. `pool` is a name or decimal GUID;
    /// use the validated execution functions rather than running these directly.
    pub fn build_args(&self, pool: &str) -> Result<Vec<OsString>, ZfsError> {
        if self.load_keys && self.no_mount {
            return Err(ZfsError::InvalidInput {
                message: "import -l and -N are mutually exclusive".into(),
            });
        }
        let mut args = vec!["import".into()];
        if self.force {
            args.push("-f".into());
        }
        if self.no_mount {
            args.push("-N".into());
        }
        if let Some(path) = &self.altroot {
            args.extend(["-R".into(), path.as_os_str().into()]);
        }
        for (name, value) in &self.properties {
            args.extend(["-o".into(), format!("{name}={value}").into()]);
        }
        self.search_source.append_args(&mut args);
        if self.load_keys {
            args.push("-l".into());
        }
        args.push(pool.into());
        Ok(args)
    }
}

/// Import by pool name or decimal GUID string. Both are validated before
/// execution. Prefer [`import_by_guid`] when the identifier is already typed.
pub async fn import(
    runner: &dyn CommandRunner,
    pool: &str,
    opts: &ImportOptions,
) -> Result<(), ZfsError> {
    if !pool.is_empty() && pool.bytes().all(|b| b.is_ascii_digit()) {
        let guid = pool.parse::<u64>().map_err(|_| ZfsError::InvalidInput {
            message: format!("pool GUID exceeds u64: {pool:?}"),
        })?;
        return import_by_guid(runner, guid, opts).await;
    }
    PoolName::parse(pool)?;
    execute(runner, opts.build_args(pool)?).await
}

/// Import using the unsigned 64-bit pool identifier instead of its name.
pub async fn import_by_guid(
    runner: &dyn CommandRunner,
    guid: u64,
    opts: &ImportOptions,
) -> Result<(), ZfsError> {
    execute(runner, opts.build_args(&guid.to_string())?).await
}

async fn execute(runner: &dyn CommandRunner, args: Vec<OsString>) -> Result<(), ZfsError> {
    let output = runner.run(Cmd::new("zpool").args(args)).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(classify_stderr(
        &String::from_utf8_lossy(&output.stderr),
        output.status.code(),
    ))
}
