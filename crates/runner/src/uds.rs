//! Unix-socket Runner client. Codex types stay out of this crate.
//!
//! `Host` + `UdsRunner` is opt-in **transport** on the same host. It does
//! not claim Linux container isolation. P0 is 1:1: one gateway connection
//! per worker. Replay is connection-local.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use crate::wire::{
    read_frame, write_frame, RunnerEvent, RunnerOp, RunnerOpResult, WireEnvelope, WireKind,
    WIRE_PROTOCOL,
};
use crate::{
    Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerError, RunnerExecRequest,
    RunnerExecResult, RunnerProcessStatus, RunnerReadProcess, RunnerReadResult, RunnerWriteStdin,
    ShellRelease,
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
        let write_result = {
            let mut writer = self.shared.writer.lock().await;
            write_envelope(&mut *writer, &envelope).await
        };
        if let Err(err) = write_result {
            self.shared.pending.lock().await.remove(&request_id);
            if matches!(err, RunnerError::TransportAmbiguous { .. }) {
                self.close().await;
            }
            return Err(err);
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

/// Records whether `write_frame` reached socket I/O. Encode errors happen
/// before `poll_write`; a later `write_all` / `flush` failure is ambiguous.
struct WriteProbe<W> {
    inner: W,
    started: bool,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for WriteProbe<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.started = true;
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn write_failure(err: io::Error, started: bool) -> RunnerError {
    if started {
        RunnerError::ambiguous(err.to_string())
    } else {
        RunnerError::before_dispatch(err.to_string())
    }
}

async fn write_envelope<W: AsyncWrite + Unpin>(
    writer: &mut W,
    envelope: &WireEnvelope,
) -> Result<(), RunnerError> {
    let mut probe = WriteProbe {
        inner: writer,
        started: false,
    };
    write_frame(&mut probe, envelope)
        .await
        .map_err(|err| write_failure(err, probe.started))
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

    async fn process_status(
        &self,
        process_id: &ProcessId,
    ) -> Result<RunnerProcessStatus, RunnerError> {
        match self
            .call(RunnerOp::ProcessStatus {
                process_id: process_id.clone(),
            })
            .await?
        {
            RunnerOpResult::ProcessStatus(result) => Ok(result),
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

#[cfg(test)]
mod tests {
    use super::*;

    struct FailOnWrite;

    impl AsyncWrite for FailOnWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<Result<usize, io::Error>> {
            Poll::Ready(Err(io::Error::other("write failed")))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    struct FailOnFlush;

    impl AsyncWrite for FailOnFlush {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, io::Error>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Err(io::Error::other("flush failed")))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn hello_envelope() -> WireEnvelope {
        WireEnvelope::request("rrpc-1".into(), RunnerOp::Hello)
    }

    #[test]
    fn encode_or_unstarted_write_is_before_dispatch() {
        let err = write_failure(io::Error::other("encode failed"), false);
        assert!(
            matches!(err, RunnerError::TransportBeforeDispatch { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn first_poll_write_error_is_ambiguous() {
        let mut writer = FailOnWrite;
        let err = write_envelope(&mut writer, &hello_envelope())
            .await
            .expect_err("write should fail");
        // `exec_command` keeps the workspace lease only on this variant.
        assert!(
            matches!(err, RunnerError::TransportAmbiguous { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn flush_error_after_write_is_ambiguous() {
        let mut writer = FailOnFlush;
        let err = write_envelope(&mut writer, &hello_envelope())
            .await
            .expect_err("flush should fail");
        assert!(
            matches!(err, RunnerError::TransportAmbiguous { .. }),
            "{err:?}"
        );
    }
}
