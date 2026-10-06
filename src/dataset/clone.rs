use crate::error::{ZfsError, classify_stderr};
use crate::names::{DatasetName, SnapshotName};
use crate::runner::{Cmd, CommandRunner};

/// Options for `zfs clone`. The clone has the source dataset's type and must
/// be in the same pool. ZFS mounts filesystems according to their properties;
/// use `canmount=off` or `mountpoint=none` when mounting is unwanted.
#[derive(Default, Clone, Debug)]
pub struct CloneOptions {
    /// `-p`: create missing parents. With this flag an existing destination
    /// succeeds without cloning or applying properties to it.
    pub parents: bool,
    /// Repeated `-o name=value` properties set at creation.
    pub properties: Vec<(String, String)>,
}

impl CloneOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parents(mut self) -> Self {
        self.parents = true;
        self
    }

    pub fn property(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.push((name.into(), value.into()));
        self
    }

    pub fn build_args(&self, snapshot: &str, destination: &str) -> Vec<String> {
        let mut args = vec!["clone".into()];
        if self.parents {
            args.push("-p".into());
        }
        for (name, value) in &self.properties {
            args.extend(["-o".into(), format!("{name}={value}")]);
        }
        args.extend([snapshot.into(), destination.into()]);
        args
    }
}

/// Clone a snapshot into a filesystem or volume. Both names are validated
/// before command execution; ZFS checks existence and same-pool constraints.
pub async fn clone(
    runner: &dyn CommandRunner,
    snapshot: &str,
    destination: &str,
    opts: &CloneOptions,
) -> Result<(), ZfsError> {
    SnapshotName::parse(snapshot)?;
    DatasetName::parse(destination)?;
    let output = runner
        .run(Cmd::new("zfs").args(opts.build_args(snapshot, destination)))
        .await?;
    if output.status.success() {
        return Ok(());
    }
    Err(classify_stderr(
        &String::from_utf8_lossy(&output.stderr),
        output.status.code(),
    ))
}
