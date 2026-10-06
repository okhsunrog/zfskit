use crate::error::{ZfsError, classify_stderr};
use crate::names::DatasetName;
use crate::runner::{Cmd, CommandRunner};

/// Promote a clone, reversing its origin dependency. ZFS transfers ownership
/// of the origin snapshot and earlier snapshots to the promoted dataset.
/// Conflicting snapshot names must be resolved by the caller before promotion.
pub async fn promote(runner: &dyn CommandRunner, dataset: &str) -> Result<(), ZfsError> {
    DatasetName::parse(dataset)?;
    let output = runner
        .run(Cmd::new("zfs").args(["promote", dataset]))
        .await?;
    if output.status.success() {
        return Ok(());
    }
    Err(classify_stderr(
        &String::from_utf8_lossy(&output.stderr),
        output.status.code(),
    ))
}
