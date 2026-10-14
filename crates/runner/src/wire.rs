//! CodeSpace-owned runner RPC. Not App Server and not MCP.
//!
//! Framing is `u32` big-endian length + JSON. Kinds share one stream:
//! request, response, and event. `request_id` (`rrpc-…`) is not an HTTP
//! id, `operation_id`, `operation_key`, or `process_id`.
//!
//! A `managed` worker serves the one connection of the gateway that started it. An
//! `independent` Runner (P1R-C01) serves one connection at a time through [`IndependentRunner`]:
//! its processes, their output, their timeouts and their exit events outlive each connection.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::task::JoinSet;

use crate::lifecycle::{RunnerIdentity, RunnerLifecycle};
use crate::{
    InProcessRunner, Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerError,
    RunnerExecRequest, RunnerExecResult, RunnerProcessStatus, RunnerReadProcess, RunnerReadResult,
    RunnerResizeResult, RunnerWriteStdin, ShellRelease,
};

pub const WIRE_PROTOCOL: u32 = 9;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLAY: usize = 32;
/// How long a stopping independent Runner waits for its terminated processes to be reaped and
/// their output to end before it exits anyway (P1R-C01).
pub const STOP_DRAIN_BOUND: Duration = Duration::from_secs(5);

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
    /// The gateway leaves an independent Runner and its processes keep running (P1R-C01). The
    /// Runner answers `Detached`, reads nothing more from this connection and serves the next
    /// one once this connection's requests have ended. A managed worker refuses it.
    Detach,
    /// Stop an independent Runner (P1R-C01): it reads nothing more, lets this connection's
    /// requests end, terminates every process, waits for them, answers `Stopped` and exits. A
    /// managed worker refuses it.
    Stop,
    /// The execution owner's DevGuard registration (CSRG-U2). Sent only to a worker whose
    /// `Hello` states `registration`; a worker without it would close the connection.
    #[cfg(feature = "devguard")]
    Registration,
    /// Prepare a governed execution under the worker's own registration (CSRG-U3): its slot,
    /// its admission and, as the worker cannot launch it, the cancellation of the unstarted
    /// attempt. Sent only to a worker whose `Hello` states `admission`. The worker never starts
    /// the command.
    #[cfg(feature = "devguard")]
    Prepare {
        workspace: Workspace,
        request: RunnerExecRequest,
        attempt_id: String,
    },
    /// Prepare a governed execution under the worker's own registration and launch it once
    /// through the worker's launch helper (CSRG-U4): the worker is the helper's parent, as
    /// DevGuard requires of the registered owner. Sent only to a worker whose `Hello` states
    /// `launch`, and never sent again: a launch is not replayed.
    #[cfg(feature = "devguard")]
    Launch {
        workspace: Workspace,
        request: RunnerExecRequest,
        attempt_id: String,
    },
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
        /// Whether the worker belongs to the gateway that started it or outlives its
        /// connections (P1R-C01).
        #[serde(default)]
        lifecycle: RunnerLifecycle,
        /// Which Runner process answered (P1R-C01).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<RunnerIdentity>,
        /// An earlier connection of this independent Runner started work, so this connection
        /// may observe and control the processes it inherits but starts nothing (P1R-C01).
        #[serde(default)]
        previous_controller: bool,
        /// Processes still running when this connection began, which it inherits from
        /// earlier connections (P1R-C01).
        #[serde(default)]
        inherited_processes: u32,
        /// The worker answers `Registration` (CSRG-U2). Left out when false, so a worker built
        /// without DevGuard says `Hello` as before.
        #[cfg(feature = "devguard")]
        #[serde(default, skip_serializing_if = "is_false")]
        registration: bool,
        /// The worker answers `Prepare` (CSRG-U3). Left out when false, as `registration`.
        #[cfg(feature = "devguard")]
        #[serde(default, skip_serializing_if = "is_false")]
        admission: bool,
        /// The worker answers `Launch`: it registers and has a launch helper (CSRG-U4). Left
        /// out when false, as `registration`.
        #[cfg(feature = "devguard")]
        #[serde(default, skip_serializing_if = "is_false")]
        launch: bool,
    },
    /// An independent Runner's answer to `Detach` (P1R-C01).
    Detached,
    /// An independent Runner's answer to `Stop`, sent once its processes ended or
    /// [`STOP_DRAIN_BOUND`] passed (P1R-C01): how many processes were still running when it
    /// terminated them.
    Stopped {
        terminated: u32,
    },
    #[cfg(feature = "devguard")]
    Registration(codespace_domain::ResourceAuthorityInfo),
    #[cfg(feature = "devguard")]
    Prepared(crate::admission::ManagedPreparation),
    #[cfg(feature = "devguard")]
    Launched(crate::launch::LaunchReport),
}

#[cfg(feature = "devguard")]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The refusal of new work on a connection to an independent Runner that an earlier connection
/// controlled (P1R-C01): recovering what that controller left is not available yet.
pub fn recovery_pending() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RecoveryPending,
        "an earlier gateway controlled this independent Runner, and recovering what it left is \
         not available yet: its processes can be queried, read, written to, resized and \
         terminated, but nothing new is started. Stop the Runner and start a new one to start \
         work again",
    )
}

/// The worker's registration, when it was started with one, and its launch helper.
#[cfg(feature = "devguard")]
#[derive(Clone, Default)]
struct Registrar {
    registration: Option<Arc<crate::OwnerRegistration>>,
    launcher: Option<crate::launch::Launcher>,
}
#[cfg(not(feature = "devguard"))]
type Registrar = ();

#[cfg(feature = "devguard")]
impl Registrar {
    /// The worker launches governed executions: it registers and has a launch helper.
    fn launches(&self) -> bool {
        self.registration.is_some() && self.launcher.is_some()
    }
}

/// What a connection's `Hello` states.
fn hello(
    registrar: &Registrar,
    lifecycle: RunnerLifecycle,
    previous_controller: bool,
    inherited_processes: u32,
) -> RunnerOpResult {
    #[cfg(not(feature = "devguard"))]
    let () = *registrar;
    RunnerOpResult::Hello {
        protocol: WIRE_PROTOCOL,
        lifecycle,
        identity: Some(RunnerIdentity::this_process()),
        previous_controller,
        inherited_processes,
        #[cfg(feature = "devguard")]
        registration: true,
        #[cfg(feature = "devguard")]
        admission: true,
        // A worker states `launch` only with a registration and a launch helper.
        #[cfg(feature = "devguard")]
        launch: registrar.launches(),
    }
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
    events: mpsc::UnboundedReceiver<RunnerEvent>,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    #[cfg(feature = "devguard")]
    let registrar = Registrar::default();
    #[cfg(not(feature = "devguard"))]
    let registrar = ();
    serve(stream, runner, events, registrar).await
}

