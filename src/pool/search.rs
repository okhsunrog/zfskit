use std::ffi::OsString;
use std::path::PathBuf;

/// Where `zpool import` searches, for discovery as well as import. Cachefile
/// lookup (`-c`) and directory/device search (`-d`) are mutually exclusive.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub enum PoolSearchSource {
    /// OpenZFS's platform default (libblkid on Linux).
    #[default]
    Default,
    /// Read a configuration cache instead of scanning devices.
    CacheFile(PathBuf),
    /// Repeated `-d` directories or individual devices/files. An empty list
    /// passes no search flags and therefore uses OpenZFS's default search.
    Directories(Vec<PathBuf>),
}

impl PoolSearchSource {
    pub(crate) fn append_args(&self, args: &mut Vec<OsString>) {
        match self {
            Self::Default => {}
            Self::CacheFile(path) => args.extend(["-c".into(), path.as_os_str().into()]),
            Self::Directories(paths) => {
                for path in paths {
                    args.extend(["-d".into(), path.as_os_str().into()]);
                }
            }
        }
    }
}
