//! High-level, entity-typed handles over `CommandRunner`.
//!
//! `Zfs` is the entry point. It hands out `Pool` handles by name and an
//! absolute-name `Dataset`, `Snapshot`, and `Bookmark` shortcuts. Distinct
//! handles keep invalid operations out of the API: snapshots expose holds and
//! rollback, while filesystems expose mount and child creation.
//!
//! The handle layer exists for ergonomics — consumers carry a single `Zfs`
//! value instead of threading a `&dyn CommandRunner` through every call,
//! and they navigate by relative name once a parent is in scope. Tests can
//! still target the free functions directly when parser/error-classification
//! detail matters.
use std::sync::Arc;

use crate::dataset::{
    CreateOptions, GetOptions, ListOptions, MountOptions, SetOptions, UnmountOptions, ZfsGetEntry,
    ZfsListEntry,
};
use crate::error::ZfsError;
use crate::models::{PropertyValue, ZpoolGetEntry, ZpoolListEntry, ZpoolStatusEntry};
use crate::names::{BookmarkName, DatasetName, NameError, PoolName, SnapshotName};
use crate::pool::{
    DestroyOptions, DiscoveredPool, ExportOptions, GetOptions as PoolGetOptions, ImportOptions,
    ListOptions as PoolListOptions, PoolCreateOptions,
};
use crate::runner::{CommandRunner, RealRunner};

/// Top-level handle. Construct once with [`Zfs::new`] (uses the system's
/// `zfs(8)`/`zpool(8)`) or [`Zfs::with_runner`] (custom runner — typically
/// `RecordingRunner` in tests).
#[derive(Clone)]
pub struct Zfs {
    runner: Arc<dyn CommandRunner>,
}

impl Zfs {
    pub fn new() -> Self {
        Self {
            runner: Arc::new(RealRunner),
        }
    }

