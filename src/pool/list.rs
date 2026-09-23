use crate::error::{ZfsError, classify_stderr};
use crate::models::{ZpoolListEntry, ZpoolListOutput};
use crate::runner::{Cmd, CommandRunner};

#[derive(Default, Clone, Debug)]
pub struct ListOptions {
    /// Empty = list all visible pools. Non-empty restricts to named pools.
    pub pools: Vec<String>,
    /// Empty = let zpool return its default property set.
    pub properties: Vec<String>,
}

impl ListOptions {
    pub fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec!["list".into(), "-j".into(), "-p".into()];
        if !self.properties.is_empty() {
            args.push("-o".into());
            args.push(self.properties.join(","));
        }
        for p in &self.pools {
            args.push(p.clone());
        }
        args
    }
}

pub async fn list(
    runner: &dyn CommandRunner,
    opts: &ListOptions,
) -> Result<Vec<ZpoolListEntry>, ZfsError> {
    let output = runner
        .run(Cmd::new("zpool").args(opts.build_args()))
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(classify_stderr(&stderr, output.status.code()));
    }
    // With no pools imported, `zpool list -j` exits 0 without printing any JSON
    // (OpenZFS 2.4), unlike `zpool status -j` and `zfs list -j`.
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let parsed: ZpoolListOutput =
        serde_json::from_slice(&output.stdout).map_err(|e| ZfsError::Parse {
            command: "zpool list",
            message: e.to_string(),
        })?;
    parsed.output_version.validate("zpool list")?;
    let mut entries: Vec<ZpoolListEntry> = parsed.pools.into_values().collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingRunner;

    #[tokio::test]
    async fn no_imported_pools_is_an_empty_list() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zpool").args(["list", "-j", "-p"]),
            vec![],
            vec![],
            0,
        );
        let pools = list(&runner, &ListOptions::default()).await.unwrap();
        assert!(pools.is_empty());
    }

    #[test]
    fn build_args_default() {
        assert_eq!(
            ListOptions::default().build_args(),
            vec!["list", "-j", "-p"]
        );
    }

    #[test]
    fn build_args_full() {
        let opts = ListOptions {
            pools: vec!["tank".into()],
            properties: vec!["size".into(), "free".into()],
        };
        assert_eq!(
            opts.build_args(),
            vec!["list", "-j", "-p", "-o", "size,free", "tank"]
        );
    }
}
