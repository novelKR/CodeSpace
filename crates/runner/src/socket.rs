//! Private Unix-socket rendezvous. CodeSpace never chmods `/tmp` or
//! other preexisting directories: only a newly created 0700 leaf.

use std::fs::DirBuilder;
use std::io::{Error, ErrorKind, Result};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const RUNNER_SOCKET_NAME: &str = "runner.sock";

static DIR_NONCE: AtomicU64 = AtomicU64::new(1);

/// True when `path` is `/`, `/tmp`, `/var/tmp`, or `$HOME` itself.
/// A child directory under those bases is allowed.
pub fn is_forbidden_runner_dir(path: &Path) -> bool {
    let requested = normalize_path(path);
    forbidden_runner_bases()
        .into_iter()
        .any(|base| requested == base)
}

pub fn runner_socket_path(dir: impl AsRef<Path>) -> PathBuf {
    dir.as_ref().join(RUNNER_SOCKET_NAME)
}

/// Create a unique 0700 leaf. Never chmods an existing directory.
///
/// * `Some(base)` → `$base/run-<pid>-<rand>/` (`base` must not be a
///   forbidden leaf).
/// * `None` → `$TMPDIR/codespace-runner-<pid>-<rand>/`.
pub fn allocate_private_runner_dir(base: Option<&Path>) -> Result<PathBuf> {
    match base {
        Some(base) => {
            if is_forbidden_runner_dir(base) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "runner dir must not be /, /tmp, /var/tmp, or $HOME (got {})",
                        base.display()
                    ),
                ));
            }
            if !base.exists() {
                std::fs::create_dir_all(base)?;
            }
            create_unique_leaf(base, "run")
        }
        None => create_unique_leaf(&std::env::temp_dir(), "codespace-runner"),
    }
}

/// Connect-probe a rendezvous path. Live listeners are not unlinked.
/// `ConnectionRefused` leftover files are removed. Missing paths are ok.
pub fn reclaim_leftover_socket(path: &Path) -> Result<()> {
    match UnixStream::connect(path) {
        Ok(_live) => Err(Error::new(
            ErrorKind::AddrInUse,
            format!("runner socket is already in use at {}", path.display()),
        )),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {
            match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(remove_err) if remove_err.kind() == ErrorKind::NotFound => Ok(()),
                Err(remove_err) => Err(remove_err),
            }
        }
        Err(err) => {
            if !path.exists() {
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

fn create_unique_leaf(parent: &Path, prefix: &str) -> Result<PathBuf> {
    let pid = std::process::id();
    for _ in 0..64 {
        let nonce = DIR_NONCE.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("{prefix}-{pid}-{nanos:x}-{nonce:x}");
        let path = parent.join(name);
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(Error::other("could not allocate a unique runner directory"))
}

fn forbidden_runner_bases() -> Vec<PathBuf> {
    let mut bases = vec![
        PathBuf::from("/"),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            bases.push(PathBuf::from(home));
        }
    }
    bases
        .into_iter()
        .map(|path| normalize_path(&path))
        .collect()
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    #[test]
    fn refuses_tmp_and_home_as_the_leaf() {
        assert!(is_forbidden_runner_dir(Path::new("/")));
        assert!(is_forbidden_runner_dir(Path::new("/tmp")));
        assert!(is_forbidden_runner_dir(Path::new("/var/tmp")));
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                assert!(is_forbidden_runner_dir(Path::new(&home)));
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_forbidden_runner_dir(tmp.path()));
    }

    #[test]
    fn allocate_creates_0700_unique_leaf_without_chmod_parent() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let leaf = allocate_private_runner_dir(Some(parent.path())).unwrap();
        let leaf_mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
        let parent_mode = std::fs::metadata(parent.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(leaf_mode, 0o700);
        assert_eq!(parent_mode, 0o755);
        assert!(leaf
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .starts_with("run-"));
        assert_eq!(
            runner_socket_path(&leaf).file_name().unwrap(),
            RUNNER_SOCKET_NAME
        );
        std::fs::remove_dir_all(&leaf).unwrap();
    }

    #[test]
    fn allocate_rejects_forbidden_base() {
        let err = allocate_private_runner_dir(Some(Path::new("/tmp"))).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn live_socket_is_not_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let sock = runner_socket_path(dir.path());
        let listener = UnixListener::bind(&sock).unwrap();
        let err = reclaim_leftover_socket(&sock).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AddrInUse);
        assert!(sock.exists());
        drop(listener);
    }

    #[test]
    fn leftover_refused_socket_is_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let sock = runner_socket_path(dir.path());
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener);
        reclaim_leftover_socket(&sock).unwrap();
        assert!(!sock.exists());
    }

    #[test]
    fn missing_socket_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        reclaim_leftover_socket(&dir.path().join("missing.sock")).unwrap();
    }
}
