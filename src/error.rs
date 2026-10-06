use std::sync::OnceLock;

use regex::Regex;
use thiserror::Error;

use crate::names::NameError;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ZfsError {
    #[error(transparent)]
    InvalidName(#[from] NameError),

    #[error("invalid operation input: {message}")]
    InvalidInput { message: String },

    #[error("failed to parse {command} output: {message}")]
    Parse {
        command: &'static str,
        message: String,
    },

    #[error("unsupported {command} output version {major}.{minor}")]
    IncompatibleOutput {
        command: &'static str,
        major: u32,
        minor: u32,
    },

    #[error("bookmark {bookmark} already exists for a different snapshot")]
    BookmarkConflict { bookmark: String },
    #[error("dataset not found: {name}")]
    DatasetNotFound { name: String },

    /// The entity named in a create/rename diagnostic. For rename, OpenZFS
    /// names the source even though the destination is the existing dataset.
    #[error("dataset already exists (operation on {name})")]
    DatasetExists { name: String },

    #[error("dataset is not a clone: {name}")]
    NotClone { name: String },

    #[error("multiple importable pools named {name}; import by GUID instead")]
    AmbiguousPool { name: String },

    #[error("permission denied")]
    PermissionDenied,

    #[error("dataset is busy: {name}")]
    Busy { name: String },

    #[error("snapshot is held: {name}")]
    SnapshotHeld { name: String },

    #[error("snapshot already exists: {name}")]
    SnapshotExists { name: String },

    #[error("encryption key not loaded{}", name.as_deref().map(|n| format!(" for {n}")).unwrap_or_default())]
    KeyNotLoaded { name: Option<String> },

    #[error("pool not found: {name}")]
    PoolNotFound { name: String },

    #[error("out of space")]
    NoSpace,

    #[error("failed to execute ZFS command: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("zfs exited with {exit_code:?}: {stderr}")]
    Other {
        exit_code: Option<i32>,
        stderr: String,
    },
}

fn dataset_not_found_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"cannot open '([^']+)': (?:dataset does not exist|no such pool or dataset)")
            .expect("dataset_not_found regex compiles")
    })
}

fn busy_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"cannot [a-z ]+ '([^']+)': (?:dataset|pool) is busy")
            .expect("busy regex compiles")
    })
}

fn snapshot_held_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"cannot destroy snapshot (.+): it's being held")
            .expect("snapshot_held regex compiles")
    })
}

fn snapshot_exists_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"cannot create snapshot '([^']+)': dataset already exists")
            .expect("snapshot_exists regex compiles")
    })
}

fn key_not_loaded_named_re() -> &'static Regex {
    // OpenZFS canonical form when an encrypted dataset is operated on without
    // its key: `Key must be loaded for 'tank/encrypted'.`
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Key must be loaded for '([^']+)'")
            .expect("key_not_loaded_named regex compiles")
    })
}

const KEY_NOT_LOADED_MARKER: &str = "Key must be loaded";

fn pool_not_found_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?m)cannot (?:open|import|destroy|export) '([^']+)': (?:no such pool available|no such pool)\s*$",
        )
            .expect("pool_not_found regex compiles")
    })
}

fn dataset_exists_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?m)^cannot (?:create|rename) '([^']+)': dataset already exists\s*$")
            .expect("dataset_exists regex compiles")
    })
}

fn not_clone_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?m)^cannot promote '([^']+)': not a cloned filesystem\s*$")
            .expect("not_clone regex compiles")
    })
}

fn ambiguous_pool_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?m)^cannot import '([^']+)': more than one matching pool\s*$")
            .expect("ambiguous_pool regex compiles")
    })
}

