//! Best-effort file rollback. Restores content, existence, and permission
//! bits. Does not call `git reset --hard` and does not rewrite directories.

use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::PathSandbox;
use codespace_domain::{ErrorBody, ErrorCode};
use codespace_fs::FsError;

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub relative: String,
    pub existed: bool,
    pub content: Option<Vec<u8>>,
    pub mode: Option<u32>,
}

pub async fn snapshot(
    sandbox: &PathSandbox,
    relatives: &[String],
) -> Result<Vec<FileSnapshot>, ErrorBody> {
    let mut out = Vec::with_capacity(relatives.len());
    for relative in relatives {
        let path = sandbox.resolve(relative)?;
        let (existed, content, mode) = match codespace_fs::metadata(&path).await {
            Ok(meta) if meta.is_file && !meta.is_symlink => {
                let bytes = codespace_fs::read(&path).await.map_err(fs_io)?;
                (true, Some(bytes), file_mode(&path))
            }
            Ok(_) | Err(FsError::NotFound) => (false, None, None),
            Err(err) => return Err(fs_io(err)),
        };
        out.push(FileSnapshot {
            relative: relative.clone(),
            existed,
            content,
            mode,
        });
    }
    Ok(out)
}

/// Restore snapshots. Incomplete restore is reported as `false`.
pub async fn restore(sandbox: &PathSandbox, snaps: &[FileSnapshot]) -> bool {
    let mut complete = true;
    for snap in snaps {
        if restore_one(sandbox, snap).await.is_err() {
            complete = false;
        }
    }
    complete
}

async fn restore_one(sandbox: &PathSandbox, snap: &FileSnapshot) -> Result<(), ErrorBody> {
    let path = sandbox.resolve(&snap.relative)?;
    if snap.existed {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
        }
        let content = snap.content.as_deref().unwrap_or(&[]);
        codespace_fs::write(&path, content).await.map_err(fs_io)?;
        if let Some(mode) = snap.mode {
            set_mode(&path, mode);
        }
    } else {
        match codespace_fs::metadata(&path).await {
            Ok(meta) if meta.is_file && !meta.is_symlink => {
                codespace_fs::remove_file(&path).await.map_err(fs_io)?;
            }
            Ok(_) | Err(FsError::NotFound) => {}
            Err(err) => return Err(fs_io(err)),
        }
    }
    Ok(())
}

fn file_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        fs::metadata(path)
            .ok()
            .map(|meta| meta.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        let mut perms = match fs::metadata(path) {
            Ok(meta) => meta.permissions(),
            Err(_) => return,
        };
        perms.set_mode(mode);
        let _ = fs::set_permissions(path, perms);
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn io_err(err: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidPatch, err.to_string())
}

fn fs_io(err: FsError) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidPatch, err.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use tempfile::tempdir;

    fn sandbox(dir: &std::path::Path) -> PathSandbox {
        PathSandbox::new(Workspace::new(
            WorkspaceId("demo".into()),
            dir.to_path_buf(),
            Profile::WorkspaceWrite,
        ))
    }

    #[tokio::test]
    async fn restore_deletes_added_file_and_keeps_dirs() {
        let dir = tempdir().unwrap();
        let s = sandbox(dir.path());
        let snaps = snapshot(&s, &["nested/new.txt".into()]).await.unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/new.txt"), "x").unwrap();
        assert!(restore(&s, &snaps).await);
        assert!(!dir.path().join("nested/new.txt").exists());
        assert!(dir.path().join("nested").is_dir());
    }

    #[tokio::test]
    async fn restore_puts_back_updated_bytes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let s = sandbox(dir.path());
        let snaps = snapshot(&s, &["a.txt".into()]).await.unwrap();
        std::fs::write(dir.path().join("a.txt"), "new").unwrap();
        assert!(restore(&s, &snaps).await);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "old"
        );
    }
}