/// [`serve_runner_connection`] for a worker that owns its executions' DevGuard registration.
/// A `Registration`, `Prepare` or `Launch` request runs beside the other requests, so no
/// process query, write, resize or termination waits for its sessions. With `launcher` the
/// worker launches governed executions (CSRG-U4).
#[cfg(feature = "devguard")]
pub async fn serve_runner_connection_with_registration<S>(
    stream: S,
    runner: InProcessRunner,
    events: mpsc::UnboundedReceiver<RunnerEvent>,
    registration: Option<Arc<crate::OwnerRegistration>>,
    launcher: Option<crate::launch::Launcher>,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve(
        stream,
        runner,
        events,
        Registrar {
            registration,
            launcher,
        },
    )
    .await
}

/// A managed worker's one connection.
async fn serve<S>(
    stream: S,
    runner: InProcessRunner,
    mut events: mpsc::UnboundedReceiver<RunnerEvent>,
    registrar: Registrar,
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

    let hello = hello(&registrar, RunnerLifecycle::Managed, false, 0);
    let mut cache: VecDeque<(String, WireEnvelope)> = VecDeque::new();
    loop {
        let payload = match read_frame(&mut read).await? {
            Some(payload) => payload,
            None => return Ok(()),
        };
        let (request_id, op) = match parse_request(&payload) {
            Ok(request) => request,
            Err(response) => {
                let mut writer = write.lock().await;
                write_frame(&mut *writer, &response).await?;
                return Ok(());
            }
        };
        #[cfg(feature = "devguard")]
        let op = match op {
            Some(op) if answered_beside(&op) => {
                // Beside the other requests, and never cached: asking again opens a new
                // session, and a preparation or a launch is never replayed. A launch is its own
                // task, so it settles even if the gateway stops waiting.
                let (write, registrar, runner) = (write.clone(), registrar.clone(), runner.clone());
                tokio::spawn(async move {
                    let result = answer_beside(op, registrar, runner).await;
                    let response = WireEnvelope::response(request_id, result);
                    let mut writer = write.lock().await;
                    let _ = write_frame(&mut *writer, &response).await;
                });
                continue;
            }
            op => op,
        };
        let response = match op {
            Some(RunnerOp::Detach | RunnerOp::Stop) => WireEnvelope::response(
                request_id,
                Err(ErrorBody::new(
                    ErrorCode::InvalidCommand,
                    "this worker belongs to the gateway that started it; only an independent \
                     Runner detaches or stops on request",
                )),
            ),
            op => answer(&mut cache, &runner, request_id, op, &hello).await,
        };
        let mut writer = write.lock().await;
        write_frame(&mut *writer, &response).await?;
    }
}

/// A request's id and operation, or the response that refuses it and ends the connection.
fn parse_request(payload: &[u8]) -> Result<(String, Option<RunnerOp>), Box<WireEnvelope>> {
    let request: WireEnvelope = serde_json::from_slice(payload).map_err(|err| {
        Box::new(WireEnvelope::response(
            String::new(),
            Err(ErrorBody::new(
                ErrorCode::InvalidPatch,
                format!("invalid runner rpc: {err}"),
            )),
        ))
    })?;
    if request.protocol != WIRE_PROTOCOL || request.kind != WireKind::Request {
        return Err(Box::new(WireEnvelope::response(
            request.request_id.clone().unwrap_or_default(),
            Err(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "runner rpc protocol mismatch",
            )),
        )));
    }
    Ok((request.request_id.unwrap_or_default(), request.op))
}

/// The response to a request answered in turn: a replay from `cache`, the connection's `hello`,
/// or the runner's answer, which `cache` keeps for replay.
async fn answer(
    cache: &mut VecDeque<(String, WireEnvelope)>,
    runner: &InProcessRunner,
    request_id: String,
    op: Option<RunnerOp>,
    hello: &RunnerOpResult,
) -> WireEnvelope {
    match op {
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
            let dispatched = match op {
                RunnerOp::Hello => Ok(hello.clone()),
                op => dispatch(runner, op).await,
            };
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
    }
}

/// A `Registration`, `Prepare` or `Launch` request, which runs beside the other requests and is
/// never cached for replay.
#[cfg(feature = "devguard")]
fn answered_beside(op: &RunnerOp) -> bool {
    matches!(
        op,
        RunnerOp::Registration | RunnerOp::Prepare { .. } | RunnerOp::Launch { .. }
    )
}

#[cfg(feature = "devguard")]
async fn answer_beside(
    op: RunnerOp,
    registrar: Registrar,
    runner: InProcessRunner,
) -> Result<RunnerOpResult, ErrorBody> {
    match op {
        RunnerOp::Registration => match registrar.registration {
            Some(registration) => Ok(RunnerOpResult::Registration(registration.report().await)),
            None => Err(ErrorBody::new(
                ErrorCode::ResourcePolicyUnsupported,
                "this worker was started without DevGuard settings",
            )),
        },
        RunnerOp::Prepare {
            workspace,
            request,
            attempt_id,
        } => match registrar.registration {
            Some(registration) => Ok(RunnerOpResult::Prepared(
                crate::admission::Preparer::new(registration.owner(), runner)
                    .prepare_and_dispose(workspace, request, attempt_id)
                    .await,
            )),
            None => Err(ErrorBody::new(
                ErrorCode::ResourcePolicyUnsupported,
                "this worker was started without DevGuard settings",
            )),
        },
        RunnerOp::Launch {
            workspace,
            request,
            attempt_id,
        } => match (registrar.registration, registrar.launcher) {
            (Some(registration), Some(launcher)) => Ok(RunnerOpResult::Launched(
                crate::admission::Preparer::new(registration.owner(), runner)
                    .with_launcher(Some(launcher))
                    .prepare_and_launch(workspace, request, attempt_id)
                    .await,
            )),
            _ => Err(ErrorBody::new(
                ErrorCode::ResourcePolicyUnsupported,
                "this worker was started without DevGuard settings or a launch helper",
            )),
        },
        other => unreachable!("{other:?} is answered in turn"),
    }
}