pub fn classify_stderr(stderr: &str, exit_code: Option<i32>) -> ZfsError {
    if let Some(caps) = dataset_exists_re().captures(stderr) {
        return ZfsError::DatasetExists {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = not_clone_re().captures(stderr) {
        return ZfsError::NotClone {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = ambiguous_pool_re().captures(stderr) {
        return ZfsError::AmbiguousPool {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = pool_not_found_re().captures(stderr) {
        return ZfsError::PoolNotFound {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = dataset_not_found_re().captures(stderr) {
        return ZfsError::DatasetNotFound {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = snapshot_held_re().captures(stderr) {
        return ZfsError::SnapshotHeld {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = snapshot_exists_re().captures(stderr) {
        return ZfsError::SnapshotExists {
            name: caps[1].to_string(),
        };
    }
    if let Some(caps) = key_not_loaded_named_re().captures(stderr) {
        return ZfsError::KeyNotLoaded {
            name: Some(caps[1].to_string()),
        };
    }
    if stderr.contains(KEY_NOT_LOADED_MARKER) {
        return ZfsError::KeyNotLoaded { name: None };
    }
    if let Some(caps) = busy_re().captures(stderr) {
        return ZfsError::Busy {
            name: caps[1].to_string(),
        };
    }
    if stderr.contains("permission denied") {
        return ZfsError::PermissionDenied;
    }
    if stderr.contains("out of space") || stderr.contains("no space left on device") {
        return ZfsError::NoSpace;
    }
    ZfsError::Other {
        exit_code,
        stderr: stderr.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_dataset_not_found_dataset_form() {
        let err = classify_stderr("cannot open 'tank/foo': dataset does not exist\n", Some(1));
        let ZfsError::DatasetNotFound { name } = err else {
            panic!("expected DatasetNotFound");
        };
        assert_eq!(name, "tank/foo");
    }

    #[test]
    fn classifies_dataset_not_found_pool_form() {
        let err = classify_stderr("cannot open 'nope': no such pool or dataset\n", Some(1));
        let ZfsError::DatasetNotFound { name } = err else {
            panic!("expected DatasetNotFound");
        };
        assert_eq!(name, "nope");
    }

    #[test]
    fn classifies_permission_denied() {
        let err = classify_stderr("cannot list 'tank': permission denied\n", Some(1));
        assert!(matches!(err, ZfsError::PermissionDenied));
    }

    #[test]
    fn classifies_out_of_space() {
        let err = classify_stderr("out of space\n", Some(1));
        assert!(matches!(err, ZfsError::NoSpace));
    }

    #[test]
    fn classifies_busy() {
        let err = classify_stderr("cannot destroy 'tank/foo': dataset is busy\n", Some(1));
        let ZfsError::Busy { name } = err else {
            panic!("expected Busy");
        };
        assert_eq!(name, "tank/foo");
    }

    #[test]
    fn classifies_busy_pool() {
        let err = classify_stderr("cannot export 'tank': pool is busy\n", Some(1));
        assert!(matches!(err, ZfsError::Busy { name } if name == "tank"));
    }

    #[test]
    fn classifies_snapshot_held() {
        let err = classify_stderr(
            "cannot destroy snapshot tank/data/home@snap1: it's being held. \
             Run 'zfs holds -r tank/data/home@snap1' to see holders.\n",
            Some(1),
        );
        let ZfsError::SnapshotHeld { name } = err else {
            panic!("expected SnapshotHeld, got {err:?}");
        };
        assert_eq!(name, "tank/data/home@snap1");
    }

    #[test]
    fn classifies_snapshot_held_when_name_contains_colon() {
        let err = classify_stderr(
            "cannot destroy snapshot tank/data:v1@snap:old: it's being held. \
             Run 'zfs holds -r tank/data:v1@snap:old' to see holders.\n",
            Some(1),
        );
        assert!(matches!(err, ZfsError::SnapshotHeld { name } if name == "tank/data:v1@snap:old"));
    }

    #[test]
    fn classifies_snapshot_exists() {
        let err = classify_stderr(
            "cannot create snapshot 'tank/data@snap1': dataset already exists\n",
            Some(1),
        );
        let ZfsError::SnapshotExists { name } = err else {
            panic!("expected SnapshotExists, got {err:?}");
        };
        assert_eq!(name, "tank/data@snap1");
    }

    #[test]
    fn classifies_key_not_loaded_named() {
        let err = classify_stderr("Key must be loaded for 'tank/encrypted'.\n", Some(1));
        let ZfsError::KeyNotLoaded { name } = err else {
            panic!("expected KeyNotLoaded, got {err:?}");
        };
        assert_eq!(name.as_deref(), Some("tank/encrypted"));
    }

    #[test]
    fn classifies_key_not_loaded_unnamed_falls_back() {
        // Hypothetical short form without the dataset name in the message.
        let err = classify_stderr("Key must be loaded.\n", Some(1));
        let ZfsError::KeyNotLoaded { name } = err else {
            panic!("expected KeyNotLoaded, got {err:?}");
        };
        assert_eq!(name, None);
    }

    #[test]
    fn classifies_pool_not_found() {
        let err = classify_stderr("cannot import 'tank': no such pool available\n", Some(1));
        let ZfsError::PoolNotFound { name } = err else {
            panic!("expected PoolNotFound, got {err:?}");
        };
        assert_eq!(name, "tank");
    }

    #[test]
    fn classifies_pool_not_found_from_zpool_list() {
        let err = classify_stderr("cannot open 'missing': no such pool\n", Some(1));
        assert!(matches!(err, ZfsError::PoolNotFound { name } if name == "missing"));
    }

    #[test]
    fn falls_back_to_other_with_trimmed_stderr() {
        let err = classify_stderr("some weird new zfs error\n", Some(2));
        let ZfsError::Other { exit_code, stderr } = err else {
            panic!("expected Other");
        };
        assert_eq!(exit_code, Some(2));
        assert_eq!(stderr, "some weird new zfs error");
    }
}