    pub fn with_runner<R: CommandRunner + 'static>(runner: R) -> Self {
        Self {
            runner: Arc::new(runner),
        }
    }

    /// Access the advanced execution backend for operations not yet surfaced
    /// by a typed handle. Application state should store `Zfs`, not the runner.
    pub fn command_runner(&self) -> &dyn CommandRunner {
        &*self.runner
    }

    /// Handle bound to a specific zpool by name.
    pub fn pool(&self, name: impl Into<String>) -> Result<Pool, NameError> {
        Ok(Pool {
            runner: self.runner.clone(),
            name: PoolName::parse(name)?,
        })
    }

    /// Sugar: handle to any dataset by absolute name. Equivalent to
    /// `zfs.pool("a").unwrap().dataset("b/c")` for `zfs.dataset("a/b/c").unwrap()` — useful
    /// when you already have the full path and don't need a `Pool` handle.
    pub fn dataset(&self, abs_name: impl Into<String>) -> Result<Dataset, NameError> {
        Ok(Dataset {
            runner: self.runner.clone(),
            name: DatasetName::parse(abs_name)?,
        })
    }

    pub fn snapshot(&self, name: impl Into<String>) -> Result<Snapshot, NameError> {
        Ok(Snapshot {
            runner: self.runner.clone(),
            name: SnapshotName::parse(name)?,
        })
    }

    pub fn bookmark(&self, name: impl Into<String>) -> Result<Bookmark, NameError> {
        Ok(Bookmark {
            runner: self.runner.clone(),
            name: BookmarkName::parse(name)?,
        })
    }

    /// List datasets across the system.
    pub async fn list_datasets(&self, opts: &ListOptions) -> Result<Vec<ZfsListEntry>, ZfsError> {
        crate::dataset::list(&*self.runner, opts).await
    }

    /// List pools imported on the system.
    pub async fn list_pools(
        &self,
        opts: &PoolListOptions,
    ) -> Result<Vec<ZpoolListEntry>, ZfsError> {
        crate::pool::list(&*self.runner, opts).await
    }

    /// `zpool status -j` for all pools.
    pub async fn pool_status_all(&self) -> Result<Vec<ZpoolStatusEntry>, ZfsError> {
        crate::pool::status_all(&*self.runner).await
    }

    /// `zpool import` (no args) — discover pools available for import.
    /// Returns an empty Vec when no pools are visible.
    pub async fn discover_importable_pools(&self) -> Result<Vec<DiscoveredPool>, ZfsError> {
        crate::pool::discover(&*self.runner).await
    }

    /// Discover importable pools using a configured search source.
    pub async fn discover_importable_pools_with(
        &self,
        opts: &crate::pool::DiscoverOptions,
    ) -> Result<Vec<DiscoveredPool>, ZfsError> {
        crate::pool::discover_with(&*self.runner, opts).await
    }

    /// Import a discovered pool by GUID, returning a handle with its exported
    /// name. Invalid names or IDs fail before execution. The caller must pass
    /// the desired search source again; discovery does not store it in the pool.
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), zfskit::ZfsError> {
    /// use zfskit::pool::{DiscoverOptions, ImportOptions, PoolSearchSource};
    /// let zfs = zfskit::Zfs::new();
    /// let source = PoolSearchSource::Directories(vec!["/dev/disk/by-id".into()]);
    /// let pools = zfs.discover_importable_pools_with(
    ///     &DiscoverOptions::new().search_source(source.clone()),
    /// ).await?;
    /// for discovered in pools {
    ///     let pool = zfs.import_pool(&discovered,
    ///         &ImportOptions::new().no_mount().readonly()
    ///             .search_source(source.clone()),
    ///     ).await?;
    ///     pool.export(&Default::default()).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn import_pool(
        &self,
        discovered: &DiscoveredPool,
        opts: &ImportOptions,
    ) -> Result<Pool, ZfsError> {
        let pool = self.pool(discovered.name.clone())?;
        crate::pool::import_by_guid(&*self.runner, discovered.guid()?, opts).await?;
        Ok(pool)
    }

    /// Create a new pool. Returns a [`Pool`] handle to the just-created pool.
    pub async fn create_pool(&self, opts: &PoolCreateOptions) -> Result<Pool, ZfsError> {
        crate::pool::create(&*self.runner, opts).await?;
        let name = PoolName::parse(opts.name.clone())?;
        Ok(Pool {
            runner: self.runner.clone(),
            name,
        })
    }

    /// Create a dataset by absolute name (e.g., `"tank/data/home"`). Convenience
    /// for callers that have full paths and don't want to navigate via a
    /// `Pool` or `Dataset` handle. Equivalent to looking up the parent and
    /// calling `pool.create_dataset(rel)` / `dataset.create_dataset(rel)`,
    /// but lets the caller skip the split.
    pub async fn create_dataset(
        &self,
        abs_name: impl Into<String>,
        opts: &CreateOptions,
    ) -> Result<Dataset, ZfsError> {
        let name = DatasetName::parse(abs_name)?;
        crate::dataset::create(&*self.runner, name.as_str(), opts).await?;
        Ok(Dataset {
            runner: self.runner.clone(),
            name,
        })
    }

    /// `zfs mount -a` — mount all importable filesystems on the system.
    pub async fn mount_all(&self) -> Result<(), ZfsError> {
        crate::dataset::mount_all(&*self.runner).await
    }

    /// `zfs umount -a [-f]` — unmount all mounted filesystems on the system.
    pub async fn unmount_all(&self, force: bool) -> Result<(), ZfsError> {
        crate::dataset::unmount_all(&*self.runner, force).await
    }

    /// `zfs destroy <bookmark>` — destroy a bookmark by its full name
    /// (e.g., `"tank/data#bm1"`).
    pub async fn destroy_bookmark(&self, bookmark_name: &str) -> Result<(), ZfsError> {
        let bookmark = BookmarkName::parse(bookmark_name)?;
        crate::bookmark::destroy(&*self.runner, bookmark.as_str()).await
    }

    pub async fn list_holds_many(
        &self,
        snapshots: &[SnapshotName],
    ) -> Result<Vec<crate::hold::Hold>, ZfsError> {
        let names: Vec<&str> = snapshots.iter().map(SnapshotName::as_str).collect();
        crate::hold::list_holds_many(&*self.runner, &names).await
    }

    pub async fn send(
        &self,
        args: &crate::send::SendArgs,
    ) -> Result<crate::send::SendProcess, ZfsError> {
        crate::send::send(&*self.runner, args).await
    }

    pub async fn receive(
        &self,
        args: &crate::recv::RecvArgs,
    ) -> Result<crate::recv::RecvProcess, crate::recv::RecvError> {
        crate::recv::recv(&*self.runner, args).await
    }

    pub async fn send_dry_run(
        &self,
        args: &crate::send::SendArgs,
    ) -> Result<crate::send::DryRunSize, ZfsError> {
        crate::send::dry_run(&*self.runner, args).await
    }

    pub async fn decode_resume_token(
        &self,
        token: &str,
    ) -> Result<crate::resume_token::ResumeToken, crate::resume_token::ResumeTokenError> {
        crate::resume_token::decode(&*self.runner, token).await
    }

    pub async fn abort_partial_receive(&self, dataset: &DatasetName) -> Result<(), ZfsError> {
        crate::recv::abort_partial(&*self.runner, dataset.as_str()).await
    }
}

