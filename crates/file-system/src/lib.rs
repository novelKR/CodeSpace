//! Isolated filesystem adapter. Wraps `LOCAL_FS` (`ExecutorFileSystem`) and
//! exposes only CodeSpace-owned types. PathSandbox is the authorizer
//! (logical workspace selection), not the I/O safety boundary. This crate
//! pins no-follow I/O. Codex sandbox context and `PermissionProfile` stay
//! inside this crate.

use std::io;
use std::path::{Path, PathBuf};

use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::GetMetadataOptions;
use codex_exec_server::ReadFileOptions;
use codex_exec_server::RemoveOptions;
use codex_exec_server::WalkEntryKind;
use codex_exec_server::WalkOptions;
use codex_exec_server::WriteFileOptions;
use codex_exec_server::LOCAL_FS;
use codex_utils_path_uri::PathUri;

#[cfg(unix)]
mod unix;

/// Matches pinned `codex-file-system::MAX_WALK_DEPTH`.
pub const MAX_WALK_DEPTH: usize = 64;
/// Matches pinned `codex-file-system::MAX_WALK_DIRECTORIES`.
pub const MAX_WALK_DIRECTORIES: usize = 10_000;
/// Matches pinned `codex-file-system::MAX_WALK_ENTRIES`.
pub const MAX_WALK_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkResult {
    pub files: Vec<PathBuf>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    SymlinkRejected,
    NotRegularFile,
    NotDirectory,
    Io(String),
}

impl FsError {
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "file not found".into(),
            Self::SymlinkRejected => "symlink files are rejected".into(),
            Self::NotRegularFile => "not a regular file".into(),
            Self::NotDirectory => "not a directory".into(),
            Self::Io(msg) => msg.clone(),
        }
    }
}

fn uri(path: &Path) -> Result<PathUri, FsError> {
    PathUri::from_host_native_path(path).map_err(|err| FsError::Io(err.to_string()))
}

pub(crate) fn map_io(err: io::Error) -> FsError {
    if err.kind() == io::ErrorKind::NotFound {
        return FsError::NotFound;
    }
    let msg = err.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not a regular file") {
        return FsError::NotRegularFile;
    }
    // ENOTDIR is not a symlink proof. `foo/bar` when `foo` is a regular
    // file also yields it. Do not collapse that into SymlinkRejected.
    if err.kind() == io::ErrorKind::NotADirectory || lower.contains("not a directory") {
        return FsError::NotDirectory;
    }
    if lower.contains("path contains a symbolic link")
        || lower.contains("symbolic link")
        || lower.contains("symlink")
        || lower.contains("too many levels")
    {
        return FsError::SymlinkRejected;
    }
    FsError::Io(msg)
}

const NO_FOLLOW_READ: ReadFileOptions = ReadFileOptions {
    follow_symlinks: false,
};
const NO_FOLLOW_WRITE: WriteFileOptions = WriteFileOptions {
    follow_symlinks: false,
};
const NO_FOLLOW_META: GetMetadataOptions = GetMetadataOptions {
    follow_symlinks: false,
};
const NO_FOLLOW_MKDIR: CreateDirectoryOptions = CreateDirectoryOptions {
    recursive: true,
    follow_symlinks: false,
};

/// Read bytes without following symlinks. `sandbox` is always `None`.
pub async fn read(path: &Path) -> Result<Vec<u8>, FsError> {
    let path = uri(path)?;
    LOCAL_FS
        .read_file(&path, NO_FOLLOW_READ, None)
        .await
        .map_err(map_io)
}

/// Write bytes without following symlinks. `sandbox` is always `None`.
pub async fn write(path: &Path, bytes: &[u8]) -> Result<(), FsError> {
    let path = uri(path)?;
    LOCAL_FS
        .write_file(&path, bytes.to_vec(), NO_FOLLOW_WRITE, None)
        .await
        .map_err(map_io)
}

/// Metadata without following symlinks.
pub async fn metadata(path: &Path) -> Result<FileMeta, FsError> {
    let path = uri(path)?;
    let meta = LOCAL_FS
        .get_metadata(&path, NO_FOLLOW_META, None)
        .await
        .map_err(map_io)?;
    Ok(FileMeta {
        is_file: meta.is_file,
        is_dir: meta.is_directory,
        is_symlink: meta.is_symlink,
    })
}

/// Create directories without following symlinks. `sandbox` is always `None`.
pub async fn create_dir_all(path: &Path) -> Result<(), FsError> {
    let path = uri(path)?;
    LOCAL_FS
        .create_directory(&path, NO_FOLLOW_MKDIR, None)
        .await
        .map_err(map_io)
}

/// Remove a file without following symlinks.
pub async fn remove_file(path: &Path) -> Result<(), FsError> {
    let path = uri(path)?;
    LOCAL_FS
        .remove(
            &path,
            RemoveOptions {
                recursive: false,
                force: false,
                follow_symlinks: false,
            },
            None,
        )
        .await
        .map_err(map_io)
}

/// Unix mode bits without following symlinks, including intermediate dirs.
#[cfg(unix)]
pub async fn unix_mode(path: &Path) -> Result<u32, FsError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || unix::unix_mode_sync(&path))
        .await
        .map_err(|err| FsError::Io(format!("filesystem task failed: {err}")))?
}

/// chmod without following symlinks, including intermediate dirs.
#[cfg(unix)]
pub async fn set_unix_mode(path: &Path, mode: u32) -> Result<(), FsError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || unix::set_unix_mode_sync(&path, mode))
        .await
        .map_err(|err| FsError::Io(format!("filesystem task failed: {err}")))?
}

