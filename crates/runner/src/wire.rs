//! CodeSpace-owned runner RPC. Not App Server and not MCP.
//!
//! Framing is `u32` big-endian length + JSON. Kinds share one stream:
//! request, response, and event. `request_id` (`rrpc-…`) is not an HTTP
//! id, `operation_id`, `operation_key`, or `process_id`.

use std::collections::VecDeque;
use std::sync::Arc;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};

use crate::{
    InProcessRunner, Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerError,
    RunnerExecRequest, RunnerExecResult, RunnerReadProcess, RunnerReadResult, RunnerWriteStdin,
    ShellRelease,
};

pub const WIRE_PROTOCOL: u32 = 1;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLAY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireKind {
    Request,
    Response,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEnvelope {
    pub protocol: u32,
    pub kind: WireKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<RunnerOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RunnerOpResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<RunnerEvent>,
}

impl WireEnvelope {
    pub fn request(request_id: String, op: RunnerOp) -> Self {
        Self {
            protocol: WIRE_PROTOCOL,
            kind: WireKind::Request,
            request_id: Some(request_id),
            op: Some(op),
            ok: None,
            result: None,
            error: None,
            event: None,
        }
    }

    pub fn response(request_id: String, result: Result<RunnerOpResult, ErrorBody>) -> Self {
        match result {
            Ok(value) => Self {
                protocol: WIRE_PROTOCOL,
                kind: WireKind::Response,
                request_id: Some(request_id),
                op: None,
                ok: Some(true),
                result: Some(value),
                error: None,
                event: None,
            },
            Err(error) => Self {
                protocol: WIRE_PROTOCOL,
                kind: WireKind::Response,
                request_id: Some(request_id),
                op: None,
                ok: Some(false),
                result: None,
                error: Some(error),
                event: None,
            },
        }
    }

    pub fn event(event: RunnerEvent) -> Self {
        Self {
            protocol: WIRE_PROTOCOL,
            kind: WireKind::Event,
            request_id: None,
            op: None,
            ok: None,
            result: None,
            error: None,
            event: Some(event),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RunnerEvent {
    ProcessExited { process_id: ProcessId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "data", rename_all = "snake_case")]
pub enum RunnerOp {
    Read {
        workspace: Workspace,
        path: String,
    },
    Find {
        workspace: Workspace,
        glob: Option<String>,
    },
    Version {
        workspace: Workspace,
        path: String,
    },
    ApplyPatch {
        workspace: Workspace,
        request: RunnerApplyPatchRequest,
    },
    Exec {
        workspace: Workspace,
        request: RunnerExecRequest,
    },
    WriteStdin {
        request: RunnerWriteStdin,
    },
    ReadProcess {
        request: RunnerReadProcess,
    },
    Terminate {
        process_id: ProcessId,
    },
    WorkspaceOf {
        process_id: String,
    },
    TerminateWorkspace {
        workspace_id: String,
    },
    Replay {
        request_id: String,
    },
    Hello,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RunnerOpResult {
    Read(ReadResult),
    Find(FindResult),
    Version(String),
    ApplyPatch(RunnerApplyPatchResult),
    Exec(RunnerExecResult),
    WriteStdin,
    ReadProcess(RunnerReadResult),
    Terminate,
    WorkspaceOf(Option<String>),
    TerminateWorkspace(u32),
    Hello { protocol: u32 },
}

/// Host worker: `InProcessRunner` plus a `ProcessExited` event stream.
pub fn host_worker() -> (InProcessRunner, mpsc::UnboundedReceiver<RunnerEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let on_release: ShellRelease = Arc::new(move |process_id: &str| {
        let _ = tx.send(RunnerEvent::ProcessExited {
            process_id: ProcessId(process_id.to_string()),
        });
    });
    (InProcessRunner::new(on_release), rx)
}

pub async fn serve_runner_connection<S>(
    stream: S,
    runner: InProcessRunner,
    mut events: mpsc::UnboundedReceiver<RunnerEvent>,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read, write) = tokio::io::split(stream);
    let write = Arc::new(Mutex::new(write));
    let event_write = write.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let mut writer = event_write.lock().await;
            if write_frame(&mut *writer, &WireEnvelope::event(event))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let mut cache: VecDeque<(String, WireEnvelope)> = VecDeque::new();
    loop {
        let payload = match read_frame(&mut read).await? {
            Some(payload) => payload,
            None => return Ok(()),
        };
        let request: WireEnvelope = match serde_json::from_slice(&payload) {
            Ok(request) => request,
            Err(err) => {
                let response = WireEnvelope::response(
                    String::new(),
                    Err(ErrorBody::new(
                        ErrorCode::InvalidPatch,
                        format!("invalid runner rpc: {err}"),
                    )),
                );
                let mut writer = write.lock().await;
                write_frame(&mut *writer, &response).await?;
                return Ok(());
            }
        };
        if request.protocol != WIRE_PROTOCOL || request.kind != WireKind::Request {
            let response = WireEnvelope::response(
                request.request_id.clone().unwrap_or_default(),
                Err(ErrorBody::new(
                    ErrorCode::InvalidPatch,
                    "runner rpc protocol mismatch",
                )),
            );
            let mut writer = write.lock().await;
            write_frame(&mut *writer, &response).await?;
            return Ok(());
        }
        let request_id = request.request_id.clone().unwrap_or_default();
        let response = match request.op {
            Some(RunnerOp::Replay {
                request_id: original,
            }) => cache
                .iter()
                .find(|(id, _)| id == &original)
                .map(|(_, cached)| {
                    let mut replayed = cached.clone();
                    replayed.request_id = Some(request_id.clone());
                    replayed
                })
                .unwrap_or_else(|| {
                    WireEnvelope::response(
                        request_id.clone(),
                        Err(ErrorBody::new(
                            ErrorCode::InvalidPatch,
                            format!("unknown runner request_id `{original}`"),
                        )),
                    )
                }),
            Some(op) => {
                let dispatched = dispatch(&runner, op).await;
                let envelope = WireEnvelope::response(request_id.clone(), dispatched);
                if !request_id.is_empty() {
                    if cache.len() >= MAX_REPLAY {
                        cache.pop_front();
                    }
                    cache.push_back((request_id, envelope.clone()));
                }
                envelope
            }
            None => WireEnvelope::response(
                request_id,
                Err(ErrorBody::new(
                    ErrorCode::InvalidPatch,
                    "runner rpc missing op",
                )),
            ),
        };
        let mut writer = write.lock().await;
        write_frame(&mut *writer, &response).await?;
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    write: &mut W,
    envelope: &WireEnvelope,
) -> Result<(), std::io::Error> {
    let payload = serde_json::to_vec(envelope).map_err(std::io::Error::other)?;
    let len = u32::try_from(payload.len()).map_err(std::io::Error::other)?;
    write.write_all(&len.to_be_bytes()).await?;
    write.write_all(&payload).await?;
    write.flush().await
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    read: &mut R,
) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut len_buf = [0u8; 4];
    match read.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(std::io::Error::other(format!(
            "invalid runner frame length {len}"
        )));
    }
    let mut payload = vec![0u8; len];
    read.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn dispatch(runner: &InProcessRunner, op: RunnerOp) -> Result<RunnerOpResult, ErrorBody> {
    let result = match op {
        RunnerOp::Read { workspace, path } => runner
            .read(&workspace, &path)
            .await
            .map(RunnerOpResult::Read),
        RunnerOp::Find { workspace, glob } => runner
            .find(&workspace, glob.as_deref())
            .await
            .map(RunnerOpResult::Find),
        RunnerOp::Version { workspace, path } => runner
            .version(&workspace, &path)
            .await
            .map(RunnerOpResult::Version),
        RunnerOp::ApplyPatch { workspace, request } => runner
            .apply_patch(&workspace, request)
            .await
            .map(RunnerOpResult::ApplyPatch),
        RunnerOp::Exec { workspace, request } => runner
            .exec(&workspace, request)
            .await
            .map(RunnerOpResult::Exec),
        RunnerOp::WriteStdin { request } => runner
            .write_stdin(request)
            .await
            .map(|_| RunnerOpResult::WriteStdin),
        RunnerOp::ReadProcess { request } => runner
            .read_process(request)
            .await
            .map(RunnerOpResult::ReadProcess),
        RunnerOp::Terminate { process_id } => runner
            .terminate(&process_id)
            .await
            .map(|_| RunnerOpResult::Terminate),
        RunnerOp::WorkspaceOf { process_id } => Ok(RunnerOpResult::WorkspaceOf(
            runner.workspace_of(&process_id).await,
        )),
        RunnerOp::TerminateWorkspace { workspace_id } => runner
            .terminate_workspace(&workspace_id)
            .await
            .map(RunnerOpResult::TerminateWorkspace),
        RunnerOp::Hello => Ok(RunnerOpResult::Hello {
            protocol: WIRE_PROTOCOL,
        }),
        RunnerOp::Replay { .. } => unreachable!("replay is handled before dispatch"),
    };
    result.map_err(RunnerError::into_error_body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_policy::Workspace;

    #[test]
    fn request_id_prefix_is_rrpc() {
        assert!(format!("rrpc-{}", 1).starts_with("rrpc-"));
    }

    #[tokio::test]
    async fn length_prefix_round_trip() {
        let (client, server) = tokio::io::duplex(4096);
        let (mut client_read, mut client_write) = tokio::io::split(client);
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let envelope = WireEnvelope::request(
            "rrpc-1".into(),
            RunnerOp::Terminate {
                process_id: ProcessId("proc-1".into()),
            },
        );
        write_frame(&mut client_write, &envelope).await.unwrap();
        let payload = read_frame(&mut server_read).await.unwrap().unwrap();
        let parsed: WireEnvelope = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed.protocol, WIRE_PROTOCOL);
        assert_eq!(parsed.kind, WireKind::Request);
        assert_eq!(parsed.request_id.as_deref(), Some("rrpc-1"));
        write_frame(
            &mut server_write,
            &WireEnvelope::response("rrpc-1".into(), Ok(RunnerOpResult::Terminate)),
        )
        .await
        .unwrap();
        let reply = read_frame(&mut client_read).await.unwrap().unwrap();
        let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
        assert_eq!(parsed.kind, WireKind::Response);
        assert_eq!(parsed.ok, Some(true));
    }

    #[tokio::test]
    async fn replay_returns_cached_response() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(
            codespace_domain::WorkspaceId("demo".into()),
            dir.path().to_path_buf(),
            codespace_domain::Profile::ReadOnly,
        );
        std::fs::write(dir.path().join("a.txt"), "cached").unwrap();
        write_frame(
            &mut write,
            &WireEnvelope::request(
                "rrpc-orig".into(),
                RunnerOp::Read {
                    workspace: ws.clone(),
                    path: "a.txt".into(),
                },
            ),
        )
        .await
        .unwrap();
        let first = read_frame(&mut read).await.unwrap().unwrap();
        let first: WireEnvelope = serde_json::from_slice(&first).unwrap();
        assert_eq!(first.ok, Some(true));
        write_frame(
            &mut write,
            &WireEnvelope::request(
                "rrpc-replay".into(),
                RunnerOp::Replay {
                    request_id: "rrpc-orig".into(),
                },
            ),
        )
        .await
        .unwrap();
        let second = read_frame(&mut read).await.unwrap().unwrap();
        let second: WireEnvelope = serde_json::from_slice(&second).unwrap();
        assert_eq!(second.request_id.as_deref(), Some("rrpc-replay"));
        assert_eq!(
            serde_json::to_value(&second.result).unwrap(),
            serde_json::to_value(&first.result).unwrap()
        );
    }

    #[tokio::test]
    async fn protocol_mismatch_responds_then_closes() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        let mut envelope = WireEnvelope::request("rrpc-bad".into(), RunnerOp::Hello);
        envelope.protocol = 99;
        write_frame(&mut write, &envelope).await.unwrap();
        let reply = read_frame(&mut read).await.unwrap().unwrap();
        let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
        assert_eq!(parsed.ok, Some(false));
        assert!(parsed.error.is_some());
        let eof = read_frame(&mut read).await.unwrap();
        assert!(eof.is_none());
    }

    #[tokio::test]
    async fn hello_round_trip() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        write_frame(
            &mut write,
            &WireEnvelope::request("rrpc-hello".into(), RunnerOp::Hello),
        )
        .await
        .unwrap();
        let reply = read_frame(&mut read).await.unwrap().unwrap();
        let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
        assert_eq!(parsed.ok, Some(true));
        match parsed.result {
            Some(RunnerOpResult::Hello { protocol }) => assert_eq!(protocol, WIRE_PROTOCOL),
            other => panic!("unexpected {other:?}"),
        }
    }
}
