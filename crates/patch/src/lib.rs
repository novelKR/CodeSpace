//! Codex V4A adapter. Parses and applies with the original crate in-process.
//! Does not wrap the standalone `apply_patch` binary and does not call `git apply`.

use std::path::{Path, PathBuf};

use codespace_domain::{ErrorBody, ErrorCode};
use codespace_policy::{resolve_path, Workspace};
use codex_apply_patch::{
    apply_patch_with_options, parse_patch, ApplyPatchFileUpdateMode, ApplyPatchOptions, Hunk,
};
use codex_exec_server::LOCAL_FS;
use codex_utils_path_uri::PathUri;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub files: Vec<String>,
}

pub fn parse_and_policy(workspace: &Workspace, patch: &str) -> Result<Vec<PathBuf>, ErrorBody> {
    let parsed = parse_patch(patch)
        .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
    let mut files = Vec::new();
    for hunk in &parsed.hunks {
        match hunk {
            Hunk::AddFile { path, .. } => {
                let dest = resolve_path(workspace, path_str(path)?)?;
                reject_symlink_ancestors(workspace, &dest)?;
                if dest.exists() {
                    return Err(ErrorBody::new(
                        ErrorCode::AddFileExists,
                        format!("{} already exists", path.display()),
                    ));
                }
                files.push(dest);
            }
            Hunk::DeleteFile { path } => {
                let dest = resolve_path(workspace, path_str(path)?)?;
                reject_symlink_ancestors(workspace, &dest)?;
                files.push(dest);
            }
            Hunk::UpdateFile {
                path, move_path, ..
            } => {
                let dest = resolve_path(workspace, path_str(path)?)?;
                reject_symlink_ancestors(workspace, &dest)?;
                files.push(dest);
                if let Some(dest) = move_path {
                    let dest_path = resolve_path(workspace, path_str(dest)?)?;
                    reject_symlink_ancestors(workspace, &dest_path)?;
                    if dest_path.exists() {
                        return Err(ErrorBody::new(
                            ErrorCode::MoveDestinationExists,
                            format!("{} already exists", dest.display()),
                        ));
                    }
                    files.push(dest_path);
                }
            }
        }
    }
    Ok(files)
}

pub async fn apply_in_workspace(
    workspace: &Workspace,
    patch: &str,
    check_only: bool,
) -> Result<ApplyOutcome, ErrorBody> {
    let files = parse_and_policy(workspace, patch)?;
    // Codex no-follow I/O walks from `/` with O_NOFOLLOW. On macOS the default
    // temp path goes through `/var` → `/private/var`, so cwd must be canonical.
    let root = workspace
        .root
        .canonicalize()
        .map_err(|err| ErrorBody::new(ErrorCode::PathEscape, err.to_string()))?;
    if check_only {
        return Ok(ApplyOutcome {
            files: relative_files(&root, &files),
        });
    }

    let cwd = PathUri::from_host_native_path(&root)
        .map_err(|err| ErrorBody::new(ErrorCode::PathEscape, err.to_string()))?;
    let options = ApplyPatchOptions {
        update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
        follow_symlinks: false,
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    apply_patch_with_options(
        patch,
        options,
        &cwd,
        &mut stdout,
        &mut stderr,
        LOCAL_FS.as_ref(),
        None,
    )
    .await
    .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;

    Ok(ApplyOutcome {
        files: relative_files(&root, &files),
    })
}

fn relative_files(root: &Path, files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

fn path_str(path: &Path) -> Result<&str, ErrorBody> {
    path.to_str()
        .ok_or_else(|| ErrorBody::new(ErrorCode::PathEscape, "path is not UTF-8"))
}

fn reject_symlink_ancestors(workspace: &Workspace, dest: &Path) -> Result<(), ErrorBody> {
    let root = workspace
        .root
        .canonicalize()
        .unwrap_or_else(|_| workspace.root.clone());
    let mut current = dest;
    loop {
        if current == root {
            break;
        }
        if let Ok(meta) = std::fs::symlink_metadata(current) {
            if meta.file_type().is_symlink() {
                return Err(ErrorBody::new(
                    ErrorCode::SymlinkRejected,
                    format!("{} is a symlink", current.display()),
                ));
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{Profile, WorkspaceId};
    use tempfile::tempdir;

    fn ws(dir: &Path) -> Workspace {
        Workspace {
            id: WorkspaceId("demo".into()),
            root: dir.to_path_buf(),
            profile: Profile::WorkspaceWrite,
        }
    }

    #[test]
    fn parse_rejects_garbage_without_git_apply() {
        let dir = tempdir().unwrap();
        let err = parse_and_policy(&ws(dir.path()), "not a patch").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPatch);
    }

    #[test]
    fn parse_rejects_path_escape() {
        let dir = tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Add File: ../escape.txt\n+nope\n*** End Patch\n";
        let err = parse_and_policy(&ws(dir.path()), patch).unwrap_err();
        assert_eq!(err.code, ErrorCode::PathEscape);
    }

    #[test]
    fn add_file_existing_is_rejected() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "old").unwrap();
        let patch = "*** Begin Patch\n*** Add File: hello.txt\n+new\n*** End Patch\n";
        let err = parse_and_policy(&ws(dir.path()), patch).unwrap_err();
        assert_eq!(err.code, ErrorCode::AddFileExists);
    }

    #[tokio::test]
    async fn apply_add_file_matches_disk() {
        let dir = tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** End Patch\n";
        let outcome = apply_in_workspace(&ws(dir.path()), patch, false)
            .await
            .unwrap();
        assert!(outcome.files.iter().any(|f| f.ends_with("nested/new.txt")));
        let body = std::fs::read_to_string(dir.path().join("nested/new.txt")).unwrap();
        assert_eq!(body, "created\n");
    }

    #[tokio::test]
    async fn check_only_does_not_write() {
        let dir = tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Add File: only.txt\n+x\n*** End Patch\n";
        apply_in_workspace(&ws(dir.path()), patch, true)
            .await
            .unwrap();
        assert!(!dir.path().join("only.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_does_not_follow_symlinks() {
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("work")).unwrap();
        std::fs::create_dir(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/secret.txt"), "secret\n").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("work/linked")).unwrap();
        let patch = "*** Begin Patch\n*** Add File: linked/pwned.txt\n+nope\n*** End Patch\n";
        let err = apply_in_workspace(&ws(&root.join("work")), patch, false)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkRejected);
        assert!(!root.join("outside/pwned.txt").exists());
    }
}
