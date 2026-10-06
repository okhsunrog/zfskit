<h1 align="center">zfskit</h1>

<p align="center">Async toolkit for OpenZFS — datasets, snapshots, send/recv streams, holds, bookmarks, pools.</p>

<p align="center">
  <a href="https://crates.io/crates/zfskit"><img alt="crates.io" src="https://img.shields.io/crates/v/zfskit.svg"></a>
  <a href="https://docs.rs/zfskit"><img alt="docs.rs" src="https://img.shields.io/docsrs/zfskit"></a>
  <img alt="license" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg">
</p>

zfskit drives the `zfs(8)` / `zpool(8)` command-line tools from async Rust.
It is the ZFS layer behind
[arctern](https://github.com/okhsunrog/arctern) (a ZFS replication daemon)
and grew out of an Arch Linux ZFS installer — so both the replication
surface (send/recv, holds, bookmarks, resume tokens) and the provisioning
surface (pool create/import, dataset trees, encryption keys) are exercised
by real consumers.

## Design

- **CLI-only, no FFI.** No `libzfs`/`libzfs_core` bindings: using the command-line
  interface avoids an unstable library ABI, needs no C toolchain or
  kernel-matched headers, and supports the same `CommandRunner` API locally
  and over SSH.
- **JSON output, not tab-splitting.** Structured `-j` output (OpenZFS ≥ 2.3)
  is parsed into serde models; version-tolerant where field sets drift
  between ZFS releases.
- **Async-only and cancellation-safe.** Processes are spawned via
  `tokio::process`; buffered commands are killed when their future is dropped,
  while typed send/receive processes expose explicit `finish()` and `cancel()`.
- **Every operation is testable without ZFS.** `Zfs` accepts a custom
  `CommandRunner`; `RealRunner` executes locally, `SshCommandRunner` executes
  on a remote host, and `RecordingRunner` replays captured fixtures in unit
  tests. The runner remains available as an advanced escape hatch.
- **Errors are classified**, not stringly: `ZfsError` distinguishes
  dataset-not-found, snapshot-held, permission, busy-pool and friends by
  parsing stderr, so callers can branch on the failure mode.

## What's covered

| Area | Operations |
|---|---|
| Datasets | list / get / set / create / destroy / mount / snapshot / rollback / clone / rename (datasets and snapshots) / promote |
| Pools | list / status (vdev tree, scan state) / create / destroy / import (name or GUID, properties, search sources, key loading) / export / discover (default, directories/devices, cachefile) / scrub (start·pause·resume·stop) / get / set |
| Replication | `zfs send` (raw/embedded/compressed/large-blocks, incremental from snapshot or bookmark, resume tokens) · `zfs recv` (`-s`/`-u`/`-F`, `-o`/`-x` property control) · dry-run size estimation · resume-token parsing and validation · `recv -A` partial-state abort |
| Protection | holds (idempotent hold/release, batch inspection) · bookmarks (GUID-anchored create/list/destroy) |
| Encryption | load-key / unload-key / property inspection / non-mutating passphrase verification |
| System | ARC statistics from `/proc/spl/kstat/zfs/arcstats` |

## Example

```rust
use zfskit::Zfs;
use zfskit::dataset::ListOptions;
use zfskit::models::DatasetType;

#[tokio::main]
async fn main() -> Result<(), zfskit::ZfsError> {
    let zfs = Zfs::new(); // system zfs(8)/zpool(8) via RealRunner

    // Pool health.
    let status = zfs.pool("tank")?.status().await?;
    println!("tank: {}", status.state);

    // Snapshots of one dataset, with their GUIDs.
    let opts = ListOptions {
        types: vec![DatasetType::Snapshot],
        roots: vec!["tank/data".into()],
        properties: vec!["guid".into()],
        ..ListOptions::default()
    };
    for snap in zfs.list_datasets(&opts).await? {
        println!("{}", snap.name);
    }

    // Take a snapshot.
    let ds = zfs.dataset("tank/data")?;
    ds.snapshot("backup_2026-07-09", &Default::default()).await?;
    Ok(())
}
```

Snapshot clones and dataset moves use entity handles:

```rust
use zfskit::Zfs;
use zfskit::dataset::{CloneOptions, RenameOptions};

async fn clone_dataset(zfs: &Zfs) -> Result<(), zfskit::ZfsError> {
    let source = zfs.dataset("tank/data")?;
    let snapshot = source.snapshot("before-update", &Default::default()).await?;
    let clone = snapshot.clone_as(
        "tank/copies/test",
        &CloneOptions::new().parents().property("mountpoint", "none"),
    ).await?;
    let clone = clone.rename("tank/copies/current", &RenameOptions::new()).await?;
    clone.promote().await?;
    Ok(())
}
```

Rename consumes the old handle and returns the new name. Other copies of the
handle, and handles for descendants or snapshots, must be reacquired after a
rename. `Snapshot::rename(tag, &SnapshotRenameOptions)` keeps snapshot recursion
separate from dataset rename. Dataset `-p` (create parents) and `-u` (keep existing
mounts) are mutually exclusive and rejected before execution.

Clones have the source dataset's type and must stay in the same pool. Clone
`-p` also succeeds when the destination already exists; it does not verify that
existing dataset's origin or apply the requested properties. There is no clone
`-u` flag: control mounting through properties such as `mountpoint=none` or
`canmount=off`. Promotion reverses the origin dependency and transfers ownership
of the origin snapshot and earlier snapshots; conflicting snapshot names must
be resolved by the caller.

Discovery and import share the same search-source model. Importing the returned
pool uses its unsigned 64-bit GUID, so duplicate exported pool names are safe:

```rust
use zfskit::Zfs;
use zfskit::pool::{DiscoverOptions, ImportOptions, PoolSearchSource};

async fn inspect_pools(zfs: &Zfs) -> Result<(), zfskit::ZfsError> {
    let source = PoolSearchSource::Directories(vec!["/dev/disk/by-id".into()]);
    let discovery = DiscoverOptions::new().search_source(source.clone());
    for discovered in zfs.discover_importable_pools_with(&discovery).await? {
        let pool = zfs.import_pool(
            &discovered,
            &ImportOptions::new().no_mount().altroot("/mnt/inspect")
                .readonly().property("cachefile", "none")
                .search_source(source.clone()),
        ).await?;
        println!("{}", pool.name());
        pool.export(&Default::default()).await?;
    }
    Ok(())
}
```

`PoolSearchSource::CacheFile(path)` uses a saved configuration instead of device
search; the enum prevents mixing `-c` and `-d`. Discovery retains the original
`DiscoveredPool.id` string; `guid()` validates it as a `u64` before import. The
low-level `pool::import_by_guid` accepts a `u64` directly. Existing name-based
`Pool::import` and default discovery remain available. Search sources must be
passed to import again; they are not stored in discovery results.

`ImportOptions::load_keys()` passes `-l` for keys of datasets being mounted;
`-l` and `-N` are incompatible and rejected before execution. It supplies no passphrase or terminal handling.
Use the explicit dataset key-loading APIs to control input, particularly for
`keylocation=prompt`. `-R` sets both the alternate root and `cachefile=none`;
explicit import properties can override that cachefile setting.

Advanced import recovery is deliberately deferred: transaction rewind (`-F`,
`-n`, `-X`, `-T`), checkpoint rewind, missing-log import (`-m`), damaged-metadata
import, and destroyed-pool recovery need a separate explicit policy API.

Replication is the same primitives with the stream left to you:

```rust
use tokio::io::AsyncWriteExt;
use zfskit::send::SendArgs;
use zfskit::recv::RecvArgs;

let zfs = zfskit::Zfs::new();
let mut src = zfs.send(&SendArgs::new("tank/data@snap").raw()).await?;
let mut dst = zfs.receive(&RecvArgs::new("backup/data").resumable().unmounted()).await?;
let mut stdout = src.take_stdout().expect("send stdout");
let mut stdin = dst.take_stdin().expect("recv stdin");
tokio::io::copy(&mut stdout, &mut stdin).await?;
stdin.shutdown().await?;
drop(stdin);
src.finish().await?;
dst.finish().await?;
```

The SSH runner is intended for development and integration tests against a
disposable remote ZFS host. Production applications should run zfskit on the
ZFS host or provide a transport with explicit remote lifecycle guarantees:

```rust
use zfskit::{SshTarget, SshCommandRunner, Zfs};

// or SshCommandRunner::from_env() reading ZFSKIT_SSH_TARGET / ZFSKIT_SSH_PASSWORD
let runner = SshCommandRunner::new(SshTarget::parse("root@nas.lan:22")?);
let zfs = Zfs::with_runner(runner);
```

## Testing

Unit tests replay captured JSON fixtures through `RecordingRunner` — they
never touch a real pool. The `integration` feature enables end-to-end tests
against a disposable ZFS host over SSH (`ZFSKIT_SSH_TARGET=user@host:port`);
the repo's `justfile` boots a QEMU VM for that. Run `just vm-up`, then
`just test-integration`, and finally `just vm-down`. With a VM already running,
all feature checks can be run with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features
ZFSKIT_SSH_TARGET=root@localhost:2226 ZFSKIT_SSH_PASSWORD='' cargo test --all-features
```

The lifecycle integration tests cover clone properties/parents, dataset and
snapshot rename, promotion origin reversal, captured error classification,
export/discovery, GUID imports without mounting, readonly import, cachefile
search, and file-based key loading. Each test uses an isolated sparse-file pool
and the existing cleanup helper.

## Requirements

- Rust 1.85 or newer (the crate uses the Rust 2024 edition).
- OpenZFS ≥ 2.3 (JSON output) on the host that runs the commands — local or
  at the far end of the SSH runner.
- Linux. The ARC-stats module reads `/proc/spl/kstat`; everything else is
  CLI-portable in principle, but only Linux is tested.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