impl Default for Zfs {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle bound to a specific zpool.
#[derive(Clone)]
pub struct Pool {
    runner: Arc<dyn CommandRunner>,
    name: PoolName,
}

impl Pool {
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub async fn import(&self, opts: &ImportOptions) -> Result<(), ZfsError> {
        crate::pool::import(&*self.runner, self.name.as_str(), opts).await
    }

    pub async fn export(&self, opts: &ExportOptions) -> Result<(), ZfsError> {
        crate::pool::export(&*self.runner, self.name.as_str(), opts).await
    }

    /// `zpool destroy [-f] <pool>`. Permanent.
    pub async fn destroy(&self, opts: &DestroyOptions) -> Result<(), ZfsError> {
        crate::pool::destroy(&*self.runner, self.name.as_str(), opts).await
    }

    /// Read multiple properties via `zpool get -j`. The `pools` field of
    /// `opts` is overridden with this handle's name.
    pub async fn get(&self, opts: &PoolGetOptions) -> Result<Vec<ZpoolGetEntry>, ZfsError> {
        let mut opts = opts.clone();
        opts.pools = vec![self.name.to_string()];
        crate::pool::get(&*self.runner, &opts).await
    }

    /// Read a single pool property.
    pub async fn get_property(&self, property: &str) -> Result<PropertyValue, ZfsError> {
        crate::pool::get_property(&*self.runner, self.name.as_str(), property).await
    }

    /// `zpool set <name>=<value> <pool>`.
    pub async fn set_property(&self, property: &str, value: &str) -> Result<(), ZfsError> {
        crate::pool::set_property(&*self.runner, self.name.as_str(), property, value).await
    }

    /// `zpool status -j <pool>`.
    pub async fn status(&self) -> Result<ZpoolStatusEntry, ZfsError> {
        crate::pool::status(&*self.runner, self.name.as_str()).await
    }

