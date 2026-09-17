//! Unix-socket Runner client. Codex types stay out of this crate.
//!
//! `Host` + `UdsRunner` is opt-in **transport** on the same host. It does
//! not claim Linux container isolation. P0 is 1:1: one gateway connection
//! per worker. Replay is connection-local.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use crate::wire::{
    read_frame, write_frame, RunnerEvent, RunnerOp, RunnerOpResult, WireEnvelope, WireKind,
    WIRE_PROTOCOL,
};
use crate::{
    Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerError, RunnerExecRequest,
    RunnerExecResult, RunnerReadProcess, RunnerReadResult, RunnerWriteStdin, ShellRelease,
};

/// Transport deadline for one RPC. Longer than the default exec timeout
/// so `apply_patch` is not cut short; short enough to fail a hung
/// mismatch instead of waiting forever.
pub const RUNNER_CALL_DEADLINE: Duration = Duration::from_secs(120);

pub type DisconnectHook = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    writer: Mutex<tokio::net::unix::OwnedWriteHalf>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<WireEnvelope, RunnerError>>>>,
    alive: AtomicBool,
    on_disconnect: DisconnectHook,
}

pub struct UdsRunner {
    shared: Arc<Shared>,
    next_id: Arc<AtomicU64>,
}

impl Clone for UdsRunner {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            next_id: self.next_id.clone(),
        }
    }
}

impl UdsRunner {
    pub async fn connect(
        path: impl AsRef<Path>,
        on_process_exit: ShellRelease,
    ) -> Result<Self, RunnerError> {
        Self::connect_with_disconnect(path, on_process_exit, Arc::new(|| {})).await
    }

    pub async fn connect_with_disconnect(
        path: impl AsRef<Path>,
        on_process_exit: ShellRelease,
        on_disconnect: DisconnectHook,
    ) -> Result<Self, RunnerError> {
        let stream = UnixStream::connect(path.as_ref())
            .await
            .map_err(|err| RunnerError::before_dispatch(err.to_string()))?;
        let runner = Self::from_stream_with_disconnect(stream, on_process_exit, on_disconnect);
        runner.handshake().await?;
        Ok(runner)
    }

    pub fn from_stream(stream: UnixStream, on_process_exit: ShellRelease) -> Self {
        Self::from_stream_with_disconnect(stream, on_process_exit, Arc::new(|| {}))
    }

    pub fn from_stream_with_disconnect(
        stream: UnixStream,
        on_process_exit: ShellRelease,
        on_disconnect: DisconnectHook,
    ) -> Self {
        let (read, write) = stream.into_split();
        let shared = Arc::new(Shared {
            writer: Mutex::new(write),
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            on_disconnect,
        });
        let reader_shared = shared.clone();
        tokio::spawn(async move {
            read_loop(read, reader_shared, on_process_exit).await;
        });
        Self {
            shared,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub async fn handshake(&self) -> Result<(), RunnerError> {
        match self.call(RunnerOp::Hello).await {
            Ok(RunnerOpResult::Hello { protocol }) if protocol == WIRE_PROTOCOL => Ok(()),
            Ok(other) => {
                self.close().await;
                Err(RunnerError::before_dispatch(format!(
                    "unexpected runner hello result: {other:?}"
                )))
            }
            Err(RunnerError::TransportBeforeDispatch { message }) => {
                Err(RunnerError::before_dispatch(message))
            }
            Err(RunnerError::TransportAmbiguous { message }) => {
                self.close().await;
                Err(RunnerError::before_dispatch(message))
            }
            Err(RunnerError::Execution(body)) => {
                self.close().await;
                Err(RunnerError::before_dispatch(body.message))
            }
        }
    }

    pub async fn close(&self) {
        self.shared.alive.store(false, Ordering::SeqCst);
        {
            let mut writer = self.shared.writer.lock().await;
            let _ = writer.shutdown().await;
        }
        fail_pending(&self.shared, RunnerError::ambiguous("runner socket closed")).await;
    }

    async fn call(&self, op: RunnerOp) -> Result<RunnerOpResult, RunnerError> {
        if !self.shared.alive.load(Ordering::SeqCst) {
            return Err(RunnerError::before_dispatch("runner socket closed"));
        }
        let request_id = format!("rrpc-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        {
            self.shared
                .pending
                .lock()
                .await
                .insert(request_id.clone(), tx);
        }
        let envelope = WireEnvelope::request(request_id.clone(), op);
        {
            let mut writer = self.shared.writer.lock().await;
            if let Err(err) = write_frame(&mut *writer, &envelope).await {
                self.shared.pending.lock().await.remove(&request_id);
                return Err(RunnerError::before_dispatch(err.to_string()));
            }
        }
        match timeout(RUNNER_CALL_DEADLINE, rx).await {
            Ok(Ok(Ok(response))) => decode_response(response),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(RunnerError::ambiguous("runner socket closed")),
            Err(_) => {
                self.close().await;
                Err(RunnerError::ambiguous("runner call deadline exceeded"))
            }
        }
    }
}

async fn read_loop(
    mut read: tokio::net::unix::OwnedReadHalf,
    shared: Arc<Shared>,
    on_process_exit: ShellRelease,
) {
    loop {
        match read_frame(&mut read).await {
            Ok(Some(payload)) => match serde_json::from_slice::<WireEnvelope>(&payload) {
                Ok(envelope) => match envelope.kind {
                    WireKind::Event => {
                        if let Some(RunnerEvent::ProcessExited { process_id }) = envelope.event {
                            on_process_exit(&process_id.0);
                        }
                    }
                    WireKind::Response => {
                        if let Some(id) = envelope.request_id.clone() {
                            if let Some(tx) = shared.pending.lock().await.remove(&id) {
                                let _ = tx.send(Ok(envelope));
                            }
                        }
                    }
                    WireKind::Request => {}
                },
                Err(err) => {
                    fail_pending(
                        &shared,
                        RunnerError::ambiguous(format!("invalid runner rpc: {err}")),
                    )
                    .await;
                    break;
                }
            },
            Ok(None) | Err(_) => {
                fail_pending(&shared, RunnerError::ambiguous("runner socket closed")).await;
                break;
            }
        }
    }
    shared.alive.store(false, Ordering::SeqCst);
    (shared.on_disconnect)();
}

async fn fail_pending(shared: &Shared, err: RunnerError) {
    let mut pending = shared.pending.lock().await;
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(err.clone()));
    }
}

