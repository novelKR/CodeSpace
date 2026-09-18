//! Isolated filesystem adapter. Wraps `LOCAL_FS` (`ExecutorFileSystem`) and
//! exposes only CodeSpace-owned types. PathSandbox stays the authorizer.
//! Codex sandbox context and `PermissionProfile` stay inside this crate.

use std::io;
use std::path::{Path, PathBuf};

use codex_exec_server::GetMetadataOptions;
use codex_exec_server::ReadFileOptions;
use codex_exec_server::RemoveOptions;
use codex_exec_server::WalkEntryKind;
use codex_exec_server::WalkOptions;
use codex_exec_server::WriteFileOptions;
use codex_exec_server::LOCAL_FS;
use codex_utils_path_uri::PathUri;

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
    Other(String),
}

impl FsError {
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "file not found".into(),
            Self::Other(msg) => msg.clone(),
        }
    }
}

fn uri(path: &Path) -> Result<PathUri, FsError> {
    PathUri::from_host_native_path(path).map_err(|err| FsError::Other(err.to_string()))
}

fn map_io(err: io::Error) -> FsError {
    if err.kind() == io::ErrorKind::NotFound {
        FsError::NotFound
    } else {
        FsError::Other(err.to_string())
    }
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
            !matches!(err, FsError::NotFound),
            "should fail as I/O, not missing: {err:?}"
        );
        let msg = err.message().to_ascii_lowercase();
        assert!(
            msg.contains("symbolic link")
                || msg.contains("symlink")
                || msg.contains("not a directory"),
            "{msg}"
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
