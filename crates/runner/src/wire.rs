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
    RunnerExecRequest, RunnerExecResult, RunnerProcessStatus, RunnerReadProcess, RunnerReadResult,
    RunnerResizeResult, RunnerWriteStdin, ShellRelease,
};

pub const WIRE_PROTOCOL: u32 = 6;
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
        #[serde(default)]
        offset: Option<u64>,
        #[serde(default)]
        limit: Option<u32>,
    },
    Find {
        workspace: Workspace,
        glob: Option<String>,
        #[serde(default)]
        offset: Option<u64>,
        #[serde(default)]
        limit: Option<u32>,
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
    ProcessStatus {
        process_id: ProcessId,
    },
    Resize {
        process_id: ProcessId,
        rows: u16,
        cols: u16,
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
    /// The execution owner's DevGuard registration (CSRG-U2). Sent only to a worker whose
    /// `Hello` states `registration`; a worker without it would close the connection.
    #[cfg(feature = "devguard")]
    Registration,
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
    ProcessStatus(RunnerProcessStatus),
    Resize(RunnerResizeResult),
    Terminate,
    WorkspaceOf(Option<String>),
    TerminateWorkspace(u32),
    Hello {
        protocol: u32,
        /// The worker answers `Registration` (CSRG-U2). Left out when false, so a worker built
        /// without DevGuard says `Hello` as before.
        #[cfg(feature = "devguard")]
        #[serde(default, skip_serializing_if = "is_false")]
        registration: bool,
    },
    #[cfg(feature = "devguard")]
    Registration(codespace_domain::ResourceAuthorityInfo),
}

#[cfg(feature = "devguard")]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The worker's registration, when it was started with one.
#[cfg(feature = "devguard")]
type Registrar = Option<Arc<crate::OwnerRegistration>>;
#[cfg(not(feature = "devguard"))]
type Registrar = ();

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
    events: mpsc::UnboundedReceiver<RunnerEvent>,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    #[cfg(feature = "devguard")]
    let registrar = None;
    #[cfg(not(feature = "devguard"))]
    let registrar = ();
    serve(stream, runner, events, registrar).await
}

/// [`serve_runner_connection`] for a worker that owns its executions' DevGuard registration.
/// A `Registration` request runs beside the other requests, so no process query, write,
/// resize or termination waits for its session.
#[cfg(feature = "devguard")]
pub async fn serve_runner_connection_with_registration<S>(
    stream: S,
    runner: InProcessRunner,
    events: mpsc::UnboundedReceiver<RunnerEvent>,
    registration: Option<Arc<crate::OwnerRegistration>>,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve(stream, runner, events, registration).await
}

