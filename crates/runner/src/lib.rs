//! `Runner` trait, host `InProcessRunner`, and opt-in Unix-socket
//! `UdsRunner`. Path sandbox, one patch transaction, host process
//! supervisor, and isolation-fixture checks. Linux containers are the
//! **target** execution OS. When the Linux helper probe succeeds, command
//! spawn is wrapped by the `codespace-linux-sandbox` helper process.
//! macOS hosts may run the
//! path sandbox for unit tests; that does **not** verify Linux isolation.
//! `exec_command` is not dispatched into compose. Default backend remains
//! in-process. Host + `UdsRunner` is the same host over UDS, not a Linux
//! isolation claim.

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

/// Workspace path authorizer. `resolve` keeps `resolve_path` plus leaf
/// and ancestor symlink/special-file rejection. That pre-check is not
/// race-proof I/O. File bytes, metadata, mkdir, chmod, remove, and
/// bounded walks go through `codespace-fs` (no-follow operations).
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
        reject_symlink_ancestors(self.root(), &path)?;
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

fn reject_symlink_ancestors(root: &Path, dest: &Path) -> Result<(), ErrorBody> {
    // Registration root is the trust anchor; canonicalize the root only.
    // User-controlled descendants below it must not be followed.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut current = dest;
    loop {
        if current == root {
            break;
        }
        if let Ok(meta) = fs::symlink_metadata(current) {
            if meta.file_type().is_symlink() {
                return Err(ErrorBody::new(
                    ErrorCode::SymlinkRejected,
                    "symlink files are rejected",
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

mod api;
mod apply;
mod files;
mod linux_sandbox;
mod patch_helper;
mod patch_verify;
mod process;
mod rollback;
mod socket;
mod uds;
mod watch;
mod wire;

pub use api::{
    default_exec_timeout_ms, runner_local_exec_env, RunnerApplyPatchRequest,
    RunnerApplyPatchResult, RunnerCwd, RunnerError, RunnerExecEnv, RunnerExecPolicy,
    RunnerExecRequest, RunnerExecResult, RunnerProcessStatus, RunnerReadProcess, RunnerReadResult,
    RunnerResizeResult, RunnerWriteStdin, DEFAULT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
pub use codespace_domain::{DEFAULT_FIND_LIMIT, DEFAULT_READ_LIMIT};
pub use files::VERSION_ABSENT;
pub use patch_helper::ensure_helper_for_tests;
pub use process::{
    InProcessRunner, RetentionPolicy, ShellRelease, DEFAULT_COMPLETED_TTL, DEFAULT_MAX_COMPLETED,
    DEFAULT_MAX_PROCESSES, DEFAULT_TIMEOUT,
};

/// True when this process probed a working Linux sandbox helper.
/// Advertisement and spawn must agree on this value.
pub fn linux_sandbox_available() -> bool {
    linux_sandbox::probe()
}

pub use socket::{
    allocate_private_runner_dir, is_forbidden_runner_dir, reclaim_leftover_socket,
    runner_socket_path, RUNNER_SOCKET_NAME,
};
pub use uds::{DisconnectHook, UdsRunner, RUNNER_CALL_DEADLINE};
pub use watch::{FsWatchEvent, FsWatchKind, WatchClosed, WatchSubscription};
pub use wire::{
    host_worker, read_frame, serve_runner_connection, write_frame, RunnerEvent, RunnerOp,
    RunnerOpResult, WireEnvelope, WireKind, WIRE_PROTOCOL,
};

/// Execution-plane API. Control-plane fields (`work_id`, coordination,
/// `operation_id` / `operation_key`) stay in the gateway.
pub trait Runner: Send + Sync {
    fn read(
        &self,
        ws: &Workspace,
        path: &str,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> impl std::future::Future<Output = Result<ReadResult, RunnerError>> + Send;
    fn find(
        &self,
        ws: &Workspace,
        glob: Option<&str>,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> impl std::future::Future<Output = Result<FindResult, RunnerError>> + Send;
    fn version(
        &self,
        ws: &Workspace,
        path: &str,
    ) -> impl std::future::Future<Output = Result<String, RunnerError>> + Send;
    fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> impl std::future::Future<Output = Result<RunnerApplyPatchResult, RunnerError>> + Send;
    fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> impl std::future::Future<Output = Result<RunnerExecResult, RunnerError>> + Send;
    fn write_stdin(
        &self,
        req: RunnerWriteStdin,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;
    fn read_process(
        &self,
        req: RunnerReadProcess,
    ) -> impl std::future::Future<Output = Result<RunnerReadResult, RunnerError>> + Send;
    fn process_status(
        &self,
        process_id: &ProcessId,
    ) -> impl std::future::Future<Output = Result<RunnerProcessStatus, RunnerError>> + Send;
    fn resize(
        &self,
        process_id: &ProcessId,
        rows: u16,
        cols: u16,
    ) -> impl std::future::Future<Output = Result<RunnerResizeResult, RunnerError>> + Send;
    fn terminate(
        &self,
        process_id: &ProcessId,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;
    fn workspace_of(
        &self,
        process_id: &str,
    ) -> impl std::future::Future<Output = Option<String>> + Send;
    fn terminate_workspace(
        &self,
        workspace_id: &str,
    ) -> impl std::future::Future<Output = Result<u32, RunnerError>> + Send;
}

impl Runner for InProcessRunner {
    async fn read(
        &self,
        ws: &Workspace,
        path: &str,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ReadResult, RunnerError> {
        self.read_file(ws, path, offset, limit)
            .await
            .map_err(RunnerError::from)
    }

    async fn find(
        &self,
        ws: &Workspace,
        glob: Option<&str>,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<FindResult, RunnerError> {
        self.find_files(ws, glob, offset, limit)
            .await
            .map_err(RunnerError::from)
    }

    async fn version(&self, ws: &Workspace, path: &str) -> Result<String, RunnerError> {
        self.file_version(ws, path).await.map_err(RunnerError::from)
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, RunnerError> {
        self.apply_patch_txn(ws, req)
            .await
            .map_err(RunnerError::from)
    }

    async fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, RunnerError> {
        self.spawn_host(ws, req).await.map_err(RunnerError::from)
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), RunnerError> {
        self.write_host_stdin(req).await.map_err(RunnerError::from)
    }

    async fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, RunnerError> {
        self.read_host_process(req).map_err(RunnerError::from)
    }

    async fn process_status(
        &self,
        process_id: &ProcessId,
    ) -> Result<RunnerProcessStatus, RunnerError> {
        self.host_process_status(process_id)
            .map_err(RunnerError::from)
    }

    async fn resize(
        &self,
        process_id: &ProcessId,
        rows: u16,
        cols: u16,
    ) -> Result<RunnerResizeResult, RunnerError> {
        self.host_resize(process_id, rows, cols)
            .map_err(RunnerError::from)
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), RunnerError> {
        self.kill_host(process_id).map_err(RunnerError::from)
    }

    async fn workspace_of(&self, process_id: &str) -> Option<String> {
        self.host_workspace_of(process_id)
    }

    async fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, RunnerError> {
        self.kill_host_workspace(workspace_id)
            .map_err(RunnerError::from)
    }
}

#[derive(Clone)]
pub enum RuntimeBackend {
    InProcess(InProcessRunner),
    Uds(UdsRunner),
}

impl RuntimeBackend {
    pub fn in_process(on_release: ShellRelease) -> Self {
        Self::InProcess(InProcessRunner::new(on_release))
    }
}

impl Runner for RuntimeBackend {
    async fn read(
        &self,
        ws: &Workspace,
        path: &str,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ReadResult, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.read(ws, path, offset, limit).await,
            Self::Uds(runner) => runner.read(ws, path, offset, limit).await,
        }
    }

    async fn find(
        &self,
        ws: &Workspace,
        glob: Option<&str>,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<FindResult, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.find(ws, glob, offset, limit).await,
            Self::Uds(runner) => runner.find(ws, glob, offset, limit).await,
        }
    }

    async fn version(&self, ws: &Workspace, path: &str) -> Result<String, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.version(ws, path).await,
            Self::Uds(runner) => runner.version(ws, path).await,
        }
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.apply_patch(ws, req).await,
            Self::Uds(runner) => runner.apply_patch(ws, req).await,
        }
    }

    async fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.exec(ws, req).await,
            Self::Uds(runner) => runner.exec(ws, req).await,
        }
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), RunnerError> {
        match self {
            Self::InProcess(runner) => runner.write_stdin(req).await,
            Self::Uds(runner) => runner.write_stdin(req).await,
        }
    }

    async fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.read_process(req).await,
            Self::Uds(runner) => runner.read_process(req).await,
        }
    }

    async fn process_status(
        &self,
        process_id: &ProcessId,
    ) -> Result<RunnerProcessStatus, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.process_status(process_id).await,
            Self::Uds(runner) => runner.process_status(process_id).await,
        }
    }

    async fn resize(
        &self,
        process_id: &ProcessId,
        rows: u16,
        cols: u16,
    ) -> Result<RunnerResizeResult, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.resize(process_id, rows, cols).await,
            Self::Uds(runner) => runner.resize(process_id, rows, cols).await,
        }
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), RunnerError> {
        match self {
            Self::InProcess(runner) => runner.terminate(process_id).await,
            Self::Uds(runner) => runner.terminate(process_id).await,
        }
    }

    async fn workspace_of(&self, process_id: &str) -> Option<String> {
        match self {
            Self::InProcess(runner) => runner.workspace_of(process_id).await,
            Self::Uds(runner) => runner.workspace_of(process_id).await,
        }
    }

    async fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, RunnerError> {
        match self {
            Self::InProcess(runner) => runner.terminate_workspace(workspace_id).await,
            Self::Uds(runner) => runner.terminate_workspace(workspace_id).await,
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
    fn path_sandbox_rejects_directory_symlink_ancestors() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("ws")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "leak").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("ws").join("via")).unwrap();
        let sandbox = PathSandbox::new(Workspace::new(
            WorkspaceId("demo".into()),
            root.join("ws"),
            Profile::ReadOnly,
        ));
        let err = sandbox.resolve("via/secret.txt").unwrap_err();
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

    #[test]
    fn advertised_pty_size_matches_adapter() {
        assert_eq!(
            codespace_domain::PTY_INITIAL_ROWS,
            codespace_pty::DEFAULT_ROWS
        );
        assert_eq!(
            codespace_domain::PTY_INITIAL_COLS,
            codespace_pty::DEFAULT_COLS
        );
    }
}
