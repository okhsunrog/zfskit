use zfskit::dataset::{
    CloneOptions, RenameOptions, SnapshotRenameOptions, clone, promote, rename, rename_snapshot,
};
use zfskit::{Cmd, RecordingRunner, Zfs, ZfsError, classify_stderr};

fn success(args: &[&str]) -> RecordingRunner {
    RecordingRunner::new().record(
        Cmd::new("zfs").args(args.iter().copied()),
        vec![],
        vec![],
        0,
    )
}

#[tokio::test]
async fn basic_clone() {
    clone(
        &success(&["clone", "tank/data@s", "tank/copy"]),
        "tank/data@s",
        "tank/copy",
        &Default::default(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn clone_parents_and_properties() {
    let opts = CloneOptions::new()
        .parents()
        .property("mountpoint", "none")
        .property("compression", "lz4");
    let args = [
        "clone",
        "-p",
        "-o",
        "mountpoint=none",
        "-o",
        "compression=lz4",
        "tank/data@s",
        "tank/a/copy",
    ];
    assert_eq!(opts.build_args("tank/data@s", "tank/a/copy"), args);
    let zfs = Zfs::with_runner(success(&args));
    let copy = zfs
        .snapshot("tank/data@s")
        .unwrap()
        .clone_as("tank/a/copy", &opts)
        .await
        .unwrap();
    assert_eq!(copy.name(), "tank/a/copy");
}

#[tokio::test]
async fn clone_rejects_invalid_names_before_execution() {
    let runner = RecordingRunner::new();
    for (source, target) in [
        ("tank/data@s", "tank//copy"),
        ("tank/data@s", "tank/copy@s"),
        ("tank/data", "tank/copy"),
    ] {
        assert!(matches!(
            clone(&runner, source, target, &Default::default()).await,
            Err(ZfsError::InvalidName(_))
        ));
    }
    let zfs = Zfs::with_runner(runner);
    assert!(matches!(
        zfs.snapshot("tank/data@s")
            .unwrap()
            .clone_as("tank/invalid?", &Default::default())
            .await,
        Err(ZfsError::InvalidName(_))
    ));
}

#[tokio::test]
async fn clone_classifies_captured_failures() {
    let runner = RecordingRunner::new()
        .record(
            Cmd::new("zfs").args([
                "clone",
                "zfskit_test_diagnostics/data@snap",
                "zfskit_test_diagnostics/clone",
            ]),
            vec![],
            include_bytes!("fixtures/stderr/clone_exists.txt").to_vec(),
            1,
        )
        .record(
            Cmd::new("zfs").args([
                "clone",
                "zfskit_test_diagnostics/data@missing",
                "zfskit_test_diagnostics/other",
            ]),
            vec![],
            include_bytes!("fixtures/stderr/clone_missing_snapshot.txt").to_vec(),
            1,
        );
    assert!(
        matches!(clone(&runner, "zfskit_test_diagnostics/data@snap", "zfskit_test_diagnostics/clone", &Default::default()).await, Err(ZfsError::DatasetExists { name }) if name == "zfskit_test_diagnostics/clone")
    );
    assert!(matches!(
        clone(
            &runner,
            "zfskit_test_diagnostics/data@missing",
            "zfskit_test_diagnostics/other",
            &Default::default()
        )
        .await,
        Err(ZfsError::DatasetNotFound { .. })
    ));
    assert!(matches!(
        classify_stderr(
            include_str!("fixtures/stderr/clone_not_snapshot.txt"),
            Some(1)
        ),
        ZfsError::Other { .. }
    ));
}

#[tokio::test]
async fn basic_rename_returns_new_handle() {
    let zfs = Zfs::with_runner(success(&["rename", "tank/old", "tank/new"]));
    let renamed = zfs
        .dataset("tank/old")
        .unwrap()
        .rename("tank/new", &Default::default())
        .await
        .unwrap();
    assert_eq!(renamed.name(), "tank/new");
}

#[tokio::test]
async fn rename_supported_options() {
    for (opts, args) in [
        (
            RenameOptions::new().parents().force_unmount(),
            vec!["rename", "-p", "-f", "tank/old", "tank/a/new"],
        ),
        (
            RenameOptions::new().no_remount().force_unmount(),
            vec!["rename", "-u", "-f", "tank/old", "tank/a/new"],
        ),
    ] {
        assert_eq!(opts.build_args("tank/old", "tank/a/new").unwrap(), args);
        rename(&success(&args), "tank/old", "tank/a/new", &opts)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn rename_rejects_invalid_input_before_execution() {
    let runner = RecordingRunner::new();
    for (source, target) in [
        ("tank/old", "tank//new"),
        ("tank/old@s", "tank/new"),
        ("tank/old", "tank/new@s"),
    ] {
        assert!(matches!(
            rename(&runner, source, target, &Default::default()).await,
            Err(ZfsError::InvalidName(_))
        ));
    }
    assert!(matches!(
        rename(
            &runner,
            "tank/old",
            "tank/new",
            &RenameOptions::new().parents().no_remount()
        )
        .await,
        Err(ZfsError::InvalidInput { .. })
    ));
    let zfs = Zfs::with_runner(runner);
    assert!(matches!(
        zfs.dataset("tank/old")
            .unwrap()
            .rename("tank//new", &Default::default())
            .await,
        Err(ZfsError::InvalidName(_))
    ));
}

#[tokio::test]
async fn rename_classifies_captured_collision() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zfs").args([
            "rename",
            "zfskit_test_diagnostics/clone",
            "zfskit_test_diagnostics/data",
        ]),
        vec![],
        include_bytes!("fixtures/stderr/rename_exists.txt").to_vec(),
        1,
    );
    assert!(
        matches!(rename(&runner, "zfskit_test_diagnostics/clone", "zfskit_test_diagnostics/data", &Default::default()).await, Err(ZfsError::DatasetExists { name }) if name == "zfskit_test_diagnostics/clone")
    );
}

#[tokio::test]
async fn snapshot_rename_returns_new_handle() {
    for (opts, args) in [
        (
            SnapshotRenameOptions::new(),
            vec!["rename", "tank/data@old", "tank/data@new"],
        ),
        (
            SnapshotRenameOptions::new().recursive(),
            vec!["rename", "-r", "tank/data@old", "tank/data@new"],
        ),
    ] {
        let zfs = Zfs::with_runner(success(&args));
        let renamed = zfs
            .snapshot("tank/data@old")
            .unwrap()
            .rename("new", &opts)
            .await
            .unwrap();
        assert_eq!(renamed.name(), "tank/data@new");
    }
}

#[tokio::test]
async fn snapshot_rename_rejects_cross_dataset_and_bad_tags() {
    let runner = RecordingRunner::new();
    assert!(matches!(
        rename_snapshot(&runner, "tank/a@s", "tank/b@s", &Default::default()).await,
        Err(ZfsError::InvalidInput { .. })
    ));
    assert!(matches!(
        rename_snapshot(&runner, "tank/a@s", "tank/a@bad/tag", &Default::default()).await,
        Err(ZfsError::InvalidName(_))
    ));
    let zfs = Zfs::with_runner(runner);
    assert!(matches!(
        zfs.snapshot("tank/a@s")
            .unwrap()
            .rename("new@bad", &Default::default())
            .await,
        Err(ZfsError::InvalidName(_))
    ));
}

#[tokio::test]
async fn promote_success_and_failure() {
    let runner = success(&["promote", "tank/copy"]).record(
        Cmd::new("zfs").args(["promote", "zfskit_test_diagnostics/data"]),
        vec![],
        include_bytes!("fixtures/stderr/promote_not_clone.txt").to_vec(),
        1,
    );
    promote(&runner, "tank/copy").await.unwrap();
    let zfs = Zfs::with_runner(runner);
    zfs.dataset("tank/copy").unwrap().promote().await.unwrap();
    assert!(
        matches!(zfs.dataset("zfskit_test_diagnostics/data").unwrap().promote().await, Err(ZfsError::NotClone { name }) if name == "zfskit_test_diagnostics/data")
    );
}

#[tokio::test]
async fn promote_rejects_snapshot_name() {
    assert!(matches!(
        promote(&RecordingRunner::new(), "tank/data@s").await,
        Err(ZfsError::InvalidName(_))
    ));
}

#[tokio::test]
async fn unknown_diagnostics_remain_other() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zfs").args(["promote", "tank/copy"]),
        vec![],
        b"unexpected diagnostic\n".to_vec(),
        7,
    );
    assert!(
        matches!(promote(&runner, "tank/copy").await, Err(ZfsError::Other { exit_code: Some(7), stderr }) if stderr == "unexpected diagnostic")
    );
}
