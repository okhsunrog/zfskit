use zfskit::pool::{
    DiscoverOptions, DiscoveredPool, ImportOptions, PoolSearchSource, discover_with, import,
    import_by_guid,
};
use zfskit::{Cmd, RecordingRunner, Zfs, ZfsError};

fn discovered(id: &str) -> DiscoveredPool {
    DiscoveredPool {
        name: "tank".into(),
        id: id.into(),
        state: "ONLINE".into(),
        status: None,
    }
}

#[test]
fn import_default_arguments_unchanged() {
    assert_eq!(
        ImportOptions::default().build_args("tank").unwrap(),
        ["import", "tank"]
    );
}

#[tokio::test]
async fn import_flags_properties_and_native_paths() {
    let opts = ImportOptions::new()
        .force()
        .no_mount()
        .altroot("/mnt/recovery")
        .readonly()
        .property("cachefile", "none")
        .search_source(PoolSearchSource::Directories(vec![
            "/dev/disk/by-id".into(),
            "/tmp/pool file.img".into(),
        ]));
    let args = [
        "import",
        "-f",
        "-N",
        "-R",
        "/mnt/recovery",
        "-o",
        "readonly=on",
        "-o",
        "cachefile=none",
        "-d",
        "/dev/disk/by-id",
        "-d",
        "/tmp/pool file.img",
        "tank",
    ];
    assert_eq!(opts.build_args("tank").unwrap(), args);
    let runner = RecordingRunner::new().record(Cmd::new("zpool").args(args), vec![], vec![], 0);
    Zfs::with_runner(runner)
        .pool("tank")
        .unwrap()
        .import(&opts)
        .await
        .unwrap();
}

#[test]
fn legacy_import_options_arguments_unchanged() {
    let opts = ImportOptions {
        force: true,
        no_mount: true,
        ..Default::default()
    };
    assert_eq!(
        opts.build_args("tank").unwrap(),
        ["import", "-f", "-N", "tank"]
    );
    let opts = ImportOptions {
        force: true,
        altroot: Some("/mnt/recovery".into()),
        ..Default::default()
    };
    assert_eq!(
        opts.build_args("tank").unwrap(),
        ["import", "-f", "-R", "/mnt/recovery", "tank"]
    );
}

#[tokio::test]
async fn import_by_unsigned_guid_returns_named_handle() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zpool").args(["import", "-N", "18446744073709551615"]),
        vec![],
        vec![],
        0,
    );
    import_by_guid(&runner, u64::MAX, &ImportOptions::new().no_mount())
        .await
        .unwrap();
    let pool = Zfs::with_runner(runner)
        .import_pool(
            &discovered(&u64::MAX.to_string()),
            &ImportOptions::new().no_mount(),
        )
        .await
        .unwrap();
    assert_eq!(pool.name(), "tank");
}

#[tokio::test]
async fn malformed_discovery_ids_rejected_before_execution() {
    let zfs = Zfs::with_runner(RecordingRunner::new());
    for id in ["", "-1", "+1", "abc", "18446744073709551616"] {
        assert!(matches!(
            zfs.import_pool(&discovered(id), &Default::default()).await,
            Err(ZfsError::InvalidInput { .. })
        ));
    }
    let mut pool = discovered("1");
    pool.name = "tank/invalid".into();
    assert!(matches!(
        zfs.import_pool(&pool, &Default::default()).await,
        Err(ZfsError::InvalidName(_))
    ));
}

#[tokio::test]
async fn captured_import_errors_are_classified() {
    let runner = RecordingRunner::new()
        .record(
            Cmd::new("zpool").args(["import", "18446744073709551615"]),
            vec![],
            include_bytes!("fixtures/stderr/import_missing_guid.txt").to_vec(),
            1,
        )
        .record(
            Cmd::new("zpool").args(["import", "zfskit_test_diagnostics"]),
            vec![],
            include_bytes!("fixtures/stderr/import_duplicate_name.txt").to_vec(),
            1,
        );
    assert!(
        matches!(import_by_guid(&runner, u64::MAX, &Default::default()).await, Err(ZfsError::PoolNotFound { name }) if name == u64::MAX.to_string())
    );
    assert!(
        matches!(import(&runner, "zfskit_test_diagnostics", &Default::default()).await, Err(ZfsError::AmbiguousPool { name }) if name == "zfskit_test_diagnostics")
    );
}