/// A request that starts work an earlier connection's successor could not know about: a
/// process, a change to the workspace or an admission (P1R-C01). The first one accepted makes
/// its connection the independent Runner's controller.
fn starts_work(op: &RunnerOp) -> bool {
    match op {
        RunnerOp::Exec { .. } | RunnerOp::ApplyPatch { .. } => true,
        #[cfg(feature = "devguard")]
        RunnerOp::Prepare { .. } | RunnerOp::Launch { .. } => true,
        _ => false,
    }
}

/// How an independent Runner's connection ended (P1R-C01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// The gateway detached: the processes keep running.
    Detached,
    /// The connection ended without `Detach` or `Stop`, or with an unusable request: a loss.
    /// The processes keep running; the Runner never infers an intent from it.
    Lost,
    /// The Runner stopped, through `Stop` or its own stop request: it terminated `terminated`
    /// processes and waited for them, and serves no further connection.
    Stopped { terminated: u32 },
}

/// Asks an independent Runner to stop (P1R-C01), as `Stop` does: from a signal handler, or with
/// no connection open.
#[derive(Clone)]
pub struct StopHandle {
    requested: Arc<watch::Sender<bool>>,
    /// How many processes the stop terminated so far.
    terminated: Arc<AtomicU32>,
    runner: InProcessRunner,
}

impl StopHandle {
    /// Stop the Runner: every process is terminated now, so no request waits on one, and the
    /// Runner reads no further request.
    pub fn request(&self) {
        self.requested.send_replace(true);
        self.terminate();
    }

    /// Terminate the processes still running, and count those nothing had asked to end yet.
    fn terminate(&self) -> u32 {
        let terminated = self.runner.terminate_all();
        self.terminated.fetch_add(terminated, Ordering::SeqCst) + terminated
    }

    pub fn is_requested(&self) -> bool {
        *self.requested.borrow()
    }

    /// Resolves once a stop has been requested.
    pub async fn requested(&self) {
        let mut requested = self.requested.subscribe();
        let _ = requested.wait_for(|stop| *stop).await;
    }
}

/// The exit events of an independent Runner's processes, kept between its connections.
struct EventHub {
    events: mpsc::UnboundedReceiver<RunnerEvent>,
    /// An event whose write failed, which the next connection gets first.
    undelivered: Option<RunnerEvent>,
}

/// Forward exit events to one connection until `stop`, or until a write fails. Events left in
/// the hub go to the next connection.
async fn forward_events<W>(
    hub: Arc<Mutex<EventHub>>,
    write: Arc<Mutex<W>>,
    mut stop: oneshot::Receiver<()>,
) where
    W: AsyncWrite + Unpin,
{
    let mut hub = hub.lock().await;
    loop {
        let event = match hub.undelivered.take() {
            Some(event) => event,
            None => tokio::select! {
                biased;
                _ = &mut stop => return,
                event = hub.events.recv() => match event {
                    Some(event) => event,
                    None => return,
                },
            },
        };
        let mut writer = write.lock().await;
        if write_frame(&mut *writer, &WireEnvelope::event(event.clone()))
            .await
            .is_err()
        {
            hub.undelivered = Some(event);
            return;
        }
    }
}

/// An independent Runner (P1R-C01), started by an operator or a harness rather than a gateway.
/// It serves one connection at a time, and its processes, their output, their timeouts and
/// their exit events outlive each connection. A connection becomes the Runner's controller with
/// the first request that starts work; every later connection states `previous_controller` in
/// its `Hello` and may only query, read, write stdin, resize and terminate, until the Runner
/// stops. Recovering what an earlier controller left is later work (P1R-C03, P1R-C04).
pub struct IndependentRunner {
    runner: InProcessRunner,
    events: Arc<Mutex<EventHub>>,
    registrar: Registrar,
    /// A connection has started work.
    controlled: bool,
    stop: StopHandle,
}

impl IndependentRunner {
    pub fn new(runner: InProcessRunner, events: mpsc::UnboundedReceiver<RunnerEvent>) -> Self {
        let stop = StopHandle {
            requested: Arc::new(watch::Sender::new(false)),
            terminated: Arc::new(AtomicU32::new(0)),
            runner: runner.clone(),
        };
        Self {
            runner,
            events: Arc::new(Mutex::new(EventHub {
                events,
                undelivered: None,
            })),
            #[cfg(feature = "devguard")]
            registrar: Registrar::default(),
            #[cfg(not(feature = "devguard"))]
            registrar: (),
            controlled: false,
            stop,
        }
    }

