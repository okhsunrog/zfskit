use crate::error::{ZfsError, classify_stderr};
use crate::names::{DatasetName, SnapshotName};
use crate::runner::{Cmd, CommandRunner};

/// Filesystem/volume rename options. Snapshot recursion uses the separate
/// [`SnapshotRenameOptions`] type. Moves must remain within the same pool.
#[derive(Default, Clone, Debug)]
pub struct RenameOptions {
    /// `-p`: create missing parents, mounting them according to inherited
    /// properties. Incompatible with `no_remount`.
    pub parents: bool,
    /// `-u`: keep filesystems mounted at their existing mountpoints. Only
    /// supported for filesystems; ZFS rejects volumes with this flag.
    pub no_remount: bool,
    /// `-f`: force unmount when necessary. Ignored by ZFS with `no_remount`.
    pub force_unmount: bool,
}

impl RenameOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parents(mut self) -> Self {
        self.parents = true;
        self
    }

    pub fn no_remount(mut self) -> Self {
        self.no_remount = true;
        self
    }

    pub fn force_unmount(mut self) -> Self {
        self.force_unmount = true;
        self
    }

    /// Reject incompatible flags before constructing a command.
    pub fn build_args(&self, source: &str, destination: &str) -> Result<Vec<String>, ZfsError> {
        if self.parents && self.no_remount {
            return Err(ZfsError::InvalidInput {
                message: "rename -p and -u are mutually exclusive".into(),
            });
        }
        let mut args = vec!["rename".into()];
        if self.parents {
            args.push("-p".into());
        }
        if self.no_remount {
            args.push("-u".into());
        }
        if self.force_unmount {
            args.push("-f".into());
        }
        args.extend([source.into(), destination.into()]);
        Ok(args)
    }
}

/// Options for snapshot rename within its current filesystem or volume.
#[derive(Default, Clone, Debug)]
pub struct SnapshotRenameOptions {
    /// `-r`: rename matching snapshot tags on descendant datasets too.
    pub recursive: bool,
}

impl SnapshotRenameOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recursive(mut self) -> Self {
        self.recursive = true;
        self
    }

    pub fn build_args(&self, source: &str, destination: &str) -> Vec<String> {
        let mut args = vec!["rename".into()];
        if self.recursive {
            args.push("-r".into());
        }
        args.extend([source.into(), destination.into()]);
        args
    }
}

/// Rename a filesystem or volume, validating both absolute dataset names.
pub async fn rename(
    runner: &dyn CommandRunner,
    source: &str,
    destination: &str,
    opts: &RenameOptions,
) -> Result<(), ZfsError> {
    DatasetName::parse(source)?;
    DatasetName::parse(destination)?;
    execute(runner, opts.build_args(source, destination)?).await
}

/// Rename a snapshot using full names. The destination must belong to the
/// source dataset. The handle API accepts just the new tag instead.
pub async fn rename_snapshot(
    runner: &dyn CommandRunner,
    source: &str,
    destination: &str,
    opts: &SnapshotRenameOptions,
) -> Result<(), ZfsError> {
    let source_name = SnapshotName::parse(source)?;
    let destination_name = SnapshotName::parse(destination)?;
    if source_name.dataset() != destination_name.dataset() {
        return Err(ZfsError::InvalidInput {
            message: "snapshots can only be renamed within their dataset".into(),
        });
    }
    execute(runner, opts.build_args(source, destination)).await
}

async fn execute(runner: &dyn CommandRunner, args: Vec<String>) -> Result<(), ZfsError> {
    let output = runner.run(Cmd::new("zfs").args(args)).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(classify_stderr(
        &String::from_utf8_lossy(&output.stderr),
        output.status.code(),
    ))
}
