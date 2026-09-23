# Changelog

## [0.2.2] — 2026-09-23

### Fixed

- `Zfs::list_pools()` returns an empty list when no pool is imported. OpenZFS 2.4
  `zpool list -j` prints no JSON in that case, which failed to parse.

## [0.2.1] — 2026-08-14

### Added

- `CreateOptions::no_mount()` support for creating datasets with `zfs create -u`.
- `SetOptions::no_mount()` and multi-property setters for changing mount-related
  properties without mounting datasets.

## [0.2.0] — 2026-07-10

This release is a breaking API redesign. It requires Rust 1.85+ and OpenZFS
2.3+; OpenZFS 2.3 is the first release with the JSON command output used by
zfskit.

### Added

- `Zfs` as the canonical entry point, with typed `Pool`, `Dataset`,
  `Snapshot`, and `Bookmark` handles.
- Validated `PoolName`, `DatasetName`, `SnapshotName`, and `BookmarkName`
  types matching OpenZFS name checks, including separate legacy-open and
  create/import pool-name rules.
- Managed send/receive processes with explicit stream ownership, `finish()`,
  and `cancel()`.
- Non-mutating passphrase verification through `zfs load-key -n`.
- Structured input, parse, output-version, and bookmark-conflict errors.
- Forward-compatible handling of unknown JSON dataset and property-source
  kinds.

### Changed

- `Zfs::pool()`, `dataset()`, `snapshot()`, and `bookmark()` now validate names
  and return `Result`.
- Existence probes return `Result<bool, ZfsError>`; explicit `*_best_effort`
  helpers retain error-collapsing behavior.
- `SendArgs::snapshot` is now `SendArgs::target`, because OpenZFS also supports
  filesystem and volume head sends. Use `SendArgs::resume(token)` for resumed
  sends.
- `RecvArgs::exclude_first_component` is now
  `discard_except_last_component`, matching `zfs receive -e` semantics.
- Send and receive argument builders reject invalid names and incompatible
  option combinations before spawning OpenZFS.
- Error classification now runs commands in the C locale and recognizes
  missing/busy pools as well as datasets.

### Security and reliability

- Secret stdin is redacted from diagnostics and zeroized after use.
- `sshpass` passwords are no longer placed in process arguments.
- Buffered and streaming child processes are killed on dropped futures or
  handles; waiting closes untaken pipes to avoid deadlocks.
- Send/receive stderr is drained concurrently to avoid pipe-buffer stalls.

[0.2.2]: https://github.com/okhsunrog/zfskit/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/okhsunrog/zfskit/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/okhsunrog/zfskit/compare/7931a91...v0.2.0
