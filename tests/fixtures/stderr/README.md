These C-locale stderr messages were captured on the repository's archzfs
QEMU test ISO with OpenZFS userspace and kernel module 2.4.4-1.

The throwaway pool `zfskit_test_diagnostics` used two 256 MiB sparse files.
After creating `data`, `data@snap`, and `clone`, the commands were:

- `zfs clone zfskit_test_diagnostics/data@snap zfskit_test_diagnostics/clone`
- `zfs clone zfskit_test_diagnostics/data@missing zfskit_test_diagnostics/other`
- `zfs clone zfskit_test_diagnostics/data zfskit_test_diagnostics/other`
- `zfs rename zfskit_test_diagnostics/clone zfskit_test_diagnostics/data`
- `zfs promote zfskit_test_diagnostics/data`
- `zpool import -d /tmp/zfskit_test_diagnostics_dir 18446744073709551615`

For the duplicate-name case, the first pool was exported, a second pool with
that same name was created on the second file and exported, then:
`zpool import -d /tmp/zfskit_test_diagnostics_dir zfskit_test_diagnostics`.

The rename diagnostic names the **source**, not the destination. The invalid
clone source without `@` remains unclassified (`Other`); the typed clone API
rejects that input before executing ZFS.