fn decode_response(response: WireEnvelope) -> Result<RunnerOpResult, RunnerError> {
    if response.ok == Some(true) {
        response.result.ok_or_else(|| {
            RunnerError::execution(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "runner rpc missing result",
            ))
        })
    } else {
        Err(RunnerError::execution(response.error.unwrap_or_else(
            || ErrorBody::new(ErrorCode::InvalidPatch, "runner rpc failed"),
        )))
    }
}

fn unexpected(result: RunnerOpResult) -> RunnerError {
    RunnerError::execution(ErrorBody::new(
        ErrorCode::InvalidPatch,
        format!("unexpected runner rpc result: {result:?}"),
    ))
}

impl Runner for UdsRunner {
    async fn read(&self, ws: &Workspace, path: &str) -> Result<ReadResult, RunnerError> {
        match self
            .call(RunnerOp::Read {
                workspace: ws.clone(),
                path: path.to_string(),
            })
            .await?
        {
            RunnerOpResult::Read(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn find(&self, ws: &Workspace, glob: Option<&str>) -> Result<FindResult, RunnerError> {
        match self
            .call(RunnerOp::Find {
                workspace: ws.clone(),
                glob: glob.map(str::to_string),
            })
            .await?
        {
            RunnerOpResult::Find(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn version(&self, ws: &Workspace, path: &str) -> Result<String, RunnerError> {
        match self
            .call(RunnerOp::Version {
                workspace: ws.clone(),
                path: path.to_string(),
            })
            .await?
        {
            RunnerOpResult::Version(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, RunnerError> {
        match self
            .call(RunnerOp::ApplyPatch {
                workspace: ws.clone(),
                request: req,
            })
            .await?
        {
            RunnerOpResult::ApplyPatch(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, RunnerError> {
        match self
            .call(RunnerOp::Exec {
                workspace: ws.clone(),
                request: req,
            })
            .await?
        {
            RunnerOpResult::Exec(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), RunnerError> {
        match self.call(RunnerOp::WriteStdin { request: req }).await? {
            RunnerOpResult::WriteStdin => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    async fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, RunnerError> {
        match self.call(RunnerOp::ReadProcess { request: req }).await? {
            RunnerOpResult::ReadProcess(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), RunnerError> {
        match self
            .call(RunnerOp::Terminate {
                process_id: process_id.clone(),
            })
            .await?
        {
            RunnerOpResult::Terminate => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    async fn workspace_of(&self, process_id: &str) -> Option<String> {
        match self
            .call(RunnerOp::WorkspaceOf {
                process_id: process_id.to_string(),
            })
            .await
            .ok()?
        {
            RunnerOpResult::WorkspaceOf(value) => value,
            _ => None,
        }
    }

    async fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, RunnerError> {
        match self
            .call(RunnerOp::TerminateWorkspace {
                workspace_id: workspace_id.to_string(),
            })
            .await?
        {
            RunnerOpResult::TerminateWorkspace(count) => Ok(count),
            other => Err(unexpected(other)),
        }
    }
}
