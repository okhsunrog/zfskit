//! Generic dataset lifecycle and pool discovery/import on the existing SSH VM.
#![cfg(feature = "integration")]

mod common;

use common::{LoopbackPool, ssh_runner_from_env};
use zfskit::dataset::{CloneOptions, RenameOptions, SnapshotOptions, SnapshotRenameOptions};
use zfskit::pool::{DiscoverOptions, ExportOptions, ImportOptions, PoolSearchSource};
use zfskit::{Cmd, CommandRunner, ZfsError};

#[tokio::test]
async fn clone_rename_promote_export_discover_guid_import_roundtrip() {
    let pool = LoopbackPool::create(ssh_runner_from_env()).await.unwrap();
    let zfs = pool.zfs();
    let source = zfs
        .dataset(pool.name())
        .unwrap()
        .create_dataset("source", &Default::default())
        .await
        .unwrap();
    let snapshot = source
        .snapshot("before", &Default::default())
        .await
        .unwrap();
    let clone_name = format!("{}/parents/copy", pool.name());
    let clone_opts = CloneOptions::new()
        .parents()
        .property("mountpoint", "none")
        .property("compression", "zstd");
    let clone = snapshot.clone_as(&clone_name, &clone_opts).await.unwrap();
    assert_eq!(
        clone.get_property("origin").await.unwrap().value,
        snapshot.name()
    );
    assert_eq!(
        clone.get_property("compression").await.unwrap().value,
        "zstd"
    );
    // -p is idempotent on an existing destination and does not apply properties.
    snapshot
        .clone_as(
            &clone_name,
            &CloneOptions::new().parents().property("compression", "off"),
        )
        .await
        .unwrap();
    assert_eq!(
        clone.get_property("compression").await.unwrap().value,
        "zstd"
    );
    let moved_name = format!("{}/newparents/moved", pool.name());
    let clone = clone
        .rename(&moved_name, &RenameOptions::new().parents())
        .await
        .unwrap();
    assert_eq!(clone.name(), moved_name);
    assert!(!zfs.dataset(clone_name).unwrap().exists().await.unwrap());
    let final_name = format!("{}/newparents/final", pool.name());
    let clone = clone
        .rename(&final_name, &RenameOptions::new().no_remount())
        .await
        .unwrap();
    clone.promote().await.unwrap();
    assert_eq!(clone.get_property("origin").await.unwrap().value, "-");
    assert_eq!(
        source.get_property("origin").await.unwrap().value,
        format!("{final_name}@before")
    );

    let handle = zfs.pool(pool.name()).unwrap();
    let guid: u64 = handle
        .get_property("guid")
        .await
        .unwrap()
        .value
        .parse()
        .unwrap();
    let source = PoolSearchSource::Directories(vec![format!("/tmp/{}.img", pool.name()).into()]);
    handle.export(&ExportOptions::default()).await.unwrap();
    let discovered = zfs
        .discover_importable_pools_with(&DiscoverOptions::new().search_source(source.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == pool.name())
        .unwrap();
    assert_eq!(discovered.guid().unwrap(), guid);
    let import_opts = ImportOptions::new()
        .no_mount()
        .altroot(pool.altroot())
        .property("cachefile", "none")
        .search_source(source);
    let imported = zfs.import_pool(&discovered, &import_opts).await.unwrap();
    assert_eq!(imported.name(), pool.name());
    assert_eq!(clone.get_property("mounted").await.unwrap().value, "no");
    assert_eq!(
        imported.get_property("cachefile").await.unwrap().value,
        "none"
    );
    imported.export(&ExportOptions::default()).await.unwrap();
    let imported = zfs
        .import_pool(&discovered, &import_opts.readonly())
        .await
        .unwrap();
    assert_eq!(imported.get_property("readonly").await.unwrap().value, "on");
    assert_eq!(clone.get_property("mounted").await.unwrap().value, "no");
    imported.export(&ExportOptions::default()).await.unwrap();
    pool.destroy().await.unwrap();
}

#[tokio::test]
async fn cachefile_discovery_and_guid_import() {
    let runner = ssh_runner_from_env();
    let pool = LoopbackPool::create(runner.clone()).await.unwrap();
    let zfs = pool.zfs();
    let handle = zfs.pool(pool.name()).unwrap();
    let cache = format!("{}/zpool.cache", pool.altroot());
    let saved = format!("{}/saved.cache", pool.altroot());
    handle.set_property("cachefile", &cache).await.unwrap();
    // Export updates the active cache, so save the configuration first.
    let out = runner
        .run(Cmd::new("cp").args([&cache, &saved]))
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    handle.export(&ExportOptions::default()).await.unwrap();
    let source = PoolSearchSource::CacheFile(saved.into());
    let discovered = zfs
        .discover_importable_pools_with(&DiscoverOptions::new().search_source(source.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == pool.name())
        .unwrap();
    let imported = zfs
        .import_pool(
            &discovered,
            &ImportOptions::new()
                .no_mount()
                .altroot(pool.altroot())
                .search_source(source),
        )
        .await
        .unwrap();
    assert_eq!(imported.name(), pool.name());
    imported.export(&ExportOptions::default()).await.unwrap();
    pool.destroy().await.unwrap();
}

#[tokio::test]
async fn snapshot_rename_and_recursive_rename() {
    let pool = LoopbackPool::create(ssh_runner_from_env()).await.unwrap();
    let zfs = pool.zfs();
    let parent = zfs
        .dataset(pool.name())
        .unwrap()
        .create_dataset("parent", &Default::default())
        .await
        .unwrap();
    let child = parent
        .create_dataset("child", &Default::default())
        .await
        .unwrap();
    let snap = parent
        .snapshot("original", &SnapshotOptions::new().recursive())
        .await
        .unwrap();
    let snap = snap
        .rename("renamed", &SnapshotRenameOptions::new().recursive())
        .await
        .unwrap();
    assert!(snap.exists().await.unwrap());
    assert!(
        child
            .snapshot_handle("renamed")
            .unwrap()
            .exists()
            .await
            .unwrap()
    );
    assert!(
        !child
            .snapshot_handle("original")
            .unwrap()
            .exists()
            .await
            .unwrap()
    );
    let snap = snap.rename("single", &Default::default()).await.unwrap();
    assert!(snap.exists().await.unwrap());
    assert!(
        child
            .snapshot_handle("renamed")
            .unwrap()
            .exists()
            .await
            .unwrap()
    );
    pool.destroy().await.unwrap();
}

#[tokio::test]
async fn lifecycle_errors_match_captured_openzfs_diagnostics() {
    let pool = LoopbackPool::create(ssh_runner_from_env()).await.unwrap();
    let zfs = pool.zfs();
    let source = zfs
        .dataset(pool.name())
        .unwrap()
        .create_dataset("source", &Default::default())
        .await
        .unwrap();
    let snap = source.snapshot("s", &Default::default()).await.unwrap();
    let clone_name = format!("{}/clone", pool.name());
    let clone = snap
        .clone_as(&clone_name, &Default::default())
        .await
        .unwrap();
    assert!(
        matches!(snap.clone_as(&clone_name, &Default::default()).await, Err(ZfsError::DatasetExists { name }) if name == clone_name)
    );
    assert!(matches!(
        clone.rename(source.name(), &Default::default()).await,
        Err(ZfsError::DatasetExists { .. })
    ));
    assert!(matches!(
        source.promote().await,
        Err(ZfsError::NotClone { .. })
    ));
    let missing = source.snapshot_handle("missing").unwrap();
    assert!(matches!(
        missing
            .clone_as(format!("{}/other", pool.name()), &Default::default())
            .await,
        Err(ZfsError::DatasetNotFound { .. })
    ));
    pool.destroy().await.unwrap();
}

#[tokio::test]
async fn import_loads_file_keys_only_when_mounting() {
    let runner = ssh_runner_from_env();
    let pool = LoopbackPool::create(runner.clone()).await.unwrap();
    let zfs = pool.zfs();
    let keyfile = format!("{}/key.raw", pool.altroot());
    let out = runner
        .run(Cmd::new("dd").args([
            "if=/dev/urandom",
            &format!("of={keyfile}"),
            "bs=32",
            "count=1",
            "status=none",
        ]))
        .await
        .unwrap();
    assert!(out.status.success());
    let encrypted = zfs
        .dataset(pool.name())
        .unwrap()
        .create_dataset(
            "encrypted",
            &zfskit::dataset::CreateOptions::new()
                .property("encryption", "aes-256-gcm")
                .property("keyformat", "raw")
                .property("keylocation", format!("file://{keyfile}"))
                .property("mountpoint", "/encrypted"),
        )
        .await
        .unwrap();
    let pool_handle = zfs.pool(pool.name()).unwrap();
    pool_handle.export(&ExportOptions::default()).await.unwrap();
    let source = PoolSearchSource::Directories(vec![format!("/tmp/{}.img", pool.name()).into()]);
    let discovered = zfs
        .discover_importable_pools_with(&DiscoverOptions::new().search_source(source.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == pool.name())
        .unwrap();
    let opts = ImportOptions::new()
        .altroot(pool.altroot())
        .search_source(source);
    let imported = zfs
        .import_pool(&discovered, &opts.clone().no_mount())
        .await
        .unwrap();
    assert_eq!(
        encrypted.get_property("keystatus").await.unwrap().value,
        "unavailable"
    );
    assert_eq!(encrypted.get_property("mounted").await.unwrap().value, "no");
    imported.export(&ExportOptions::default()).await.unwrap();
    let imported = zfs
        .import_pool(&discovered, &opts.load_keys())
        .await
        .unwrap();
    assert_eq!(
        encrypted.get_property("keystatus").await.unwrap().value,
        "available"
    );
    assert_eq!(
        encrypted.get_property("mounted").await.unwrap().value,
        "yes"
    );
    imported.export(&ExportOptions::default()).await.unwrap();
    pool.destroy().await.unwrap();
}
