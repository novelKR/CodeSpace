//! In-process `Runner`: path sandbox, one patch transaction, host process
//! supervisor, and isolation-fixture checks. Linux containers are the
//! **target** execution OS. macOS hosts may run the path sandbox for unit
//! tests; that does **not** verify Linux isolation. `exec_command` is not
//! dispatched into compose.

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
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

mod api;
mod apply;
mod files;
mod patch_helper;
mod patch_verify;
mod process;
mod rollback;

pub use api::{
    RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerExecRequest, RunnerExecResult,
    RunnerReadProcess, RunnerReadResult, RunnerWriteStdin,
};
pub use files::{DEFAULT_FIND_LIMIT, DEFAULT_READ_LIMIT, VERSION_ABSENT};
pub use patch_helper::ensure_helper_for_tests;
pub use process::{
    InProcessRunner, RetentionPolicy, ShellRelease, DEFAULT_COMPLETED_TTL, DEFAULT_MAX_COMPLETED,
    DEFAULT_MAX_PROCESSES, DEFAULT_TIMEOUT, MAX_OUTPUT_BYTES,
};

/// Execution-plane API. Control-plane fields (`work_id`, coordination,
/// `operation_id` / `operation_key`) stay in the gateway.
pub trait Runner: Send + Sync {
    fn read(&self, ws: &Workspace, path: &str) -> Result<ReadResult, ErrorBody>;
    fn find(&self, ws: &Workspace, glob: Option<&str>) -> Result<FindResult, ErrorBody>;
    fn version(&self, ws: &Workspace, path: &str) -> Result<String, ErrorBody>;
    fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> impl std::future::Future<Output = Result<RunnerApplyPatchResult, ErrorBody>> + Send;
    fn exec(&self, ws: &Workspace, req: RunnerExecRequest) -> Result<RunnerExecResult, ErrorBody>;
    fn write_stdin(
        &self,
        req: RunnerWriteStdin,
    ) -> impl std::future::Future<Output = Result<(), ErrorBody>> + Send;
    fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody>;
    fn terminate(&self, process_id: &ProcessId) -> Result<(), ErrorBody>;
    fn workspace_of(&self, process_id: &str) -> Option<String>;
    fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody>;
}

impl Runner for InProcessRunner {
    fn read(&self, ws: &Workspace, path: &str) -> Result<ReadResult, ErrorBody> {
        self.read_file(ws, path)
    }

    fn find(&self, ws: &Workspace, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        self.find_files(ws, glob)
    }

    fn version(&self, ws: &Workspace, path: &str) -> Result<String, ErrorBody> {
        self.file_version(ws, path)
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, ErrorBody> {
        self.apply_patch_txn(ws, req).await
    }

    fn exec(&self, ws: &Workspace, req: RunnerExecRequest) -> Result<RunnerExecResult, ErrorBody> {
        self.spawn_host(ws, req)
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), ErrorBody> {
        self.write_host_stdin(req).await
    }

    fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody> {
        self.read_host_process(req)
    }

    fn terminate(&self, process_id: &ProcessId) -> Result<(), ErrorBody> {
        self.kill_host(process_id)
    }

    fn workspace_of(&self, process_id: &str) -> Option<String> {
        self.host_workspace_of(process_id)
    }

    fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        self.kill_host_workspace(workspace_id)
    }
}

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
