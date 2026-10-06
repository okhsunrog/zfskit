use zfskit::dataset::{CreateOptions, create};
use zfskit::{Cmd, RecordingRunner, ZfsError};

#[tokio::test]
async fn create_no_props_succeeds() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zfs").args(["create", "tank/data"]),
        vec![],
        vec![],
        0,
    );
    create(&runner, "tank/data", &CreateOptions::new())
        .await
        .expect("create succeeds");
}

#[tokio::test]
async fn create_no_mount_emits_dash_u_before_properties() {
    // Boot-environment case: the mountpoint is fixed at creation time, but the
    // dataset must not be mounted over the running system's /home.
    let runner = RecordingRunner::new().record(
        Cmd::new("zfs").args([
            "create",
            "-p",
            "-u",
            "-o",
            "mountpoint=/home",
            "tank/be0/data/home",
        ]),
        vec![],
        vec![],
        0,
    );
    create(
        &runner,
        "tank/be0/data/home",
        &CreateOptions::new()
            .create_parents()
            .no_mount()
            .property("mountpoint", "/home"),
    )
    .await
    .expect("create with -u succeeds");
}

#[tokio::test]
async fn create_with_properties_emits_dash_o_pairs() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zfs").args([
            "create",
            "-o",
            "mountpoint=/mnt/data",
            "-o",
            "compression=lz4",
            "tank/data",
        ]),
        vec![],
        vec![],
        0,
    );
    let opts = CreateOptions::new()
        .property("mountpoint", "/mnt/data")
        .property("compression", "lz4");
    create(&runner, "tank/data", &opts)
        .await
        .expect("create with props succeeds");
}

#[tokio::test]
async fn create_classifies_existing_dataset() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zfs").args(["create", "tank/data"]),
        vec![],
        b"cannot create 'tank/data': dataset already exists\n".to_vec(),
        1,
    );
    let err = create(&runner, "tank/data", &CreateOptions::new())
        .await
        .expect_err("already-exists should error");
    let ZfsError::DatasetExists { name } = err else {
        panic!("expected DatasetExists, got {err:?}");
    };
    assert_eq!(name, "tank/data");
}
