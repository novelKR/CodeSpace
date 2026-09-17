//! Path sandbox, in-process host process supervisor, and isolation-fixture
//! checks. Linux containers are the **target** execution OS. macOS hosts
//! may run the path sandbox for unit tests; that does **not** verify Linux
//! isolation. `exec_command` is not dispatched into compose.

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use codespace_domain::{ErrorBody, ErrorCode};
use codespace_policy::{resolve_path, Workspace};

/// Bind-mounts that must never appear in the runner container.
pub const FORBIDDEN_MOUNT_MARKERS: &[&str] = &[
    "/var/run/docker.sock",
    "/run/host-services/ssh-auth.sock",
    "${HOME}",
    "$HOME",
    "~/.ssh",
    ".env",
    "CODESPACE_HTTP_TOKEN",
    "*.sqlite",
];

pub const RUNNER_UID: u32 = 10001;
pub const WORKSPACE_MOUNT: &str = "/workspace";

#[derive(Debug, Clone)]
pub struct PathSandbox {
    workspace: Workspace,
}

impl PathSandbox {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf, ErrorBody> {
        let path = resolve_path(&self.workspace, relative)?;
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(ErrorBody::new(
                    ErrorCode::SymlinkRejected,
                    "symlink files are rejected",
                ));
            }
            if meta.file_type().is_fifo()
                || meta.file_type().is_socket()
                || meta.file_type().is_block_device()
                || meta.file_type().is_char_device()
            {
                return Err(ErrorBody::new(
                    ErrorCode::SpecialFileRejected,
                    "special files are rejected",
                ));
            }
        }
        Ok(path)
    }

    pub fn root(&self) -> &Path {
        &self.workspace.root
    }
}

mod files;

pub use files::{DEFAULT_FIND_LIMIT, DEFAULT_READ_LIMIT, VERSION_ABSENT};

pub fn compose_allows_marker(compose: &str, marker: &str) -> bool {
    compose.contains(marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{Profile, WorkspaceId};
    use tempfile::tempdir;

    #[test]
    fn deploy_compose_omits_forbidden_mounts() {
        let compose = include_str!("../../../deploy/compose.yml");
        let dockerfile = include_str!("../../../deploy/Dockerfile");
        for marker in FORBIDDEN_MOUNT_MARKERS {
            assert!(
                !compose_allows_marker(compose, marker),
                "compose must not mention {marker}"
            );
        }
        assert!(compose.contains("10001"));
        assert!(compose.contains(WORKSPACE_MOUNT));
        assert!(dockerfile.contains("USER codespace") || dockerfile.contains("10001"));
    }

    #[test]
    fn path_sandbox_rejects_symlink() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("ws")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("ws").join("link")).unwrap();
        let sandbox = PathSandbox::new(Workspace {
            id: WorkspaceId("demo".into()),
            root: root.join("ws"),
            profile: Profile::ReadOnly,
        });
        let err = sandbox.resolve("link").unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkRejected);
    }

    #[test]
    fn path_sandbox_allows_regular_file() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(ws.join("readme.txt"), "ok").unwrap();
        let sandbox = PathSandbox::new(Workspace {
            id: WorkspaceId("demo".into()),
            root: ws,
            profile: Profile::ReadOnly,
        });
        let path = sandbox.resolve("readme.txt").unwrap();
        assert!(path.ends_with("readme.txt"));
    }
}