#[tokio::test]
async fn sources_work_for_both_discovery_and_import() {
    for (source, flags) in [
        (PoolSearchSource::Default, vec![]),
        (
            PoolSearchSource::CacheFile("/tmp/zpool.cache".into()),
            vec!["-c", "/tmp/zpool.cache"],
        ),
        (
            PoolSearchSource::Directories(vec!["/dev".into(), "/tmp/disk.img".into()]),
            vec!["-d", "/dev", "-d", "/tmp/disk.img"],
        ),
        (PoolSearchSource::Directories(vec![]), vec![]),
    ] {
        let mut args = vec!["import"];
        args.extend(flags);
        let opts = DiscoverOptions::new().search_source(source.clone());
        assert_eq!(opts.build_args(), args);
        let mut import_args = args.clone();
        import_args.push("tank");
        let import_opts = ImportOptions::new().search_source(source);
        assert_eq!(import_opts.build_args("tank").unwrap(), import_args);
        let runner = RecordingRunner::new()
            .record(
                Cmd::new("zpool").args(args),
                include_bytes!("fixtures/pool_discover_two_pools.txt").to_vec(),
                vec![],
                0,
            )
            .record(Cmd::new("zpool").args(import_args), vec![], vec![], 0);
        assert_eq!(discover_with(&runner, &opts).await.unwrap().len(), 2);
        let zfs = Zfs::with_runner(runner);
        assert_eq!(
            zfs.discover_importable_pools_with(&opts)
                .await
                .unwrap()
                .len(),
            2
        );
        zfs.pool("tank")
            .unwrap()
            .import(&import_opts)
            .await
            .unwrap();
    }
}

#[test]
fn selecting_a_source_replaces_the_previous_source() {
    let source = PoolSearchSource::CacheFile("/tmp/zpool.cache".into());
    let opts = ImportOptions::new()
        .search_source(PoolSearchSource::Directories(vec!["/dev".into()]))
        .search_source(source);
    assert_eq!(
        opts.build_args("tank").unwrap(),
        ["import", "-c", "/tmp/zpool.cache", "tank"]
    );
}

#[tokio::test]
async fn configured_discovery_keeps_tolerant_handling() {
    let opts =
        DiscoverOptions::new().search_source(PoolSearchSource::Directories(vec!["/dev".into()]));
    let cmd = Cmd::new("zpool").args(["import", "-d", "/dev"]);
    let runner = RecordingRunner::new().record(
        cmd.clone(),
        vec![],
        include_bytes!("fixtures/pool_discover_two_pools.txt").to_vec(),
        1,
    );
    assert_eq!(discover_with(&runner, &opts).await.unwrap().len(), 2);
    let runner = RecordingRunner::new().record(
        cmd.clone(),
        vec![],
        b"no pools available to import\n".to_vec(),
        1,
    );
    assert!(discover_with(&runner, &opts).await.unwrap().is_empty());
    let runner = RecordingRunner::new().record(cmd, vec![], b"unknown failure\n".to_vec(), 1);
    assert!(matches!(
        discover_with(&runner, &opts).await,
        Err(ZfsError::Other { .. })
    ));
}

#[cfg(unix)]
#[test]
fn import_and_discovery_preserve_non_utf8_path_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let path = OsString::from_vec(b"/tmp/pool-\xff".to_vec());
    let opts = ImportOptions::new()
        .altroot(path.clone())
        .search_source(PoolSearchSource::CacheFile(path.clone().into()));
    assert_eq!(
        opts.build_args("tank").unwrap(),
        vec![
            "import".into(),
            "-R".into(),
            path.clone(),
            "-c".into(),
            path.clone(),
            "tank".into()
        ]
    );
    assert_eq!(
        DiscoverOptions::new()
            .search_source(PoolSearchSource::Directories(vec![path.clone().into()]))
            .build_args(),
        vec![OsString::from("import"), "-d".into(), path]
    );
}

#[tokio::test]
async fn legacy_guid_string_import_and_input_validation() {
    let runner = RecordingRunner::new().record(
        Cmd::new("zpool").args(["import", "18446744073709551615"]),
        vec![],
        vec![],
        0,
    );
    import(&runner, "18446744073709551615", &Default::default())
        .await
        .unwrap();
    assert!(matches!(
        import(&runner, "18446744073709551616", &Default::default()).await,
        Err(ZfsError::InvalidInput { .. })
    ));
    assert!(matches!(
        import(&runner, "tank/child", &Default::default()).await,
        Err(ZfsError::InvalidName(_))
    ));
}

#[tokio::test]
async fn key_loading_flag_and_no_mount_exclusion() {
    let opts = ImportOptions::new().load_keys();
    assert_eq!(opts.build_args("tank").unwrap(), ["import", "-l", "tank"]);
    let runner = RecordingRunner::new().record(
        Cmd::new("zpool").args(["import", "-l", "tank"]),
        vec![],
        vec![],
        0,
    );
    import(&runner, "tank", &opts).await.unwrap();
    let invalid = opts.no_mount();
    assert!(matches!(
        invalid.build_args("tank"),
        Err(ZfsError::InvalidInput { .. })
    ));
    assert!(matches!(
        import(&RecordingRunner::new(), "tank", &invalid).await,
        Err(ZfsError::InvalidInput { .. })
    ));
    assert!(matches!(
        import_by_guid(&RecordingRunner::new(), 123, &invalid).await,
        Err(ZfsError::InvalidInput { .. })
    ));
}