    /// Returns whether the pool is imported and visible, preserving failures.
    pub async fn exists(&self) -> Result<bool, ZfsError> {
        let opts = PoolListOptions {
            pools: vec![self.name.to_string()],
            ..Default::default()
        };
        match crate::pool::list(&*self.runner, &opts).await {
            Ok(entries) => Ok(!entries.is_empty()),
            Err(ZfsError::PoolNotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn exists_best_effort(&self) -> bool {
        self.exists().await.unwrap_or(false)
    }

    pub async fn scrub(&self, action: crate::pool::ScrubAction) -> Result<(), ZfsError> {
        crate::pool::scrub(&*self.runner, self.name.as_str(), action).await
    }

    /// Handle to the pool's root filesystem (the zfs dataset whose name
    /// matches the pool name). Encryption properties of a pool actually
    /// live on this dataset.
    pub fn root_dataset(&self) -> Dataset {
        Dataset {
            runner: self.runner.clone(),
            name: DatasetName::parse(self.name.to_string()).expect("pool name is a dataset name"),
        }
    }

    /// Lookup a nested dataset by relative name. The full ZFS name is
    /// `<pool>/<rel_name>`.
    pub fn dataset(&self, rel_name: &str) -> Result<Dataset, NameError> {
        Ok(Dataset {
            runner: self.runner.clone(),
            name: DatasetName::parse(format!("{}/{rel_name}", self.name))?,
        })
    }

    /// Create a child dataset within this pool. The full ZFS name is
    /// `<pool>/<rel_name>`. Returns a `Dataset` handle to the just-created
    /// dataset on success.
    pub async fn create_dataset(
        &self,
        rel_name: &str,
        opts: &CreateOptions,
    ) -> Result<Dataset, ZfsError> {
        let full_name = DatasetName::parse(format!("{}/{rel_name}", self.name))?;
        crate::dataset::create(&*self.runner, full_name.as_str(), opts).await?;
        Ok(Dataset {
            runner: self.runner.clone(),
            name: full_name,
        })
    }
}

/// Handle bound to a specific dataset.
#[derive(Clone)]
pub struct Dataset {
    runner: Arc<dyn CommandRunner>,
    name: DatasetName,
}

impl Dataset {
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Rename this dataset and return its new handle. Consumes the old handle
    /// even on failure; existing cloned handles and descendant/snapshot handles
    /// are not updated and must be reacquired after success.
    pub async fn rename(
        self,
        destination: impl Into<String>,
        opts: &crate::dataset::RenameOptions,
    ) -> Result<Self, ZfsError> {
        let name = DatasetName::parse(destination)?;
        crate::dataset::rename(&*self.runner, self.name.as_str(), name.as_str(), opts).await?;
        Ok(Self {
            runner: self.runner,
            name,
        })
    }

    /// Reverse a clone's origin dependency via `zfs promote`.
    pub async fn promote(&self) -> Result<(), ZfsError> {
        crate::dataset::promote(&*self.runner, self.name.as_str()).await
    }

    /// Read multiple properties via `zfs get -j`. The `datasets` field of
    /// `opts` is overridden with this handle's name.
    pub async fn get(&self, opts: &GetOptions) -> Result<Vec<ZfsGetEntry>, ZfsError> {
        let mut opts = opts.clone();
        opts.datasets = vec![self.name.to_string()];
        crate::dataset::get(&*self.runner, &opts).await
    }

    /// Read a single property.
    pub async fn get_property(&self, property: &str) -> Result<PropertyValue, ZfsError> {
        crate::dataset::get_property(&*self.runner, self.name.as_str(), property).await
    }

    /// `zfs set <property>=<value> <dataset>`.
    pub async fn set_property(&self, property: &str, value: &str) -> Result<(), ZfsError> {
        crate::dataset::set_property(&*self.runner, self.name.as_str(), property, value).await
    }

    /// `zfs set [-u] <property>=<value>… <dataset>` — several properties in one
    /// invocation, with modifiers.
    ///
    /// Use `SetOptions::no_mount()` when changing `mountpoint` on a dataset that
    /// must not be mounted right now, such as a boot environment being prepared
    /// from the running system.
    pub async fn set_properties(
        &self,
        properties: &[(&str, &str)],
        opts: &SetOptions,
    ) -> Result<(), ZfsError> {
        crate::dataset::set_properties(&*self.runner, self.name.as_str(), properties, opts).await
    }

    /// `zfs mount [-R] <dataset>`. Idempotent on already-mounted.
    pub async fn mount(&self, opts: &MountOptions) -> Result<(), ZfsError> {
        crate::dataset::mount(&*self.runner, self.name.as_str(), opts).await
    }

    /// `zfs umount [-f] <dataset>`. Idempotent on not-currently-mounted.
    pub async fn unmount(&self, opts: &UnmountOptions) -> Result<(), ZfsError> {
        crate::dataset::unmount(&*self.runner, self.name.as_str(), opts).await
    }

    /// Returns whether this dataset exists, preserving transport and parse errors.
    pub async fn exists(&self) -> Result<bool, ZfsError> {
        let opts = ListOptions {
            roots: vec![self.name.to_string()],
            ..Default::default()
        };
        match crate::dataset::list(&*self.runner, &opts).await {
            Ok(entries) => Ok(!entries.is_empty()),
            Err(ZfsError::DatasetNotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Best-effort existence probe that deliberately collapses errors to false.
    pub async fn exists_best_effort(&self) -> bool {
        self.exists().await.unwrap_or(false)
    }

    /// `zfs load-key <dataset>`. Reads the key from the dataset's
    /// `keylocation` (file://, prompt, ...). Idempotent on already-loaded.
    pub async fn load_key(&self) -> Result<(), ZfsError> {
        crate::encryption::load_key(&*self.runner, self.name.as_str()).await
    }

    /// `zfs load-key <dataset>` with the passphrase delivered via stdin —
    /// no terminal prompt, no temp keyfile. Idempotent on already-loaded.
    pub async fn load_key_with_passphrase(&self, passphrase: &[u8]) -> Result<(), ZfsError> {
        crate::encryption::load_key_with_passphrase(&*self.runner, self.name.as_str(), passphrase)
            .await
    }

    /// Verify a prompt passphrase without loading or unloading the key.
    pub async fn verify_passphrase(&self, passphrase: &[u8]) -> Result<bool, ZfsError> {
        crate::encryption::verify_passphrase(&*self.runner, self.name.as_str(), passphrase).await
    }

    /// `zfs load-key -L <keylocation> <dataset>` — load with an explicit
    /// keylocation override (e.g., `file:///path` when the key file lives at
    /// a known location but the dataset's `keylocation` property is `prompt`).
    /// Idempotent on already-loaded.
    pub async fn load_key_with_keylocation(&self, keylocation: &str) -> Result<(), ZfsError> {
        crate::encryption::load_key_with_keylocation(&*self.runner, self.name.as_str(), keylocation)
            .await
    }

    /// `zfs unload-key <dataset>`. Idempotent on already-unloaded.
    pub async fn unload_key(&self) -> Result<(), ZfsError> {
        crate::encryption::unload_key(&*self.runner, self.name.as_str()).await
    }

    /// Lookup a nested dataset by relative name. The full ZFS name is
    /// `<self>/<rel_name>`.
    pub fn dataset(&self, rel_name: &str) -> Result<Dataset, NameError> {
        Ok(Dataset {
            runner: self.runner.clone(),
            name: DatasetName::parse(format!("{}/{rel_name}", self.name))?,
        })
    }

    /// Create a child dataset within this dataset. The full ZFS name is
    /// `<self>/<rel_name>`. Returns a `Dataset` handle to the just-created
    /// dataset on success.
    pub async fn create_dataset(
        &self,
        rel_name: &str,
        opts: &CreateOptions,
    ) -> Result<Dataset, ZfsError> {
        let full_name = DatasetName::parse(format!("{}/{rel_name}", self.name))?;
        crate::dataset::create(&*self.runner, full_name.as_str(), opts).await?;
        Ok(Dataset {
            runner: self.runner.clone(),
            name: full_name,
        })
    }

    /// `zfs snapshot [-r] <self>@<tag>`.
    pub async fn snapshot(
        &self,
        tag: &str,
        opts: &crate::dataset::SnapshotOptions,
    ) -> Result<Snapshot, ZfsError> {
        let name = SnapshotName::new(self.name.clone(), tag)?;
        crate::dataset::snapshot(&*self.runner, name.as_str(), opts).await?;
        Ok(Snapshot {
            runner: self.runner.clone(),
            name,
        })
    }

    /// Construct a typed handle for an existing snapshot.
    pub fn snapshot_handle(&self, tag: &str) -> Result<Snapshot, NameError> {
        Ok(Snapshot {
            runner: self.runner.clone(),
            name: SnapshotName::new(self.name.clone(), tag)?,
        })
    }

    /// `zfs destroy [flags] <self>` — destroy this dataset. Snapshots and
    /// bookmarks are destroyed through their own typed handles.
    pub async fn destroy(&self, opts: &crate::dataset::DestroyOptions) -> Result<(), ZfsError> {
        crate::dataset::destroy(&*self.runner, self.name.as_str(), opts).await
    }

    /// Probe whether this dataset has a partial-receive in flight. `None`
    /// means no pending receive; `Some(token)` means a previous recv was
    /// interrupted and can be resumed by feeding `token` into
    /// [`crate::send::SendArgs::resume`].
    pub async fn receive_resume_token(&self) -> Result<Option<String>, ZfsError> {
        crate::recv::receive_resume_token(&*self.runner, self.name.as_str()).await
    }
}

/// Handle bound to a snapshot. Only snapshot-valid operations are exposed.
#[derive(Clone)]
pub struct Snapshot {
    runner: Arc<dyn CommandRunner>,
    name: SnapshotName,
}

impl Snapshot {
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Clone this snapshot to a validated absolute dataset name. With `-p`,
    /// ZFS also succeeds when the destination already exists; the returned
    /// handle then refers to that existing dataset without verifying its origin.
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), zfskit::ZfsError> {
    /// use zfskit::dataset::{CloneOptions, RenameOptions};
    /// let zfs = zfskit::Zfs::new();
    /// let snapshot = zfs.snapshot("tank/data@before")?;
    /// let clone = snapshot.clone_as(
    ///     "tank/copies/test",
    ///     &CloneOptions::new().parents().property("mountpoint", "none"),
    /// ).await?;
    /// let clone = clone.rename("tank/copies/current", &RenameOptions::new()).await?;
    /// clone.promote().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn clone_as(
        &self,
        destination: impl Into<String>,
        opts: &crate::dataset::CloneOptions,
    ) -> Result<Dataset, ZfsError> {
        let name = DatasetName::parse(destination)?;
        crate::dataset::clone(&*self.runner, self.name.as_str(), name.as_str(), opts).await?;
        Ok(Dataset {
            runner: self.runner.clone(),
            name,
        })
    }

    /// Rename to a new snapshot tag within this dataset, consuming the old
    /// handle even on failure. Recursive rename can invalidate other handles;
    /// no previously cloned handles are updated automatically.
    pub async fn rename(
        self,
        tag: &str,
        opts: &crate::dataset::SnapshotRenameOptions,
    ) -> Result<Self, ZfsError> {
        let name = SnapshotName::new(self.name.dataset().clone(), tag)?;
        crate::dataset::rename_snapshot(&*self.runner, self.name.as_str(), name.as_str(), opts)
            .await?;
        Ok(Self {
            runner: self.runner,
            name,
        })
    }

    pub async fn exists(&self) -> Result<bool, ZfsError> {
        let opts = ListOptions {
            roots: vec![self.name.to_string()],
            types: vec![crate::models::DatasetType::Snapshot],
            ..Default::default()
        };
        match crate::dataset::list(&*self.runner, &opts).await {
            Ok(entries) => Ok(!entries.is_empty()),
            Err(ZfsError::DatasetNotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn hold(&self, tag: &str) -> Result<(), ZfsError> {
        crate::hold::hold(&*self.runner, self.name.as_str(), tag).await
    }

    pub async fn release(&self, tag: &str) -> Result<(), ZfsError> {
        crate::hold::release(&*self.runner, self.name.as_str(), tag).await
    }

    pub async fn list_holds(&self) -> Result<Vec<crate::hold::Hold>, ZfsError> {
        crate::hold::list_holds(&*self.runner, self.name.as_str()).await
    }

    pub async fn rollback(&self, opts: &crate::dataset::RollbackOptions) -> Result<(), ZfsError> {
        crate::dataset::rollback(&*self.runner, self.name.as_str(), opts).await
    }

    pub async fn destroy(&self, opts: &crate::dataset::DestroyOptions) -> Result<(), ZfsError> {
        crate::dataset::destroy(&*self.runner, self.name.as_str(), opts).await
    }

    pub async fn bookmark(&self, mark: &str) -> Result<Bookmark, ZfsError> {
        let name = BookmarkName::new(self.name.dataset().clone(), mark)?;
        crate::bookmark::create(&*self.runner, self.name.as_str(), name.as_str()).await?;
        Ok(Bookmark {
            runner: self.runner.clone(),
            name,
        })
    }
}

/// Handle bound to a bookmark.
#[derive(Clone)]
pub struct Bookmark {
    runner: Arc<dyn CommandRunner>,
    name: BookmarkName,
}

impl Bookmark {
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub async fn destroy(&self) -> Result<(), ZfsError> {
        crate::bookmark::destroy(&*self.runner, self.name.as_str()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Cmd, RecordingRunner};

    fn get_property_json(ds: &str, value: &str, source_kind: &str) -> Vec<u8> {
        format!(
            "{{\"output_version\":{{\"command\":\"zfs get\",\"vers_major\":0,\"vers_minor\":1}},\
             \"datasets\":{{\"{ds}\":{{\"name\":\"{ds}\",\"type\":\"FILESYSTEM\",\
             \"pool\":\"{ds}\",\"createtxg\":\"1\",\"properties\":{{\"encryption\":\
             {{\"value\":\"{value}\",\"source\":{{\"type\":\"{source_kind}\",\"data\":\"-\"}}}}}}}}}}}}"
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn dataset_get_property_via_handle() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["get", "-j", "-p", "encryption", "tank"]),
            get_property_json("tank", "off", "DEFAULT"),
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        let prop = zfs
            .dataset("tank")
            .unwrap()
            .get_property("encryption")
            .await
            .unwrap();
        assert_eq!(prop.value, "off");
    }

    #[tokio::test]
    async fn pool_import_export_via_handle() {
        let runner = RecordingRunner::new()
            .record(
                Cmd::new("zpool").args(["import", "-f", "-N", "tank"]),
                vec![],
                vec![],
                0,
            )
            .record(
                Cmd::new("zpool").args(["export", "tank"]),
                vec![],
                vec![],
                0,
            );
        let zfs = Zfs::with_runner(runner);
        let pool = zfs.pool("tank").unwrap();
        let opts = ImportOptions {
            force: true,
            no_mount: true,
            ..Default::default()
        };
        pool.import(&opts).await.expect("import succeeds");
        pool.export(&ExportOptions::default())
            .await
            .expect("export succeeds");
    }

    #[tokio::test]
    async fn pool_create_dataset_returns_handle_with_full_name() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["create", "-o", "compression=lz4", "tank/data"]),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        let pool = zfs.pool("tank").unwrap();
        let ds = pool
            .create_dataset("data", &CreateOptions::new().property("compression", "lz4"))
            .await
            .expect("create_dataset succeeds");
        assert_eq!(ds.name(), "tank/data");
    }

    #[tokio::test]
    async fn zfs_create_dataset_takes_absolute_name() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["create", "-o", "compression=lz4", "tank/data/home"]),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        let ds = zfs
            .create_dataset(
                "tank/data/home",
                &CreateOptions::new().property("compression", "lz4"),
            )
            .await
            .expect("create_dataset by abs name succeeds");
        assert_eq!(ds.name(), "tank/data/home");
    }

    #[tokio::test]
    async fn nested_create_dataset_joins_path() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["create", "tank/data/home"]),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        let parent = zfs.dataset("tank/data").unwrap();
        let child = parent
            .create_dataset("home", &CreateOptions::new())
            .await
            .expect("nested create_dataset succeeds");
        assert_eq!(child.name(), "tank/data/home");
    }

    #[tokio::test]
    async fn pool_root_dataset_uses_pool_name() {
        let zfs = Zfs::with_runner(RecordingRunner::new());
        let root = zfs.pool("tank").unwrap().root_dataset();
        assert_eq!(root.name(), "tank");
    }

    #[tokio::test]
    async fn pool_dataset_lookup_joins_path() {
        let zfs = Zfs::with_runner(RecordingRunner::new());
        let ds = zfs.pool("tank").unwrap().dataset("data/home").unwrap();
        assert_eq!(ds.name(), "tank/data/home");
    }

    #[tokio::test]
    async fn dataset_set_property_via_handle() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["set", "compression=zstd", "tank/data"]),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        zfs.dataset("tank/data")
            .unwrap()
            .set_property("compression", "zstd")
            .await
            .expect("set_property succeeds");
    }

    #[tokio::test]
    async fn dataset_set_properties_no_mount_via_handle() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["set", "-u", "mountpoint=/home", "tank/be0/data/home"]),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        zfs.dataset("tank/be0/data/home")
            .unwrap()
            .set_properties(&[("mountpoint", "/home")], &SetOptions::new().no_mount())
            .await
            .expect("set_properties with -u succeeds");
    }