async fn serve<S>(
    stream: S,
    runner: InProcessRunner,
    mut events: mpsc::UnboundedReceiver<RunnerEvent>,
    registrar: Registrar,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    #[cfg(not(feature = "devguard"))]
    let () = registrar;
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
        #[cfg(feature = "devguard")]
        if matches!(request.op, Some(RunnerOp::Registration)) {
            // Not cached for replay: asking again opens a new session for the same identity.
            let (write, registrar) = (write.clone(), registrar.clone());
            tokio::spawn(async move {
                let result = match registrar {
                    Some(registration) => {
                        Ok(RunnerOpResult::Registration(registration.report().await))
                    }
                    None => Err(ErrorBody::new(
                        ErrorCode::ResourcePolicyUnsupported,
                        "this worker was started without DevGuard settings",
                    )),
                };
                let response = WireEnvelope::response(request_id, result);
                let mut writer = write.lock().await;
                let _ = write_frame(&mut *writer, &response).await;
            });
            continue;
        }
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
        RunnerOp::Read {
            workspace,
            path,
            offset,
            limit,
        } => runner
            .read(&workspace, &path, offset, limit)
            .await
            .map(RunnerOpResult::Read),
        RunnerOp::Find {
            workspace,
            glob,
            offset,
            limit,
        } => runner
            .find(&workspace, glob.as_deref(), offset, limit)
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
        RunnerOp::ProcessStatus { process_id } => runner
            .process_status(&process_id)
            .await
            .map(RunnerOpResult::ProcessStatus),
        RunnerOp::Resize {
            process_id,
            rows,
            cols,
        } => runner
            .resize(&process_id, rows, cols)
            .await
            .map(RunnerOpResult::Resize),
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
            #[cfg(feature = "devguard")]
            registration: true,
        }),
        RunnerOp::Replay { .. } => unreachable!("replay is handled before dispatch"),
        #[cfg(feature = "devguard")]
        RunnerOp::Registration => unreachable!("registration is handled before dispatch"),
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

    #[test]
    fn wire_protocol_is_v6() {
        assert_eq!(WIRE_PROTOCOL, 6);
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
                    offset: None,
                    limit: None,
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
    async fn protocol_3_hello_is_rejected() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        let mut envelope = WireEnvelope::request("rrpc-old".into(), RunnerOp::Hello);
        envelope.protocol = 3;
        write_frame(&mut write, &envelope).await.unwrap();
        let reply = read_frame(&mut read).await.unwrap().unwrap();
        let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
        assert_eq!(parsed.ok, Some(false));
        assert!(parsed.error.is_some());
    }

    #[tokio::test]
    async fn protocol_4_hello_is_rejected() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        let mut envelope = WireEnvelope::request("rrpc-old".into(), RunnerOp::Hello);
        envelope.protocol = 4;
        write_frame(&mut write, &envelope).await.unwrap();
        let reply = read_frame(&mut read).await.unwrap().unwrap();
        let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
        assert_eq!(parsed.ok, Some(false));
        assert!(parsed.error.is_some());
    }

    #[cfg(feature = "devguard")]
    mod registration {
        use super::*;
        use crate::registration::{Owner, OwnerCredential, OwnerSettings};
        use crate::OwnerRegistration;
        use codespace_domain::ResourceOwner;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::time::Duration;

        async fn request(write: &mut tokio::net::unix::OwnedWriteHalf, id: &str, op: RunnerOp) {
            write_frame(write, &WireEnvelope::request(id.into(), op))
                .await
                .unwrap();
        }

        async fn response(read: &mut tokio::net::unix::OwnedReadHalf) -> WireEnvelope {
            loop {
                let frame = read_frame(read).await.unwrap().unwrap();
                let envelope: WireEnvelope = serde_json::from_slice(&frame).unwrap();
                if envelope.kind == WireKind::Response {
                    return envelope;
                }
            }
        }

        fn serve_with(
            registration: Option<Arc<OwnerRegistration>>,
        ) -> (
            tokio::net::unix::OwnedReadHalf,
            tokio::net::unix::OwnedWriteHalf,
        ) {
            let (client, server) = tokio::net::UnixStream::pair().unwrap();
            let (worker, events) = host_worker();
            tokio::spawn(async move {
                serve_runner_connection_with_registration(server, worker, events, registration)
                    .await
                    .expect("serve");
            });
            client.into_split()
        }

        #[tokio::test]
        async fn hello_states_registration_and_an_unconfigured_worker_says_unsupported() {
            let (mut read, mut write) = serve_with(None);
            request(&mut write, "rrpc-hello", RunnerOp::Hello).await;
            match response(&mut read).await.result {
                Some(RunnerOpResult::Hello {
                    protocol,
                    registration,
                }) => assert_eq!((protocol, registration), (WIRE_PROTOCOL, true)),
                other => panic!("unexpected {other:?}"),
            }
            request(&mut write, "rrpc-reg", RunnerOp::Registration).await;
            let reply = response(&mut read).await;
            assert_eq!(reply.request_id.as_deref(), Some("rrpc-reg"));
            assert_eq!(reply.ok, Some(false));
            assert_eq!(
                reply.error.unwrap().code,
                ErrorCode::ResourcePolicyUnsupported
            );
            // The connection stays open.
            request(&mut write, "rrpc-hello-again", RunnerOp::Hello).await;
            assert_eq!(response(&mut read).await.ok, Some(true));
        }

        #[test]
        fn a_hello_without_registration_decodes_as_false() {
            let old: RunnerOpResult =
                serde_json::from_str(r#"{"type":"hello","data":{"protocol":6}}"#).unwrap();
            assert!(matches!(
                old,
                RunnerOpResult::Hello {
                    registration: false,
                    ..
                }
            ));
        }

        /// A private directory on a path without symbolic links, as the credential needs.
        fn private_directory() -> tempfile::TempDir {
            tempfile::Builder::new()
                .prefix("cs-wire-")
                .tempdir_in(if cfg!(target_os = "macos") {
                    "/private/tmp"
                } else {
                    "/tmp"
                })
                .unwrap()
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn process_control_does_not_wait_for_a_registration_session() {
            let dir = private_directory();
            let credential = dir.path().join("consumer.secret");
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&credential)
                .unwrap()
                .write_all("ab".repeat(32).as_bytes())
                .unwrap();
            // An authority that accepts each session and answers nothing, so a registration
            // session lasts until DevGuard's 250 ms frame deadline.
            let socket = dir.path().join("silent.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let (accepted, sessions) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let _ = accepted.send(());
                    let stream = stream.unwrap();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(2));
                        drop(stream);
                    });
                }
            });
            let owner = Owner::new(
                OwnerSettings {
                    socket,
                    consumer: "codespace".into(),
                    generation: "g1".into(),
                },
                OwnerCredential::File(credential),
            );
            let (mut read, mut write) =
                serve_with(Some(OwnerRegistration::new(owner, ResourceOwner::Worker)));
            request(&mut write, "rrpc-reg", RunnerOp::Registration).await;
            // Once the session is open, process control on the same connection is answered
            // before the session ends.
            tokio::task::spawn_blocking(move || sessions.recv().unwrap())
                .await
                .unwrap();
            request(
                &mut write,
                "rrpc-status",
                RunnerOp::ProcessStatus {
                    process_id: ProcessId("proc-none".into()),
                },
            )
            .await;
            request(
                &mut write,
                "rrpc-terminate",
                RunnerOp::Terminate {
                    process_id: ProcessId("proc-none".into()),
                },
            )
            .await;
            let order: Vec<_> = [
                response(&mut read).await,
                response(&mut read).await,
                response(&mut read).await,
            ]
            .into_iter()
            .map(|reply| reply.request_id.unwrap())
            .collect();
            assert_eq!(order, ["rrpc-status", "rrpc-terminate", "rrpc-reg"]);
        }

        /// Through the gateway's client: while every registration session of the worker fails,
        /// a running process still times out, reports its outcome and can be queried.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_timeout_needs_no_successful_registration() {
            use crate::{Runner, RunnerExecRequest, UdsRunner};
            use codespace_domain::{
                ProcessState, ProcessTermination, Profile, ResourceRegistrationState,
            };
            use std::sync::atomic::{AtomicBool, Ordering};
            let dir = private_directory();
            let credential = dir.path().join("consumer.secret");
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&credential)
                .unwrap()
                .write_all("cd".repeat(32).as_bytes())
                .unwrap();
            // An authority that accepts each session and answers nothing: every registration
            // ends unavailable at DevGuard's frame deadline.
            let socket = dir.path().join("silent.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let stream = stream.unwrap();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(2));
                        drop(stream);
                    });
                }
            });
            let owner = Owner::new(
                OwnerSettings {
                    socket,
                    consumer: "codespace".into(),
                    generation: "g1".into(),
                },
                OwnerCredential::File(credential),
            );
            let (client, server) = tokio::net::UnixStream::pair().unwrap();
            let (worker, events) = host_worker();
            let registration = OwnerRegistration::new(owner, ResourceOwner::Worker);
            tokio::spawn(async move {
                serve_runner_connection_with_registration(
                    server,
                    worker,
                    events,
                    Some(registration),
                )
                .await
                .expect("serve");
            });
            let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
            runner.handshake().await.unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let registering = {
                let (runner, stop) = (runner.clone(), stop.clone());
                tokio::spawn(async move {
                    let mut failed = 0usize;
                    while !stop.load(Ordering::Relaxed) {
                        let info = runner.registration().await.unwrap();
                        assert_eq!(
                            info.registration.unwrap().state,
                            ResourceRegistrationState::Unavailable
                        );
                        failed += 1;
                    }
                    failed
                })
            };
            let root = tempfile::tempdir().unwrap();
            let workspace = Workspace::new(
                codespace_domain::WorkspaceId("demo".into()),
                root.path().to_path_buf(),
                Profile::WorkspaceWrite,
            );
            let process_id = ProcessId("proc-timeout".into());
            let mut request = RunnerExecRequest::for_host(
                vec!["/bin/sleep".into(), "30".into()],
                process_id.clone(),
                Profile::WorkspaceWrite,
            );
            request.timeout_ms = 300;
            runner.exec(&workspace, request).await.unwrap();
            let mut status = runner.process_status(&process_id).await.unwrap();
            for _ in 0..500 {
                if status.state == ProcessState::Exited {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                status = runner.process_status(&process_id).await.unwrap();
            }
            assert_eq!(status.state, ProcessState::Exited, "{status:?}");
            assert_eq!(
                status.termination,
                Some(ProcessTermination::Timeout),
                "{status:?}"
            );
            stop.store(true, Ordering::Relaxed);
            assert!(registering.await.unwrap() > 0);
        }
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
            Some(RunnerOpResult::Hello { protocol, .. }) => assert_eq!(protocol, WIRE_PROTOCOL),
            other => panic!("unexpected {other:?}"),
        }
    }
}
