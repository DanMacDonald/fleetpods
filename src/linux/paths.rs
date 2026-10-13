//! Socket and status locations for this daemon.
//! An unsafe runtime directory is an error. There is no `/tmp` fallback.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const SOCKET_NAME: &str = "fleetpods.sock";
pub const STATUS_DIR: &str = "fleetpods";
pub const STATUS_FILE: &str = "status.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPaths {
    pub socket: PathBuf,
    pub status: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    RuntimeMissing,
    RuntimeRelative,
    RuntimeUnsafe,
    StateMissing,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeMissing => {
                write!(f, "XDG_RUNTIME_DIR is unset; refusing to use /tmp")
            }
            Self::RuntimeRelative => write!(f, "runtime directory is not absolute"),
            Self::RuntimeUnsafe => {
                write!(f, "runtime directory is unsafe; refusing a shared socket")
            }
            Self::StateMissing => write!(f, "state directory is not set"),
        }
    }
}

pub fn resolve(
    runtime_dir: &Path,
    runtime_meta: &Metadata,
    state_home: &Path,
) -> Result<SessionPaths, PathError> {
    validate_runtime(runtime_dir, runtime_meta)?;
    if state_home.as_os_str().is_empty() {
        return Err(PathError::StateMissing);
    }
    Ok(SessionPaths {
        socket: runtime_dir.join(SOCKET_NAME),
        status: state_home.join(STATUS_DIR).join(STATUS_FILE),
    })
}

pub fn validate_runtime(path: &Path, meta: &Metadata) -> Result<(), PathError> {
    if path.as_os_str().is_empty() {
        return Err(PathError::RuntimeMissing);
    }
    if !path.is_absolute() {
        return Err(PathError::RuntimeRelative);
    }
    if path == Path::new("/tmp") || path.starts_with("/tmp/") {
        return Err(PathError::RuntimeUnsafe);
    }
    // Group- or world-writable directories would expose the socket.
    if meta.mode() & 0o022 != 0 {
        return Err(PathError::RuntimeUnsafe);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn meta_of(path: &Path) -> Metadata {
        fs::metadata(path).unwrap()
    }

    #[test]
    fn rejects_tmp_and_a_writable_directory() {
        let tmp = meta_of(Path::new("/tmp"));
        assert_eq!(validate_runtime(Path::new("/tmp"), &tmp), Err(PathError::RuntimeUnsafe));
        assert!(validate_runtime(Path::new("run/user/1000"), &tmp).is_err());
    }
}
