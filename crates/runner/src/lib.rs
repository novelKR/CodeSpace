//! `Runner` trait, host `InProcessRunner`, and opt-in Unix-socket
//! `ContainerRunner`. Path sandbox, one patch transaction, host process
//! supervisor, and isolation-fixture checks. Linux containers are the
//! **target** execution OS. macOS hosts may run the path sandbox for unit
//! tests; that does **not** verify Linux isolation. `exec_command` is not
//! dispatched into compose. Default backend remains in-process.

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
mod container;
mod files;
mod patch_helper;
mod patch_verify;
mod process;
mod rollback;
mod wire;

pub use api::{
    default_exec_timeout_ms, default_host_exec_env, RunnerApplyPatchRequest,
    RunnerApplyPatchResult, RunnerExecPolicy, RunnerExecRequest, RunnerExecResult,
    RunnerReadProcess, RunnerReadResult, RunnerWriteStdin, DEFAULT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
pub use container::ContainerRunner;
pub use files::{DEFAULT_FIND_LIMIT, DEFAULT_READ_LIMIT, VERSION_ABSENT};
pub use patch_helper::ensure_helper_for_tests;
pub use process::{
    InProcessRunner, RetentionPolicy, ShellRelease, DEFAULT_COMPLETED_TTL, DEFAULT_MAX_COMPLETED,
    DEFAULT_MAX_PROCESSES, DEFAULT_TIMEOUT,
};
pub use wire::{serve_runner_connection, RunnerOp, RunnerOpResult, WireRequest, WireResponse};

/// Execution-plane API. Control-plane fields (`work_id`, coordination,
/// `operation_id` / `operation_key`) stay in the gateway.
pub trait Runner: Send + Sync {
    fn read(
        &self,
        ws: &Workspace,
        path: &str,
    ) -> impl std::future::Future<Output = Result<ReadResult, ErrorBody>> + Send;
    fn find(
        &self,
        ws: &Workspace,
        glob: Option<&str>,
    ) -> impl std::future::Future<Output = Result<FindResult, ErrorBody>> + Send;
    fn version(
        &self,
        ws: &Workspace,
        path: &str,
    ) -> impl std::future::Future<Output = Result<String, ErrorBody>> + Send;
    fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> impl std::future::Future<Output = Result<RunnerApplyPatchResult, ErrorBody>> + Send;
    fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> impl std::future::Future<Output = Result<RunnerExecResult, ErrorBody>> + Send;
    fn write_stdin(
        &self,
        req: RunnerWriteStdin,
    ) -> impl std::future::Future<Output = Result<(), ErrorBody>> + Send;
    fn read_process(
        &self,
        req: RunnerReadProcess,
    ) -> impl std::future::Future<Output = Result<RunnerReadResult, ErrorBody>> + Send;
    fn terminate(
        &self,
        process_id: &ProcessId,
    ) -> impl std::future::Future<Output = Result<(), ErrorBody>> + Send;
    fn workspace_of(
        &self,
        process_id: &str,
    ) -> impl std::future::Future<Output = Option<String>> + Send;
    fn terminate_workspace(
        &self,
        workspace_id: &str,
    ) -> impl std::future::Future<Output = Result<u32, ErrorBody>> + Send;
}

impl Runner for InProcessRunner {
    async fn read(&self, ws: &Workspace, path: &str) -> Result<ReadResult, ErrorBody> {
        self.read_file(ws, path)
    }

    async fn find(&self, ws: &Workspace, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        self.find_files(ws, glob)
    }

    async fn version(&self, ws: &Workspace, path: &str) -> Result<String, ErrorBody> {
        self.file_version(ws, path)
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, ErrorBody> {
        self.apply_patch_txn(ws, req).await
    }

    async fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, ErrorBody> {
        self.spawn_host(ws, req)
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), ErrorBody> {
        self.write_host_stdin(req).await
    }

    async fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody> {
        self.read_host_process(req)
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), ErrorBody> {
        self.kill_host(process_id)
    }

    async fn workspace_of(&self, process_id: &str) -> Option<String> {
        self.host_workspace_of(process_id)
    }

    async fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        self.kill_host_workspace(workspace_id)
    }
}

#[derive(Clone)]
pub enum RuntimeBackend {
    InProcess(InProcessRunner),
    Container(ContainerRunner),
}

impl RuntimeBackend {
    pub fn in_process(on_release: ShellRelease) -> Self {
        Self::InProcess(InProcessRunner::new(on_release))
    }
}

impl Runner for RuntimeBackend {
    async fn read(&self, ws: &Workspace, path: &str) -> Result<ReadResult, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.read(ws, path).await,
            Self::Container(runner) => runner.read(ws, path).await,
        }
    }

    async fn find(&self, ws: &Workspace, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.find(ws, glob).await,
            Self::Container(runner) => runner.find(ws, glob).await,
        }
    }

    async fn version(&self, ws: &Workspace, path: &str) -> Result<String, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.version(ws, path).await,
            Self::Container(runner) => runner.version(ws, path).await,
        }
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.apply_patch(ws, req).await,
            Self::Container(runner) => runner.apply_patch(ws, req).await,
        }
    }

    async fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.exec(ws, req).await,
            Self::Container(runner) => runner.exec(ws, req).await,
        }
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.write_stdin(req).await,
            Self::Container(runner) => runner.write_stdin(req).await,
        }
    }

    async fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.read_process(req).await,
            Self::Container(runner) => runner.read_process(req).await,
        }
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.terminate(process_id).await,
            Self::Container(runner) => runner.terminate(process_id).await,
        }
    }

    async fn workspace_of(&self, process_id: &str) -> Option<String> {
        match self {
            Self::InProcess(runner) => runner.workspace_of(process_id).await,
            Self::Container(runner) => runner.workspace_of(process_id).await,
        }
    }

    async fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        match self {
            Self::InProcess(runner) => runner.terminate_workspace(workspace_id).await,
            Self::Container(runner) => runner.terminate_workspace(workspace_id).await,
        }
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
        let sandbox = PathSandbox::new(Workspace::new(
            WorkspaceId("demo".into()),
            root.join("ws"),
            Profile::ReadOnly,
        ));
        let err = sandbox.resolve("link").unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkRejected);
    }

    #[test]
    fn path_sandbox_allows_regular_file() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(ws.join("readme.txt"), "ok").unwrap();
        let sandbox = PathSandbox::new(Workspace::new(
            WorkspaceId("demo".into()),
            ws,
            Profile::ReadOnly,
        ));
        let path = sandbox.resolve("readme.txt").unwrap();
        assert!(path.ends_with("readme.txt"));
    }
}