    /// The Runner's own DevGuard registration, which it keeps across its connections, and its
    /// launch helper (CSRG-U2, CSRG-U4).
    #[cfg(feature = "devguard")]
    pub fn with_registration(
        mut self,
        registration: Option<Arc<crate::OwnerRegistration>>,
        launcher: Option<crate::launch::Launcher>,
    ) -> Self {
        self.registrar = Registrar {
            registration,
            launcher,
        };
        self
    }

    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }

    /// Serve each connection that `accept` yields, one at a time, until the Runner stops: through
    /// `Stop`, or a stop request, which ends a connection being served or the wait for the next
    /// one. `served` hears how each connection ended, or why one could not be accepted; a failed
    /// accept is tried again after a pause. How many processes the stop terminated.
    pub async fn run<S, A, F>(
        mut self,
        mut accept: A,
        mut served: impl FnMut(std::io::Result<Served>),
    ) -> u32
    where
        A: FnMut() -> F,
        F: std::future::Future<Output = std::io::Result<S>>,
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let stop = self.stop.clone();
        loop {
            let accepted = tokio::select! {
                biased;
                () = stop.requested() => {
                    let terminated = self.stop().await;
                    served(Ok(Served::Stopped { terminated }));
                    return terminated;
                }
                accepted = accept() => accepted,
            };
            match accepted {
                Ok(stream) => {
                    let ending = self.serve(stream).await;
                    served(Ok(ending));
                    if let Served::Stopped { terminated } = ending {
                        return terminated;
                    }
                }
                Err(err) => {
                    served(Err(err));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Serve one connection until it detaches, is lost or stops the Runner. Every request it
    /// sent has ended when this returns, its `Registration`, `Prepare` and `Launch` requests
    /// included, so the next connection starts after them.
    pub async fn serve<S>(&mut self, stream: S) -> Served
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let previous_controller = self.controlled;
        let hello = hello(
            &self.registrar,
            RunnerLifecycle::Independent,
            previous_controller,
            self.runner.live_processes(),
        );
        let (mut read, write) = tokio::io::split(stream);
        let write = Arc::new(Mutex::new(write));
        let (stop_events, events_stopped) = oneshot::channel();
        let forwarder = tokio::spawn(forward_events(
            self.events.clone(),
            write.clone(),
            events_stopped,
        ));
        let mut beside: JoinSet<()> = JoinSet::new();
        let mut cache: VecDeque<(String, WireEnvelope)> = VecDeque::new();
        let stop = self.stop.clone();
        let ending = loop {
            let frame = tokio::select! {
                biased;
                () = stop.requested() => break Ending::Stop(None),
                frame = read_frame(&mut read) => frame,
            };
            let Ok(Some(payload)) = frame else {
                break Ending::Lost;
            };
            let (request_id, op) = match parse_request(&payload) {
                Ok(request) => request,
                Err(response) => {
                    let mut writer = write.lock().await;
                    let _ = write_frame(&mut *writer, &response).await;
                    break Ending::Lost;
                }
            };
            let response = match op {
                Some(RunnerOp::Detach) => break Ending::Detach(request_id),
                Some(RunnerOp::Stop) => {
                    self.stop.request();
                    break Ending::Stop(Some(request_id));
                }
                Some(op) if starts_work(&op) && previous_controller => {
                    WireEnvelope::response(request_id, Err(recovery_pending()))
                }
                op => {
                    if op.as_ref().is_some_and(starts_work) {
                        self.controlled = true;
                    }
                    #[cfg(feature = "devguard")]
                    let op = match op {
                        Some(op) if answered_beside(&op) => {
                            let (write, registrar, runner) =
                                (write.clone(), self.registrar.clone(), self.runner.clone());
                            beside.spawn(async move {
                                let result = answer_beside(op, registrar, runner).await;
                                let response = WireEnvelope::response(request_id, result);
                                let mut writer = write.lock().await;
                                let _ = write_frame(&mut *writer, &response).await;
                            });
                            continue;
                        }
                        op => op,
                    };
                    answer(&mut cache, &self.runner, request_id, op, &hello).await
                }
            };
            let mut writer = write.lock().await;
            if write_frame(&mut *writer, &response).await.is_err() {
                break Ending::Lost;
            }
        };
        if let Ending::Detach(request_id) = &ending {
            let response = WireEnvelope::response(request_id.clone(), Ok(RunnerOpResult::Detached));
            let mut writer = write.lock().await;
            let _ = write_frame(&mut *writer, &response).await;
        }
        // The connection's requests end before anything else: no later connection, and no
        // stop, overlaps them.
        while beside.join_next().await.is_some() {}
        let served = match ending {
            Ending::Detach(_) => Served::Detached,
            Ending::Lost => Served::Lost,
            Ending::Stop(request_id) => {
                let terminated = self.end_processes().await;
                if let Some(request_id) = request_id {
                    let response = WireEnvelope::response(
                        request_id,
                        Ok(RunnerOpResult::Stopped { terminated }),
                    );
                    let mut writer = write.lock().await;
                    let _ = write_frame(&mut *writer, &response).await;
                }
                Served::Stopped { terminated }
            }
        };
        let _ = stop_events.send(());
        let _ = forwarder.await;
        let _ = write.lock().await.shutdown().await;
        served
    }

    /// Stop with no connection open (P1R-C01): terminate every process and wait for them, as
    /// `Stop` does. How many were still running.
    pub async fn stop(&self) -> u32 {
        self.stop.request();
        self.end_processes().await
    }

    /// Terminate every process, those a request started after the stop request included, and
    /// wait until each is reaped and its output has ended, at most [`STOP_DRAIN_BOUND`]: a pipe
    /// process's own children may keep its output open, as a pipe process has no process group.
    /// How many processes the stop terminated.
    async fn end_processes(&self) -> u32 {
        let terminated = self.stop.terminate();
        let deadline = tokio::time::Instant::now() + STOP_DRAIN_BOUND;
        while self.runner.occupied_slots() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        terminated
    }
}

/// Why an independent Runner's connection stopped reading.
enum Ending {
    Detach(String),
    Lost,
    /// With the request's id when `Stop` asked for it.
    Stop(Option<String>),
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
        RunnerOp::Hello => unreachable!("hello is answered before dispatch"),
        RunnerOp::Replay { .. } => unreachable!("replay is handled before dispatch"),
        RunnerOp::Detach | RunnerOp::Stop => {
            unreachable!("detach and stop are handled before dispatch")
        }
        #[cfg(feature = "devguard")]
        RunnerOp::Registration => unreachable!("registration is handled before dispatch"),
        #[cfg(feature = "devguard")]
        RunnerOp::Prepare { .. } => unreachable!("preparation is handled before dispatch"),
        #[cfg(feature = "devguard")]
        RunnerOp::Launch { .. } => unreachable!("launch is handled before dispatch"),
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
    fn wire_protocol_is_v9() {
        assert_eq!(WIRE_PROTOCOL, 9);
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

    #[test]
    fn a_signaled_status_crosses_the_wire() {
        use codespace_domain::{ProcessSignal, ProcessState, ProcessTermination};
        let status = RunnerProcessStatus {
            process_id: ProcessId("proc-1".into()),
            state: ProcessState::Exited,
            exit_code: None,
            termination: Some(ProcessTermination::Signaled),
            signal: Some(ProcessSignal {
                number: 9,
                name: Some("SIGKILL".into()),
            }),
            output_total: 0,
            output_retained_from: 0,
            eof: true,
        };
        let envelope = WireEnvelope::response(
            "rrpc-1".into(),
            Ok(RunnerOpResult::ProcessStatus(status.clone())),
        );
        let parsed: WireEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        match parsed.result {
            Some(RunnerOpResult::ProcessStatus(got)) => assert_eq!(got, status),
            other => panic!("unexpected {other:?}"),
        }
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
            serve_launching(registration, None)
        }

        fn serve_launching(
            registration: Option<Arc<OwnerRegistration>>,
            launcher: Option<crate::launch::Launcher>,
        ) -> (
            tokio::net::unix::OwnedReadHalf,
            tokio::net::unix::OwnedWriteHalf,
        ) {
            let (client, server) = tokio::net::UnixStream::pair().unwrap();
            let (worker, events) = host_worker();
            tokio::spawn(async move {
                serve_runner_connection_with_registration(
                    server,
                    worker,
                    events,
                    registration,
                    launcher,
                )
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
                    lifecycle,
                    registration,
                    admission,
                    launch,
                    ..
                }) => assert_eq!(
                    (protocol, lifecycle, registration, admission, launch),
                    (WIRE_PROTOCOL, RunnerLifecycle::Managed, true, true, false)
                ),
                other => panic!("unexpected {other:?}"),
            }
            // Nor can it prepare a governed execution; it starts nothing for one.
            let dir = tempfile::tempdir().unwrap();
            let mut workspace = Workspace::new(
                codespace_domain::WorkspaceId("gov".into()),
                dir.path().to_path_buf(),
                codespace_domain::Profile::WorkspaceWrite,
            );
            workspace.resources.participation = codespace_policy::Participation::Required;
            let touch = dir.path().join("started").display().to_string();
            request(
                &mut write,
                "rrpc-prepare",
                RunnerOp::Prepare {
                    workspace,
                    request: RunnerExecRequest::for_host(
                        vec!["/usr/bin/touch".into(), touch],
                        ProcessId("proc-gov".into()),
                        codespace_domain::Profile::WorkspaceWrite,
                    ),
                    attempt_id: "cs-attempt-1".into(),
                },
            )
            .await;
            let reply = response(&mut read).await;
            assert_eq!(reply.request_id.as_deref(), Some("rrpc-prepare"));
            assert_eq!(
                reply.error.unwrap().code,
                ErrorCode::ResourcePolicyUnsupported
            );
            assert!(!dir.path().join("started").exists());
            // Nor launch one (CSRG-U4).
            let mut workspace = Workspace::new(
                codespace_domain::WorkspaceId("gov".into()),
                dir.path().to_path_buf(),
                codespace_domain::Profile::WorkspaceWrite,
            );
            workspace.resources.participation = codespace_policy::Participation::Required;
            request(
                &mut write,
                "rrpc-launch",
                RunnerOp::Launch {
                    workspace,
                    request: RunnerExecRequest::for_host(
                        vec![
                            "/usr/bin/touch".into(),
                            dir.path().join("started").display().to_string(),
                        ],
                        ProcessId("proc-gov".into()),
                        codespace_domain::Profile::WorkspaceWrite,
                    ),
                    attempt_id: "cs-attempt-2".into(),
                },
            )
            .await;
            let reply = response(&mut read).await;
            assert_eq!(reply.request_id.as_deref(), Some("rrpc-launch"));
            assert_eq!(
                reply.error.unwrap().code,
                ErrorCode::ResourcePolicyUnsupported
            );
            assert!(!dir.path().join("started").exists());
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
                    lifecycle: RunnerLifecycle::Managed,
                    identity: None,
                    previous_controller: false,
                    inherited_processes: 0,
                    registration: false,
                    admission: false,
                    launch: false,
                    ..
                }
            ));
        }

        /// A worker states `launch` only when it registers and has a launch helper (CSRG-U4).
        #[tokio::test]
        async fn only_a_registered_worker_with_a_launch_helper_states_launch() {
            let dir = private_directory();
            let owner = || {
                OwnerRegistration::new(
                    Owner::new(
                        OwnerSettings {
                            socket: dir.path().join("absent.sock"),
                            consumer: "codespace".into(),
                            generation: "g1".into(),
                        },
                        OwnerCredential::Unavailable,
                    ),
                    ResourceOwner::Worker,
                )
            };
            let launcher =
                || crate::launch::Launcher::new("/usr/libexec/devguard-launch".into()).ok();
            for (registration, launcher, expected) in [
                (Some(owner()), launcher(), true),
                (Some(owner()), None, false),
                (None, launcher(), false),
            ] {
                let (mut read, mut write) = serve_launching(registration, launcher);
                request(&mut write, "rrpc-hello", RunnerOp::Hello).await;
                match response(&mut read).await.result {
                    Some(RunnerOpResult::Hello { launch, .. }) => assert_eq!(launch, expected),
                    other => panic!("unexpected {other:?}"),
                }
            }
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
            // Once the session is open, process control on the same connection is answered
            // before the session ends. DevGuard reads the consumer secret within 250 ms of wall
            // time, so a session whose thread the scheduler held that long stops at the
            // credential although the file is intact: it opens nothing and is answered at once,
            // and the registration is asked for again.
            let mut sessions = Some(sessions);
            for attempt in 1..=5 {
                request(&mut write, "rrpc-reg", RunnerOp::Registration).await;
                let waiting = sessions.take().unwrap();
                let (opened, waiting) = tokio::task::spawn_blocking(move || {
                    (waiting.recv_timeout(Duration::from_secs(10)), waiting)
                })
                .await
                .unwrap();
                sessions = Some(waiting);
                if opened.is_ok() {
                    break;
                }
                let reply = response(&mut read).await;
                match reply.result {
                    Some(RunnerOpResult::Registration(info)) => assert_eq!(
                        info.registration.unwrap().state,
                        codespace_domain::ResourceRegistrationState::CredentialUnavailable
                    ),
                    other => panic!("unexpected {other:?}"),
                }
                assert!(attempt < 5, "no registration session opened");
            }
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
                    None,
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
                        let state = info.registration.unwrap().state;
                        // DevGuard reads the consumer secret within 250 ms of wall time; a
                        // session whose thread the scheduler held that long stops there and
                        // opens nothing.
                        if state == ResourceRegistrationState::CredentialUnavailable {
                            continue;
                        }
                        assert_eq!(state, ResourceRegistrationState::Unavailable);
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

        /// An independent Runner keeps its registration across its connections and states it in
        /// each `Hello`, and ends a connection only once its `Registration` requests were
        /// answered, so the next connection starts after them (P1R-C01).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_independent_runner_answers_registration_before_the_next_connection() {
            let dir = private_directory();
            let credential = dir.path().join("consumer.secret");
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&credential)
                .unwrap()
                .write_all("ef".repeat(32).as_bytes())
                .unwrap();
            // An authority that accepts each session and answers nothing, so a registration
            // session lasts until DevGuard's frame deadline.
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
            let (worker, events) = host_worker();
            let mut independent = IndependentRunner::new(worker, events).with_registration(
                Some(OwnerRegistration::new(owner, ResourceOwner::Worker)),
                None,
            );
            for connection in 0..2 {
                let (client, server) = tokio::net::UnixStream::pair().unwrap();
                let serving = tokio::spawn(async move {
                    let served = independent.serve(server).await;
                    (served, independent)
                });
                let (mut read, mut write) = client.into_split();
                request(&mut write, "rrpc-hello", RunnerOp::Hello).await;
                match response(&mut read).await.result {
                    Some(RunnerOpResult::Hello {
                        lifecycle,
                        registration,
                        admission,
                        launch,
                        ..
                    }) => assert_eq!(
                        (lifecycle, registration, admission, launch),
                        (RunnerLifecycle::Independent, true, true, false),
                        "connection {connection}"
                    ),
                    other => panic!("unexpected {other:?}"),
                }
                request(&mut write, "rrpc-reg", RunnerOp::Registration).await;
                request(&mut write, "rrpc-detach", RunnerOp::Detach).await;
                // Both are answered before the connection ends.
                let mut answered = Vec::new();
                while let Some(frame) = read_frame(&mut read).await.unwrap() {
                    let envelope: WireEnvelope = serde_json::from_slice(&frame).unwrap();
                    if envelope.kind == WireKind::Response {
                        answered.push(envelope.request_id.unwrap());
                    }
                }
                answered.sort();
                assert_eq!(answered, ["rrpc-detach", "rrpc-reg"]);
                let (served, returned) = serving.await.unwrap();
                assert_eq!(served, Served::Detached);
                independent = returned;
            }
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

    /// A managed worker refuses `Detach` and `Stop` and keeps serving; its `Hello` states its
    /// lifecycle and identity (P1R-C01).
    #[tokio::test]
    async fn a_managed_worker_refuses_detach_and_stop() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        for (id, op) in [
            ("rrpc-detach", RunnerOp::Detach),
            ("rrpc-stop", RunnerOp::Stop),
            ("rrpc-hello", RunnerOp::Hello),
        ] {
            write_frame(&mut write, &WireEnvelope::request(id.into(), op))
                .await
                .unwrap();
            let reply = read_frame(&mut read).await.unwrap().unwrap();
            let reply: WireEnvelope = serde_json::from_slice(&reply).unwrap();
            assert_eq!(reply.request_id.as_deref(), Some(id));
            match reply.result {
                Some(RunnerOpResult::Hello {
                    lifecycle,
                    identity,
                    previous_controller,
                    inherited_processes,
                    ..
                }) => {
                    assert_eq!(lifecycle, RunnerLifecycle::Managed);
                    assert_eq!(identity, Some(RunnerIdentity::this_process()));
                    assert!(!previous_controller);
                    assert_eq!(inherited_processes, 0);
                }
                None => assert_eq!(reply.error.unwrap().code, ErrorCode::InvalidCommand),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// An independent Runner across its connections, through the gateway's client (P1R-C01).
    mod independent {
        use super::*;
        use crate::{RunnerApplyPatchRequest, RunnerReadProcess, RunnerWriteStdin, UdsRunner};
        use codespace_domain::{ProcessState, ProcessTermination, Profile, WorkspaceId};
        use std::path::Path;
        use std::time::Instant;

        /// An independent Runner that serves the streams sent to it in turn, as the worker binary
        /// serves each connection it accepts. Dropping it stops the Runner.
        struct Host {
            streams: mpsc::UnboundedSender<tokio::net::UnixStream>,
            served: mpsc::UnboundedReceiver<Served>,
            stop: StopHandle,
            runner: InProcessRunner,
        }

        impl Host {
            fn start() -> Self {
                let (runner, events) = host_worker();
                let independent = IndependentRunner::new(runner.clone(), events);
                let stop = independent.stop_handle();
                let (streams, incoming) = mpsc::unbounded_channel::<tokio::net::UnixStream>();
                let incoming = Arc::new(Mutex::new(incoming));
                let (report, served) = mpsc::unbounded_channel();
                tokio::spawn(independent.run(
                    move || {
                        let incoming = incoming.clone();
                        async move {
                            incoming
                                .lock()
                                .await
                                .recv()
                                .await
                                .ok_or_else(|| std::io::Error::other("no more connections"))
                        }
                    },
                    move |ending| {
                        if let Ok(ending) = ending {
                            let _ = report.send(ending);
                        }
                    },
                ));
                Self {
                    streams,
                    served,
                    stop,
                    runner,
                }
            }

            /// A new gateway's connection, after its handshake.
            async fn connect(&self) -> UdsRunner {
                self.connect_reporting_exits(Arc::new(|_: &str| {})).await
            }

            async fn connect_reporting_exits(&self, on_exit: ShellRelease) -> UdsRunner {
                let (client, server) = tokio::net::UnixStream::pair().unwrap();
                self.streams.send(server).unwrap();
                let gateway = UdsRunner::from_stream(client, on_exit);
                gateway.handshake().await.unwrap();
                gateway
            }

            /// A connection the gateway's client does not speak for.
            fn connect_raw(&self) -> tokio::net::UnixStream {
                let (client, server) = tokio::net::UnixStream::pair().unwrap();
                self.streams.send(server).unwrap();
                client
            }

            /// How the connection being served ended.
            async fn served(&mut self) -> Served {
                tokio::time::timeout(Duration::from_secs(20), self.served.recv())
                    .await
                    .expect("the connection ends")
                    .expect("the Runner reports it")
            }
        }

        impl Drop for Host {
            fn drop(&mut self) {
                self.stop.request();
            }
        }

        fn workspace(root: &Path) -> Workspace {
            Workspace::new(
                WorkspaceId("demo".into()),
                root.to_path_buf(),
                Profile::WorkspaceWrite,
            )
        }

        fn command(
            argv: &[&str],
            process_id: &str,
            timeout_ms: u64,
            tty: bool,
        ) -> RunnerExecRequest {
            let mut request = RunnerExecRequest::for_host(
                argv.iter().map(|arg| arg.to_string()).collect(),
                ProcessId(process_id.into()),
                Profile::WorkspaceWrite,
            );
            request.timeout_ms = timeout_ms;
            request.tty = tty;
            request
        }

        fn code(err: RunnerError) -> ErrorCode {
            match err.as_execution() {
                Some(body) => body.code,
                None => panic!("{err:?}"),
            }
        }

        /// The process's output from its start, once it contains `expected`.
        async fn output_with(gateway: &UdsRunner, process_id: &ProcessId, expected: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let read = gateway
                    .read_process(RunnerReadProcess {
                        process_id: process_id.clone(),
                        cursor: 0,
                    })
                    .await
                    .unwrap();
                if read.chunk.contains(expected) {
                    return;
                }
                assert!(Instant::now() < deadline, "{process_id:?}: {read:?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn exited<R: Runner>(runner: &R, process_id: &ProcessId) -> RunnerProcessStatus {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let status = runner.process_status(process_id).await.unwrap();
                if status.state == ProcessState::Exited {
                    return status;
                }
                assert!(Instant::now() < deadline, "{status:?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn next_exit(exited_ids: &mut mpsc::UnboundedReceiver<String>) -> Option<String> {
            tokio::time::timeout(Duration::from_secs(10), exited_ids.recv())
                .await
                .expect("an exit event")
        }

        async fn disconnected(gateway: &UdsRunner, process_id: &ProcessId) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match gateway.process_status(process_id).await {
                    Err(RunnerError::Execution(body))
                        if body.code == ErrorCode::RunnerDisconnected =>
                    {
                        return
                    }
                    other => assert!(Instant::now() < deadline, "{other:?}"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// After a detach the next gateway reads the output and status of the same process_id,
        /// writes to it and resizes it, and the process times out at its original deadline.
        /// That gateway starts nothing new, also once the inherited processes ended.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn the_next_gateway_inherits_the_processes_but_starts_nothing() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let first = host.connect().await;
            let hello = first.hello();
            assert_eq!(hello.lifecycle, RunnerLifecycle::Independent);
            assert_eq!(hello.identity, Some(RunnerIdentity::this_process()));
            assert!(!hello.previous_controller);
            assert_eq!(hello.inherited_processes, 0);
            assert!(!first.refuses_new_work());
            let pipe = ProcessId("proc-pipe".into());
            let tty = ProcessId("proc-tty".into());
            let started = Instant::now();
            first
                .exec(
                    &ws,
                    command(
                        &[
                            "/bin/sh",
                            "-c",
                            "echo ready; read line; echo got $line; exec sleep 30",
                        ],
                        &pipe.0,
                        3_000,
                        false,
                    ),
                )
                .await
                .unwrap();
            first
                .exec(
                    &ws,
                    command(
                        &["/bin/sh", "-c", "echo tty-ready; exec sleep 30"],
                        &tty.0,
                        60_000,
                        true,
                    ),
                )
                .await
                .unwrap();
            output_with(&first, &pipe, "ready").await;
            first.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);
            // The gateway that detached reaches the Runner no more.
            assert_eq!(
                code(first.process_status(&pipe).await.unwrap_err()),
                ErrorCode::RunnerDisconnected
            );

            let second = host.connect().await;
            let inherited = second.hello();
            assert_eq!(inherited.identity, hello.identity);
            assert!(inherited.previous_controller);
            assert_eq!(inherited.inherited_processes, 2);
            assert!(second.refuses_new_work());
            output_with(&second, &pipe, "ready").await;
            second
                .write_stdin(RunnerWriteStdin {
                    process_id: pipe.clone(),
                    data: "hello\n".into(),
                })
                .await
                .unwrap();
            output_with(&second, &pipe, "got hello").await;
            output_with(&second, &tty, "tty-ready").await;
            assert_eq!(
                second.resize(&tty, 40, 100).await.unwrap(),
                RunnerResizeResult {
                    rows: 40,
                    cols: 100
                }
            );
            let status = exited(&second, &pipe).await;
            let elapsed = started.elapsed();
            assert_eq!(
                status.termination,
                Some(ProcessTermination::Timeout),
                "{status:?}"
            );
            assert!(
                elapsed >= Duration::from_millis(3_000) && elapsed < Duration::from_secs(10),
                "{elapsed:?}"
            );
            // Nothing new starts, and the workspace is not changed.
            let marker = root.path().join("started");
            let touch = command(
                &["/usr/bin/touch", &marker.display().to_string()],
                "proc-new",
                5_000,
                false,
            );
            assert_eq!(
                code(second.exec(&ws, touch.clone()).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            let patch = RunnerApplyPatchRequest {
                patch: "*** Begin Patch\n*** Add File: added.txt\n+added\n*** End Patch\n".into(),
                expected_versions: Default::default(),
                check_only: false,
            };
            assert_eq!(
                code(second.apply_patch(&ws, patch.clone()).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            // An inherited process still ends on request.
            second.terminate(&tty).await.unwrap();
            assert_eq!(
                exited(&second, &tty).await.termination,
                Some(ProcessTermination::Terminated)
            );
            // The barrier stays once the inherited processes ended.
            second.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);
            let third = host.connect().await;
            assert!(third.hello().previous_controller);
            assert_eq!(third.hello().inherited_processes, 0);
            assert!(third.refuses_new_work());
            assert_eq!(
                code(third.exec(&ws, touch).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            assert_eq!(
                code(third.apply_patch(&ws, patch).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            assert!(!marker.exists());
            assert!(!root.path().join("added.txt").exists());
            assert_eq!(third.stop_runner().await.unwrap(), 0);
            assert_eq!(host.served().await, Served::Stopped { terminated: 0 });
        }

        /// Only a connection that starts work becomes the controller: one that only read leaves
        /// the next gateway free to start work.
        #[tokio::test]
        async fn a_gateway_that_starts_nothing_leaves_no_controller() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let reader = host.connect().await;
            reader.find(&ws, None, None, None).await.unwrap();
            reader.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);
            let worker = host.connect().await;
            assert!(!worker.hello().previous_controller);
            assert!(!worker.refuses_new_work());
            worker
                .exec(
                    &ws,
                    command(&["/bin/sh", "-c", "exit 0"], "proc-ok", 5_000, false),
                )
                .await
                .unwrap();
            worker.close().await;
            assert_eq!(host.served().await, Served::Lost);
            assert!(host.connect().await.hello().previous_controller);
        }

        /// An explicit stop ends the processes, a tty's process group included, and the Runner.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_stop_ends_the_processes_and_the_runner() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let gateway = host.connect().await;
            let pipe = ProcessId("proc-pipe".into());
            let tty = ProcessId("proc-tty".into());
            gateway
                .exec(&ws, command(&["/bin/sleep", "30"], &pipe.0, 60_000, false))
                .await
                .unwrap();
            gateway
                .exec(
                    &ws,
                    command(
                        &["/bin/sh", "-c", "echo tty-ready; exec sleep 30"],
                        &tty.0,
                        60_000,
                        true,
                    ),
                )
                .await
                .unwrap();
            output_with(&gateway, &tty, "tty-ready").await;
            let started = Instant::now();
            assert_eq!(gateway.stop_runner().await.unwrap(), 2);
            assert!(
                started.elapsed() < STOP_DRAIN_BOUND,
                "{:?}",
                started.elapsed()
            );
            assert_eq!(host.served().await, Served::Stopped { terminated: 2 });
            assert_eq!(host.runner.occupied_slots(), 0);
            for process_id in [&pipe, &tty] {
                let status = exited(&host.runner, process_id).await;
                assert_eq!(
                    status.termination,
                    Some(ProcessTermination::Terminated),
                    "{status:?}"
                );
            }
            assert_eq!(
                code(gateway.process_status(&pipe).await.unwrap_err()),
                ErrorCode::RunnerDisconnected
            );
        }

        /// A stop request, as the Runner's own SIGTERM or SIGINT makes, ends the connection being
        /// served and the processes; the gateway's later requests are refused.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_stop_request_ends_the_connection_and_the_processes() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let gateway = host.connect().await;
            let pipe = ProcessId("proc-pipe".into());
            gateway
                .exec(&ws, command(&["/bin/sleep", "30"], &pipe.0, 60_000, false))
                .await
                .unwrap();
            host.stop.request();
            assert_eq!(host.served().await, Served::Stopped { terminated: 1 });
            assert_eq!(
                exited(&host.runner, &pipe).await.termination,
                Some(ProcessTermination::Terminated)
            );
            disconnected(&gateway, &pipe).await;
        }

        /// A connection that ends without a word is a loss: the processes keep running, and an
        /// exit while no gateway is connected reaches the next one.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_lost_connection_keeps_the_processes_and_their_exit_events() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let first = host.connect().await;
            let quick = ProcessId("proc-quick".into());
            let long = ProcessId("proc-long".into());
            first
                .exec(&ws, command(&["/bin/sleep", "2"], &quick.0, 60_000, false))
                .await
                .unwrap();
            first
                .exec(&ws, command(&["/bin/sleep", "30"], &long.0, 60_000, false))
                .await
                .unwrap();
            first.close().await;
            assert_eq!(host.served().await, Served::Lost);
            assert_eq!(
                exited(&host.runner, &quick).await.termination,
                Some(ProcessTermination::Exited)
            );
            let (exits, mut exited_ids) = mpsc::unbounded_channel();
            let second = host
                .connect_reporting_exits(Arc::new(move |process_id: &str| {
                    let _ = exits.send(process_id.to_string());
                }))
                .await;
            assert!(second.hello().previous_controller);
            assert_eq!(second.hello().inherited_processes, 1);
            assert_eq!(
                next_exit(&mut exited_ids).await.as_deref(),
                Some("proc-quick")
            );
            assert_eq!(
                second.process_status(&long).await.unwrap().state,
                ProcessState::Running
            );
            second.terminate(&long).await.unwrap();
            assert_eq!(
                next_exit(&mut exited_ids).await.as_deref(),
                Some("proc-long")
            );
        }

        /// A gateway whose connection ends right after its `Detach` or `Stop` still detaches or
        /// stops: the Runner acts on what it read, though its answer is lost.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_detach_or_stop_whose_answer_is_lost_still_holds() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let gateway = host.connect().await;
            let long = ProcessId("proc-long".into());
            gateway
                .exec(&ws, command(&["/bin/sleep", "30"], &long.0, 60_000, false))
                .await
                .unwrap();
            gateway.close().await;
            assert_eq!(host.served().await, Served::Lost);
            for (op, ending) in [
                (RunnerOp::Detach, Served::Detached),
                (RunnerOp::Stop, Served::Stopped { terminated: 1 }),
            ] {
                let (_, mut write) = host.connect_raw().into_split();
                write_frame(&mut write, &WireEnvelope::request("rrpc-end".into(), op))
                    .await
                    .unwrap();
                drop(write);
                assert_eq!(host.served().await, ending);
                assert_eq!(
                    host.runner.process_status(&long).await.unwrap().state == ProcessState::Running,
                    ending == Served::Detached
                );
            }
            assert_eq!(
                exited(&host.runner, &long).await.termination,
                Some(ProcessTermination::Terminated)
            );
        }

        /// When a call outlives the gateway's deadline the gateway closes the connection: its
        /// later requests are refused, and the Runner treats the closed connection as a loss and
        /// keeps the processes.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_call_past_its_deadline_closes_the_connection_and_keeps_the_processes() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let gateway = host
                .connect()
                .await
                .with_call_deadline(Duration::from_millis(500));
            // A process that never reads its stdin: a large write to it waits for its timeout.
            let stuck = ProcessId("proc-stuck".into());
            let long = ProcessId("proc-long".into());
            gateway
                .exec(&ws, command(&["/bin/sleep", "30"], &stuck.0, 2_000, false))
                .await
                .unwrap();
            gateway
                .exec(&ws, command(&["/bin/sleep", "30"], &long.0, 60_000, false))
                .await
                .unwrap();
            let err = gateway
                .write_stdin(RunnerWriteStdin {
                    process_id: stuck.clone(),
                    data: "x".repeat(4 * 1024 * 1024),
                })
                .await
                .unwrap_err();
            assert!(
                matches!(err, RunnerError::TransportAmbiguous { .. }),
                "{err:?}"
            );
            assert_eq!(
                code(gateway.process_status(&long).await.unwrap_err()),
                ErrorCode::RunnerDisconnected
            );
            assert_eq!(host.served().await, Served::Lost);
            let next = host.connect().await;
            assert_eq!(
                next.process_status(&long).await.unwrap().state,
                ProcessState::Running
            );
            assert_eq!(
                exited(&next, &stuck).await.termination,
                Some(ProcessTermination::Timeout)
            );
        }
    }
}