    #[tokio::test]
    async fn dataset_set_properties_empty_is_noop() {
        // No command is recorded, so a runner call would panic on unknown input.
        let zfs = Zfs::with_runner(RecordingRunner::new());
        zfs.dataset("tank/data")
            .unwrap()
            .set_properties(&[], &SetOptions::default())
            .await
            .expect("empty property list is a no-op");
    }

    #[tokio::test]
    async fn dataset_mount_unmount_via_handle() {
        let runner = RecordingRunner::new()
            .record(
                Cmd::new("zfs").args(["mount", "tank/data"]),
                vec![],
                vec![],
                0,
            )
            .record(
                Cmd::new("zfs").args(["umount", "-f", "tank/data"]),
                vec![],
                vec![],
                0,
            );
        let zfs = Zfs::with_runner(runner);
        let ds = zfs.dataset("tank/data").unwrap();
        ds.mount(&MountOptions::default())
            .await
            .expect("mount succeeds");
        ds.unmount(&UnmountOptions { force: true })
            .await
            .expect("unmount succeeds");
    }

    #[tokio::test]
    async fn dataset_snapshot_rollback_destroy_via_handle() {
        let runner = RecordingRunner::new()
            .record(
                Cmd::new("zfs").args(["snapshot", "tank/data@snap1"]),
                vec![],
                vec![],
                0,
            )
            .record(
                Cmd::new("zfs").args(["rollback", "-r", "tank/data@snap1"]),
                vec![],
                vec![],
                0,
            )
            .record(
                Cmd::new("zfs").args(["destroy", "tank/data@snap1"]),
                vec![],
                vec![],
                0,
            )
            .record(
                Cmd::new("zfs").args(["destroy", "-r", "-f", "tank/data"]),
                vec![],
                vec![],
                0,
            );
        let zfs = Zfs::with_runner(runner);
        let ds = zfs.dataset("tank/data").unwrap();
        let snap = ds
            .snapshot("snap1", &crate::dataset::SnapshotOptions::new())
            .await
            .expect("snapshot succeeds");
        assert_eq!(snap.name(), "tank/data@snap1");
        snap.rollback(&crate::dataset::RollbackOptions::new().destroy_newer())
            .await
            .expect("rollback succeeds");
        snap.destroy(&crate::dataset::DestroyOptions::new())
            .await
            .expect("destroy_snapshot succeeds");
        ds.destroy(
            &crate::dataset::DestroyOptions::new()
                .recursive()
                .force_unmount(),
        )
        .await
        .expect("destroy succeeds");
    }

    #[tokio::test]
    async fn zfs_unmount_all_force() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs").args(["umount", "-a", "-f"]),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        zfs.unmount_all(true)
            .await
            .expect("unmount_all force succeeds");
    }

    #[tokio::test]
    async fn dataset_load_key_with_passphrase_via_handle() {
        let runner = RecordingRunner::new().record(
            Cmd::new("zfs")
                .args(["load-key", "-L", "prompt", "tank/encrypted"])
                .stdin_secret(b"correct".to_vec()),
            vec![],
            vec![],
            0,
        );
        let zfs = Zfs::with_runner(runner);
        zfs.dataset("tank/encrypted")
            .unwrap()
            .load_key_with_passphrase(b"correct")
            .await
            .expect("load_key_with_passphrase succeeds");
    }
}