/// Bounded walk of regular files. Directory symlinks are not followed.
pub async fn walk_files(root: &Path) -> Result<WalkResult, FsError> {
    walk_files_limited(root, MAX_WALK_DEPTH, MAX_WALK_DIRECTORIES, MAX_WALK_ENTRIES).await
}

pub async fn walk_files_limited(
    root: &Path,
    max_depth: usize,
    max_directories: usize,
    max_entries: usize,
) -> Result<WalkResult, FsError> {
    let root_uri = uri(root)?;
    let outcome = LOCAL_FS
        .walk(
            &root_uri,
            WalkOptions {
                max_depth,
                max_directories,
                max_entries,
                follow_directory_symlinks: false,
                prune_hidden_directories: false,
            },
            None,
        )
        .await
        .map_err(map_io)?;
    let mut files = Vec::new();
    for entry in outcome.entries {
        if entry.kind != WalkEntryKind::File {
            continue;
        }
        files.push(entry.path.to_path_buf());
    }
    Ok(WalkResult {
        files,
        truncated: outcome.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    #[test]
    fn crate_is_isolated_adapter() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-fs");
    }

    #[test]
    fn walk_caps_match_upstream() {
        assert_eq!(MAX_WALK_DEPTH, codex_file_system::MAX_WALK_DEPTH);
        assert_eq!(
            MAX_WALK_DIRECTORIES,
            codex_file_system::MAX_WALK_DIRECTORIES
        );
        assert_eq!(MAX_WALK_ENTRIES, codex_file_system::MAX_WALK_ENTRIES);
    }

    fn canon_temp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    #[tokio::test]
    async fn read_skips_directory_symlink() {
        let (_dir, root) = canon_temp();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("secret.txt"), "nope").unwrap();
        symlink(&real, root.join("via")).unwrap();
        let err = read(&root.join("via").join("secret.txt"))
            .await
            .expect_err("directory symlink must not be followed");
        assert!(
            matches!(err, FsError::NotDirectory | FsError::SymlinkRejected),
            "expected NotDirectory or SymlinkRejected, got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_through_regular_file_is_not_directory() {
        let (_dir, root) = canon_temp();
        std::fs::write(root.join("foo"), "not-a-dir").unwrap();
        let err = read(&root.join("foo").join("bar.txt"))
            .await
            .expect_err("a file is not a directory");
        assert!(
            matches!(err, FsError::NotDirectory),
            "expected NotDirectory, got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_rejects_fifo_as_not_regular_file() {
        let (_dir, root) = canon_temp();
        let fifo = root.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
        let err = read(&fifo).await.expect_err("fifo is not a regular file");
        assert!(
            matches!(err, FsError::NotRegularFile),
            "expected NotRegularFile, got {err:?}"
        );
    }

    #[tokio::test]
    async fn create_dir_all_skips_directory_symlink() {
        let (_dir, root) = canon_temp();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join("via")).unwrap();
        let err = create_dir_all(&root.join("via").join("nested"))
            .await
            .expect_err("mkdir must not follow a directory symlink");
        assert!(
            matches!(err, FsError::NotDirectory | FsError::SymlinkRejected),
            "expected NotDirectory or SymlinkRejected, got {err:?}"
        );
    }

    #[tokio::test]
    async fn unix_mode_rejects_symlink_and_set_mode_is_nofollow() {
        let (_dir, root) = canon_temp();
        let file = root.join("a.txt");
        std::fs::write(&file, "hi").unwrap();
        let mode = unix_mode(&file).await.unwrap();
        set_unix_mode(&file, 0o600).await.unwrap();
        assert_eq!(unix_mode(&file).await.unwrap() & 0o777, 0o600);
        let _ = mode;

        symlink("/etc/passwd", root.join("link")).unwrap();
        let err = unix_mode(&root.join("link")).await.unwrap_err();
        assert!(
            matches!(err, FsError::SymlinkRejected),
            "expected SymlinkRejected, got {err:?}"
        );
        let err = set_unix_mode(&root.join("link"), 0o600).await.unwrap_err();
        assert!(
            matches!(err, FsError::SymlinkRejected),
            "expected SymlinkRejected, got {err:?}"
        );
    }

    #[tokio::test]
    async fn walk_skips_symlinks_and_can_truncate() {
        let (_dir, root) = canon_temp();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        symlink("/etc/passwd", root.join("link")).unwrap();
        let all = walk_files(&root).await.unwrap();
        let names: Vec<_> = all
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".into()));
        assert!(names.contains(&"b.txt".into()));
        assert!(!names.iter().any(|n| n == "link"));
        assert!(!all.truncated);

        let limited = walk_files_limited(&root, MAX_WALK_DEPTH, MAX_WALK_DIRECTORIES, 1)
            .await
            .unwrap();
        assert!(limited.truncated);
        assert_eq!(limited.files.len(), 1);
    }

    #[tokio::test]
    async fn walk_depth_cap_stops_descent() {
        let (_dir, root) = canon_temp();
        std::fs::create_dir_all(root.join("d0/d1")).unwrap();
        std::fs::write(root.join("top.txt"), "t").unwrap();
        std::fs::write(root.join("d0/mid.txt"), "m").unwrap();
        std::fs::write(root.join("d0/d1/deep.txt"), "d").unwrap();
        let shallow = walk_files_limited(&root, 0, MAX_WALK_DIRECTORIES, MAX_WALK_ENTRIES)
            .await
            .unwrap();
        let names: Vec<_> = shallow
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"top.txt".into()));
        assert!(!names.iter().any(|n| n == "mid.txt" || n == "deep.txt"));
    }
}
