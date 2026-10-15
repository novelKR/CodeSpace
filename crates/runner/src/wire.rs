//! CodeSpace-owned runner RPC. Not App Server and not MCP.
//!
//! Framing is `u32` big-endian length + JSON. Kinds share one stream:
//! request, response, and event. `request_id` (`rrpc-…`) is not an HTTP
//! id, `operation_id`, `operation_key`, or `process_id`.
//!
//! A `managed` worker serves the one connection of the gateway that started it. An
//! `independent` Runner (P1R-C01) serves its connections through [`IndependentRunner`]: its
//! processes, their output, their timeouts and their exit events outlive each connection. A
//! connection controls the Runner only after its `Claim` (P1R-C03), and every request that
//! changes a process or the workspace carries the epoch of that Claim, which the Runner checks.
//! A controller after one that started work in a workspace starts nothing there until it
//! reconciled that workspace (P1R-C04).

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::task::JoinSet;

use crate::lifecycle::{RunnerIdentity, RunnerLifecycle};
use crate::{
    InProcessRunner, Recalled, Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult,
    RunnerError, RunnerExecRequest, RunnerExecResult, RunnerProcessStatus, RunnerReadProcess,
    RunnerReadResult, RunnerResizeResult, RunnerWriteStdin, ShellRelease,
    DEFAULT_MAX_UNACKNOWLEDGED,
};

pub const WIRE_PROTOCOL: u32 = 11;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLAY: usize = 32;
/// How long a stopping independent Runner waits for its terminated processes to be reaped and
/// their output to end before it exits anyway (P1R-C01).
pub const STOP_DRAIN_BOUND: Duration = Duration::from_secs(5);
/// How long an independent Runner's `Claim` waits for the requests of the connection that
/// controlled it before, once that connection has ended, before it is refused (P1R-C03).
pub const CLAIM_WAIT_BOUND: Duration = Duration::from_secs(10);
/// How often a waiting `Claim` looks again, besides each change of control.
const CLAIM_POLL: Duration = Duration::from_millis(20);
/// How many of a connection's requests an independent Runner reads ahead of the one it
/// answers, so that it sees the connection end while a request runs.
const READ_AHEAD: usize = 8;

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
    /// The controller epoch a request is sent under (P1R-C03): an independent Runner refuses a
    /// request that changes a process or the workspace unless it carries the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<RunnerOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RunnerOpResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    /// An event's number: the worker numbers its exit events from 1 in the order they
    /// happened (P1R-C03).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<RunnerEvent>,
}

impl WireEnvelope {
    pub fn request(request_id: String, op: RunnerOp) -> Self {
        Self {
            protocol: WIRE_PROTOCOL,
            kind: WireKind::Request,
            request_id: Some(request_id),
            epoch: None,
            op: Some(op),
            ok: None,
            result: None,
            error: None,
            seq: None,
            event: None,
        }
    }

    /// This request, sent under the controller epoch `epoch` (P1R-C03).
    pub fn with_epoch(mut self, epoch: Option<u64>) -> Self {
        self.epoch = epoch;
        self
    }

    pub fn response(request_id: String, result: Result<RunnerOpResult, ErrorBody>) -> Self {
        let (ok, result, error) = match result {
            Ok(value) => (true, Some(value), None),
            Err(error) => (false, None, Some(error)),
        };
        Self {
            protocol: WIRE_PROTOCOL,
            kind: WireKind::Response,
            request_id: Some(request_id),
            epoch: None,
            op: None,
            ok: Some(ok),
            result,
            error,
            seq: None,
            event: None,
        }
    }

    pub fn event(seq: u64, event: RunnerEvent) -> Self {
        Self {
            protocol: WIRE_PROTOCOL,
            kind: WireKind::Event,
            request_id: None,
            epoch: None,
            op: None,
            ok: None,
            result: None,
            error: None,
            seq: Some(seq),
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
    /// Take control of an independent Runner for the operations database `store_id`
    /// (P1R-C03). Answered `Claimed` with a new controller epoch, or `ClaimRefused`. A managed
    /// worker refuses it.
    Claim {
        store_id: String,
    },
    /// The gateway leaves an independent Runner and its processes keep running (P1R-C01). The
    /// Runner answers `Detached`, reads nothing more from this connection and lets another one
    /// claim it once this connection's requests have ended. A managed worker refuses it.
    Detach,
    /// Stop an independent Runner (P1R-C01): it reads nothing more, lets the requests in
    /// progress end, terminates every process, waits for them, answers `Stopped` and exits.
    /// Only its controller stops it (P1R-C03). A managed worker refuses it.
    Stop,
    /// What an independent Runner knows of each of these processes (P1R-C04), answered
    /// `Recalled`: whether a request to start it reached the Runner, whether the Runner created
    /// it, and its status while the Runner holds it. Only its controller asks. A managed
    /// worker refuses it.
    Recall {
        process_ids: Vec<ProcessId>,
    },
    /// The controller reconciled these workspaces after its Claim (P1R-C04): the Runner starts
    /// work there for it again, answering `Reconciled`. A managed worker refuses it.
    Reconciled {
        workspace_ids: Vec<String>,
    },
    /// The controller recorded how these processes ended (P1R-C04), answered `Acknowledged`:
    /// the Runner evicts them as any other ended process from now on, instead of keeping them
    /// for a controller. A managed worker refuses it.
    Acknowledge {
        process_ids: Vec<ProcessId>,
    },
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
    /// Look up, with DevGuard, the attempts this independent Runner admitted for these
    /// processes (P1R-C04), answered `LookedUp`. Only its controller asks. A managed worker
    /// refuses it.
    #[cfg(feature = "devguard")]
    Lookup {
        attempts: Vec<crate::admission::AttemptRef>,
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
    /// An independent Runner's answer to a `Claim` it accepted (P1R-C03).
    Claimed(RunnerClaim),
    /// An independent Runner's answer to a `Claim` it refused (P1R-C03): the connection may
    /// ask again, and nothing else changed.
    ClaimRefused {
        refusal: ClaimRefusal,
        message: String,
    },
    /// An independent Runner's answer to `Detach` (P1R-C01).
    Detached,
    /// An independent Runner's answer to `Stop`, sent once its processes ended or
    /// [`STOP_DRAIN_BOUND`] passed (P1R-C01): how many processes were still running when it
    /// terminated them, and every process it held then, with its last status (P1R-C04).
    Stopped {
        terminated: u32,
        #[serde(default)]
        processes: Vec<RunnerInventoryEntry>,
    },
    /// An independent Runner's answer to `Recall` (P1R-C04), in the order asked.
    Recalled(Vec<RecalledProcess>),
    /// An independent Runner's answer to `Reconciled` (P1R-C04).
    Reconciled,
    /// An independent Runner's answer to `Acknowledge` (P1R-C04).
    Acknowledged,
    #[cfg(feature = "devguard")]
    Registration(codespace_domain::ResourceAuthorityInfo),
    #[cfg(feature = "devguard")]
    Prepared(crate::admission::ManagedPreparation),
    #[cfg(feature = "devguard")]
    Launched(crate::launch::LaunchReport),
    /// An independent Runner's answer to `Lookup` (P1R-C04), in the order asked.
    #[cfg(feature = "devguard")]
    LookedUp(Vec<crate::admission::AttemptLookup>),
}

/// What a connection that claimed an independent Runner controls (P1R-C03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerClaim {
    /// The controller epoch of this Claim, one more than the one before. Every request of this
    /// connection that changes a process or the workspace carries it.
    pub epoch: u64,
    /// An earlier controller started work: in each workspace of `barrier` (P1R-C01).
    pub previous_controller: bool,
    /// The workspaces in which an earlier controller started work (P1R-C04). This connection
    /// starts nothing in one until it states that it reconciled it.
    #[serde(default)]
    pub barrier: Vec<String>,
    /// Every process the Runner holds when the Claim is answered, those that ended included.
    pub processes: Vec<RunnerInventoryEntry>,
    /// The number of the last exit event before the inventory was taken: an event numbered up
    /// to it is already reflected in the inventory. Events this connection receives may start
    /// below it, with those no controller received.
    pub events_through: u64,
}

/// A process an independent Runner holds (P1R-C03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerInventoryEntry {
    pub workspace_id: String,
    pub status: RunnerProcessStatus,
}

/// What an independent Runner knows of one process (P1R-C04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecalledProcess {
    pub process_id: ProcessId,
    pub recalled: Recalled,
}

/// Why an independent Runner refused a `Claim` (P1R-C03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimRefusal {
    /// The connection's peer runs as another user than the Runner, or its credentials could
    /// not be read.
    PeerUid,
    /// The Runner was first claimed for another operations database.
    StoreMismatch,
    /// Another connection controls the Runner and is still open, or claims it now.
    ControllerActive,
    /// The connection that controlled the Runner ended, but its requests did not end within
    /// [`CLAIM_WAIT_BOUND`].
    RequestsRunning,
    /// The Runner is stopping.
    Stopping,
}

impl ClaimRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PeerUid => "peer_uid",
            Self::StoreMismatch => "store_mismatch",
            Self::ControllerActive => "controller_active",
            Self::RequestsRunning => "requests_running",
            Self::Stopping => "stopping",
        }
    }

    /// Asking again later may succeed: the controller may end, or its requests may.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::ControllerActive | Self::RequestsRunning)
    }
}

#[cfg(feature = "devguard")]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The refusal of new work in a workspace where an earlier controller of an independent Runner
/// started work, which this controller has not reconciled yet (P1R-C01, P1R-C04).
pub fn recovery_pending(workspace_id: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RecoveryPending,
        format!(
            "an earlier gateway started work in workspace `{workspace_id}` on this independent \
             Runner, and this gateway has not reconciled it yet: its processes can be queried, \
             read, written to, resized and terminated, but nothing new is started there"
        ),
    )
}

/// The refusal of a request that does not come from an independent Runner's current
/// controller (P1R-C03).
fn stale_controller(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::StaleController, message)
}

/// The worker's registration, when it was started with one, and its launch helper.
#[cfg(feature = "devguard")]
#[derive(Clone, Default)]
struct Registrar {
    registration: Option<Arc<crate::OwnerRegistration>>,
    launcher: Option<crate::launch::Launcher>,
    /// The admissions an independent Runner asked for, to look them up again (P1R-C04).
    admissions: Option<crate::admission::Admissions>,
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
    identity: &RunnerIdentity,
) -> RunnerOpResult {
    #[cfg(not(feature = "devguard"))]
    let () = *registrar;
    RunnerOpResult::Hello {
        protocol: WIRE_PROTOCOL,
        lifecycle,
        identity: Some(identity.clone()),
        #[cfg(feature = "devguard")]
        registration: true,
        #[cfg(feature = "devguard")]
        admission: true,
        // A worker states `launch` only with a registration and a launch helper.
        #[cfg(feature = "devguard")]
        launch: registrar.launches(),
    }
}

/// The exit events of a host worker's processes, numbered from 1 in the order the processes
/// ended (P1R-C03).
pub struct RunnerEvents {
    receiver: mpsc::UnboundedReceiver<(u64, RunnerEvent)>,
    /// The number of the last event, shared with the worker's release hook.
    numbered: Arc<std::sync::Mutex<u64>>,
}

/// Host worker: `InProcessRunner` plus its numbered `ProcessExited` events.
pub fn host_worker() -> (InProcessRunner, RunnerEvents) {
    let (tx, receiver) = mpsc::unbounded_channel();
    let numbered = Arc::new(std::sync::Mutex::new(0u64));
    let numbering = numbered.clone();
    let on_release: ShellRelease = Arc::new(move |process_id: &str| {
        // Numbered and queued under one lock, so the queue's order is the numbers' order.
        let mut last = numbering.lock().unwrap_or_else(PoisonError::into_inner);
        *last += 1;
        let _ = tx.send((
            *last,
            RunnerEvent::ProcessExited {
                process_id: ProcessId(process_id.to_string()),
            },
        ));
    });
    (
        InProcessRunner::new(on_release),
        RunnerEvents { receiver, numbered },
    )
}

pub async fn serve_runner_connection<S>(
    stream: S,
    runner: InProcessRunner,
    events: RunnerEvents,
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
    events: RunnerEvents,
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
            admissions: None,
        },
    )
    .await
}

/// A managed worker's one connection.
async fn serve<S>(
    stream: S,
    runner: InProcessRunner,
    events: RunnerEvents,
    registrar: Registrar,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read, write) = tokio::io::split(stream);
    let write = Arc::new(Mutex::new(write));
    let event_write = write.clone();
    let mut events = events.receiver;
    tokio::spawn(async move {
        while let Some((seq, event)) = events.recv().await {
            let mut writer = event_write.lock().await;
            if write_frame(&mut *writer, &WireEnvelope::event(seq, event))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let hello = hello(
        &registrar,
        RunnerLifecycle::Managed,
        &RunnerIdentity::this_process(),
    );
    let mut cache: VecDeque<(String, WireEnvelope)> = VecDeque::new();
    loop {
        let payload = match read_frame(&mut read).await? {
            Some(payload) => payload,
            None => return Ok(()),
        };
        let (request_id, _, op) = match parse_request(&payload) {
            Ok(request) => request,
            Err(response) => {
                let mut writer = write.lock().await;
                write_frame(&mut *writer, &response).await?;
                return Ok(());
            }
        };
        #[cfg(feature = "devguard")]
        let op = match op {
            Some(op) if answered_beside(&op) && !independent_only(&op) => {
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
            Some(op) if independent_only(&op) => WireEnvelope::response(
                request_id,
                Err(ErrorBody::new(
                    ErrorCode::InvalidCommand,
                    "this worker belongs to the gateway that started it; only an independent \
                     Runner is claimed, detaches, stops on request or is reconciled",
                )),
            ),
            op => answer(&mut cache, &runner, request_id, op, &hello).await,
        };
        let mut writer = write.lock().await;
        write_frame(&mut *writer, &response).await?;
    }
}

/// A request that only an independent Runner answers (P1R-C01, P1R-C03, P1R-C04).
fn independent_only(op: &RunnerOp) -> bool {
    match op {
        RunnerOp::Claim { .. }
        | RunnerOp::Detach
        | RunnerOp::Stop
        | RunnerOp::Recall { .. }
        | RunnerOp::Reconciled { .. }
        | RunnerOp::Acknowledge { .. } => true,
        #[cfg(feature = "devguard")]
        RunnerOp::Lookup { .. } => true,
        _ => false,
    }
}

/// A request's id, epoch and operation, or the response that refuses it and ends the
/// connection.
fn parse_request(
    payload: &[u8],
) -> Result<(String, Option<u64>, Option<RunnerOp>), Box<WireEnvelope>> {
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
    Ok((
        request.request_id.unwrap_or_default(),
        request.epoch,
        request.op,
    ))
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
        RunnerOp::Registration
            | RunnerOp::Prepare { .. }
            | RunnerOp::Launch { .. }
            | RunnerOp::Lookup { .. }
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
                    .with_admissions(registrar.admissions)
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
                    .with_admissions(registrar.admissions)
                    .prepare_and_launch(workspace, request, attempt_id)
                    .await,
            )),
            _ => Err(ErrorBody::new(
                ErrorCode::ResourcePolicyUnsupported,
                "this worker was started without DevGuard settings or a launch helper",
            )),
        },
        // An attempt this Runner cannot ask about is reported as a failed lookup.
        RunnerOp::Lookup { attempts } => Ok(RunnerOpResult::LookedUp(
            crate::admission::look_up(
                registrar.registration.map(
                    |registration| -> Arc<dyn crate::admission::AttemptAuthority> {
                        registration.owner()
                    },
                ),
                &registrar.admissions.unwrap_or_default(),
                attempts,
            )
            .await,
        )),
        other => unreachable!("{other:?} is answered in turn"),
    }
}

/// The workspace of a request that starts work an earlier connection's successor could not
/// know about: a process, a change to the workspace or an admission (P1R-C01). The first one
/// accepted from a controller keeps every later controller from starting work in that
/// workspace until it reconciled it (P1R-C04).
fn starts_work(op: &RunnerOp) -> Option<&str> {
    match op {
        RunnerOp::Exec { workspace, .. } | RunnerOp::ApplyPatch { workspace, .. } => {
            Some(&workspace.id.0)
        }
        #[cfg(feature = "devguard")]
        RunnerOp::Prepare { workspace, .. } | RunnerOp::Launch { workspace, .. } => {
            Some(&workspace.id.0)
        }
        _ => None,
    }
}

/// The process that a request asks the Runner to start (P1R-C04).
fn started_process(op: &RunnerOp) -> Option<&ProcessId> {
    match op {
        RunnerOp::Exec { request, .. } => Some(&request.process_id),
        #[cfg(feature = "devguard")]
        RunnerOp::Prepare { request, .. } | RunnerOp::Launch { request, .. } => {
            Some(&request.process_id)
        }
        _ => None,
    }
}

/// A request that changes a process or the workspace, or stops the Runner: an independent
/// Runner refuses it unless it carries the current controller epoch (P1R-C03).
fn fenced(op: &RunnerOp) -> bool {
    match op {
        RunnerOp::Exec { .. }
        | RunnerOp::ApplyPatch { .. }
        | RunnerOp::WriteStdin { .. }
        | RunnerOp::Resize { .. }
        | RunnerOp::Terminate { .. }
        | RunnerOp::TerminateWorkspace { .. }
        | RunnerOp::Stop
        | RunnerOp::Reconciled { .. }
        | RunnerOp::Acknowledge { .. } => true,
        #[cfg(feature = "devguard")]
        RunnerOp::Prepare { .. } | RunnerOp::Launch { .. } => true,
        _ => false,
    }
}

/// How an independent Runner's controlling connection ended (P1R-C01).
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

/// The exit events of an independent Runner's processes, kept between its controllers.
struct EventHub {
    events: mpsc::UnboundedReceiver<(u64, RunnerEvent)>,
    /// An event whose write failed, which the next controller gets first.
    undelivered: Option<(u64, RunnerEvent)>,
}

/// Forward exit events to the controller's connection until `stop`, or until a write fails.
/// Events left in the hub go to the next controller.
async fn forward_events<W>(
    hub: Arc<Mutex<EventHub>>,
    write: Arc<Mutex<W>>,
    mut stop: oneshot::Receiver<()>,
) where
    W: AsyncWrite + Unpin,
{
    let mut hub = hub.lock().await;
    loop {
        let (seq, event) = match hub.undelivered.take() {
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
        if write_frame(&mut *writer, &WireEnvelope::event(seq, event.clone()))
            .await
            .is_err()
        {
            hub.undelivered = Some((seq, event));
            return;
        }
    }
}

/// What an independent Runner tells its operator about each `Claim` (P1R-C03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimNote {
    Claimed {
        epoch: u64,
        previous_controller: bool,
        peer_pid: Option<i32>,
    },
    Refused {
        refusal: ClaimRefusal,
        message: String,
        peer_pid: Option<i32>,
    },
}

pub type ClaimObserver = Arc<dyn Fn(ClaimNote) + Send + Sync>;

/// An independent Runner (P1R-C01), started by an operator or a harness rather than a gateway.
/// It serves any number of connections at once, and its processes, their output, their
/// timeouts and their exit events outlive each connection. One connection at a time controls
/// it, after its `Claim` (P1R-C03): only that connection's requests are answered beyond `Hello`
/// and `Claim`, only it receives the exit events, and each of its requests that changes a
/// process or the workspace must carry the epoch of its Claim. A Claim is refused while
/// another connection controls the Runner and is open; once that connection ends, the Claim
/// waits up to [`CLAIM_WAIT_BOUND`] for its requests. A controller after one that started work
/// in a workspace may observe and control the processes it inherits, but starts nothing in
/// that workspace until it states that it reconciled it (P1R-C04). The Runner keeps what its
/// controllers need for that: whether it created a process for each `process_id` it was asked
/// to start, and each ended process until a controller acknowledges it.
pub struct IndependentRunner {
    runner: InProcessRunner,
    events: RunnerEvents,
    registrar: Registrar,
    stop: StopHandle,
    identity: RunnerIdentity,
    owner_uid: u32,
    claim_wait: Duration,
    observer: Option<ClaimObserver>,
}

impl IndependentRunner {
    pub fn new(runner: InProcessRunner, events: RunnerEvents) -> Self {
        runner.keep_until_acknowledged(DEFAULT_MAX_UNACKNOWLEDGED);
        let stop = StopHandle {
            requested: Arc::new(watch::Sender::new(false)),
            terminated: Arc::new(AtomicU32::new(0)),
            runner: runner.clone(),
        };
        Self {
            runner,
            events,
            #[cfg(feature = "devguard")]
            registrar: Registrar::default(),
            #[cfg(not(feature = "devguard"))]
            registrar: (),
            stop,
            identity: RunnerIdentity::this_process(),
            // SAFETY: geteuid has no preconditions and cannot fail.
            owner_uid: unsafe { libc::geteuid() },
            claim_wait: CLAIM_WAIT_BOUND,
            observer: None,
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
            admissions: Some(crate::admission::Admissions::default()),
        };
        self
    }

    /// Tell `observer` about each Claim, accepted or refused (P1R-C03).
    pub fn with_claim_observer(mut self, observer: ClaimObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// The user a Claim's peer must run as, instead of this process's effective user. For
    /// tests: a peer of another user cannot be made without privileges.
    #[doc(hidden)]
    pub fn with_owner_uid(mut self, uid: u32) -> Self {
        self.owner_uid = uid;
        self
    }

    /// How long a Claim waits for the requests of the controller before it, instead of
    /// [`CLAIM_WAIT_BOUND`]. For tests.
    #[doc(hidden)]
    pub fn with_claim_wait(mut self, wait: Duration) -> Self {
        self.claim_wait = wait;
        self
    }

    /// The identity the Runner states, instead of this process's. For tests: two Runners in one
    /// process.
    #[doc(hidden)]
    pub fn with_identity(mut self, identity: RunnerIdentity) -> Self {
        self.identity = identity;
        self
    }

    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }

    /// Serve each connection that `accept` yields, all at once, until the Runner stops:
    /// through its controller's `Stop`, or a stop request, which ends every connection and the
    /// wait for the next one. `served` hears how each controlling connection ended, the stop,
    /// and why a connection could not be accepted; a failed accept is tried again after a
    /// pause. How many processes the stop terminated.
    pub async fn run<A, F>(
        self,
        mut accept: A,
        mut served: impl FnMut(std::io::Result<Served>),
    ) -> u32
    where
        A: FnMut() -> F,
        F: std::future::Future<Output = std::io::Result<UnixStream>>,
    {
        let shared = Arc::new(Shared::new(self));
        let stop = shared.stop.clone();
        let mut connections: JoinSet<Ended> = JoinSet::new();
        let next_connection = AtomicU64::new(1);
        loop {
            tokio::select! {
                biased;
                () = stop.requested() => break,
                Some(ended) = connections.join_next(), if !connections.is_empty() => {
                    if let Ok(Ended { served: Some(ending), .. }) = ended {
                        served(Ok(ending));
                    }
                }
                accepted = accept() => match accepted {
                    Ok(stream) => {
                        let id = next_connection.fetch_add(1, Ordering::SeqCst);
                        connections.spawn(connection(shared.clone(), stream, id));
                    }
                    Err(err) => {
                        served(Err(err));
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
            }
        }
        // Every connection stops reading, and its requests end before the processes are
        // waited for: none of them overlaps the stop.
        let mut answers = Vec::new();
        while let Some(ended) = connections.join_next().await {
            if let Ok(ended) = ended {
                if let Some(ending) = ended.served {
                    served(Ok(ending));
                }
                answers.extend(ended.stop_answer);
            }
        }
        let terminated = shared.end_processes().await;
        let processes = shared.inventory();
        for (request_id, write) in answers {
            let response = WireEnvelope::response(
                request_id,
                Ok(RunnerOpResult::Stopped {
                    terminated,
                    processes: processes.clone(),
                }),
            );
            let mut writer = write.lock().await;
            let _ = write_frame(&mut *writer, &response).await;
            let _ = writer.shutdown().await;
        }
        served(Ok(Served::Stopped { terminated }));
        terminated
    }
}

/// Which connection controls an independent Runner, and under which epoch (P1R-C03).
#[derive(Default)]
struct Control {
    /// The epoch of the last Claim; 0 before the first. It never decreases.
    epoch: u64,
    controller: Option<Controller>,
    /// The connection whose Claim waits for the controller before it.
    claiming: Option<u64>,
    /// The operations database of the first accepted Claim.
    store_id: Option<String>,
    /// The workspaces in which a controller started work (P1R-C01, P1R-C04).
    started_in: BTreeSet<String>,
}

struct Controller {
    connection: u64,
    epoch: u64,
    /// The connection reads no further request: its stream ended, or it detached; its
    /// requests may still be running.
    ended: Arc<AtomicBool>,
    /// The workspaces in which an earlier controller started work, and which this one has not
    /// reconciled yet: it starts nothing there (P1R-C04).
    barrier: BTreeSet<String>,
}

/// What an independent Runner's connections share.
struct Shared {
    runner: InProcessRunner,
    registrar: Registrar,
    events: Arc<Mutex<EventHub>>,
    /// The number of the last exit event.
    numbered: Arc<std::sync::Mutex<u64>>,
    stop: StopHandle,
    identity: RunnerIdentity,
    owner_uid: u32,
    claim_wait: Duration,
    observer: Option<ClaimObserver>,
    control: std::sync::Mutex<Control>,
    /// Bumped whenever a controller lets go.
    released: watch::Sender<u64>,
}

/// How one connection of an independent Runner ended.
struct Ended {
    /// How it ended, when it controlled the Runner and did not stop it.
    served: Option<Served>,
    /// A `Stop` it sent, to be answered once the processes ended.
    stop_answer: Option<(String, Arc<Mutex<OwnedWriteHalf>>)>,
}

/// Why an independent Runner's connection stopped reading.
enum Ending {
    Detach(String),
    Lost,
    /// With the request's id when `Stop` asked for it.
    Stop(Option<String>),
}

/// The peer of a connection, from its credentials (P1R-C03).
struct Peer {
    uid: std::io::Result<u32>,
    pid: Option<i32>,
}

impl Peer {
    fn of(stream: &UnixStream) -> Self {
        match stream.peer_cred() {
            Ok(credentials) => Self {
                uid: Ok(credentials.uid()),
                pid: credentials.pid(),
            },
            Err(err) => Self {
                uid: Err(err),
                pid: None,
            },
        }
    }
}

impl Shared {
    fn new(independent: IndependentRunner) -> Self {
        let IndependentRunner {
            runner,
            events,
            registrar,
            stop,
            identity,
            owner_uid,
            claim_wait,
            observer,
        } = independent;
        Self {
            runner,
            registrar,
            events: Arc::new(Mutex::new(EventHub {
                events: events.receiver,
                undelivered: None,
            })),
            numbered: events.numbered,
            stop,
            identity,
            owner_uid,
            claim_wait,
            observer,
            control: std::sync::Mutex::new(Control::default()),
            released: watch::Sender::new(0),
        }
    }

    fn control(&self) -> std::sync::MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answer connection `connection`'s Claim for the operations database `store_id`, and tell
    /// the observer.
    async fn claim(
        &self,
        connection: u64,
        peer: &Peer,
        ended: &Arc<AtomicBool>,
        store_id: &str,
    ) -> RunnerOpResult {
        match self.take_control(connection, peer, ended, store_id).await {
            Ok(claim) => {
                self.note(ClaimNote::Claimed {
                    epoch: claim.epoch,
                    previous_controller: claim.previous_controller,
                    peer_pid: peer.pid,
                });
                RunnerOpResult::Claimed(claim)
            }
            Err((refusal, message)) => {
                self.note(ClaimNote::Refused {
                    refusal,
                    message: message.clone(),
                    peer_pid: peer.pid,
                });
                RunnerOpResult::ClaimRefused { refusal, message }
            }
        }
    }

    fn note(&self, note: ClaimNote) {
        if let Some(observer) = &self.observer {
            observer(note);
        }
    }

    async fn take_control(
        &self,
        connection: u64,
        peer: &Peer,
        ended: &Arc<AtomicBool>,
        store_id: &str,
    ) -> Result<RunnerClaim, (ClaimRefusal, String)> {
        if self.stop.is_requested() {
            return Err((ClaimRefusal::Stopping, "the Runner is stopping".into()));
        }
        match &peer.uid {
            Ok(uid) if *uid == self.owner_uid => {}
            Ok(uid) => {
                return Err((
                    ClaimRefusal::PeerUid,
                    format!(
                        "the connection's peer runs as uid {uid}, and this Runner as uid {}",
                        self.owner_uid
                    ),
                ))
            }
            Err(err) => {
                return Err((
                    ClaimRefusal::PeerUid,
                    format!("the connection's peer credentials could not be read: {err}"),
                ))
            }
        }
        if store_id.is_empty() {
            return Err((
                ClaimRefusal::StoreMismatch,
                "a Claim names the operations database of its gateway".into(),
            ));
        }
        {
            let mut control = self.control();
            if let Some(known) = control
                .store_id
                .as_deref()
                .filter(|known| *known != store_id)
            {
                return Err((
                    ClaimRefusal::StoreMismatch,
                    format!(
                        "this Runner was claimed for the operations database {known}, not \
                         {store_id}"
                    ),
                ));
            }
            if let Some(controller) = &control.controller {
                if controller.connection == connection {
                    return Err((
                        ClaimRefusal::ControllerActive,
                        format!(
                            "this connection already controls the Runner, under epoch {}",
                            controller.epoch
                        ),
                    ));
                }
                if !controller.ended.load(Ordering::SeqCst) {
                    return Err((
                        ClaimRefusal::ControllerActive,
                        "another connection controls this Runner and is still open".into(),
                    ));
                }
            }
            if control.claiming.is_some() {
                return Err((
                    ClaimRefusal::ControllerActive,
                    "another connection is claiming this Runner".into(),
                ));
            }
            control.claiming = Some(connection);
        }
        let mut released = self.released.subscribe();
        let deadline = tokio::time::Instant::now() + self.claim_wait;
        loop {
            {
                let mut control = self.control();
                if control.controller.is_none() {
                    control.claiming = None;
                    control.epoch += 1;
                    let epoch = control.epoch;
                    let barrier = control.started_in.clone();
                    control.controller = Some(Controller {
                        connection,
                        epoch,
                        ended: ended.clone(),
                        barrier: barrier.clone(),
                    });
                    if control.store_id.is_none() {
                        control.store_id = Some(store_id.to_string());
                    }
                    drop(control);
                    return Ok(self.claimed(epoch, barrier));
                }
                let refusal = if self.stop.is_requested() {
                    Some((ClaimRefusal::Stopping, "the Runner is stopping".to_string()))
                } else if ended.load(Ordering::SeqCst) {
                    Some((
                        ClaimRefusal::RequestsRunning,
                        "the connection ended while its Claim waited".to_string(),
                    ))
                } else if tokio::time::Instant::now() >= deadline {
                    Some((
                        ClaimRefusal::RequestsRunning,
                        format!(
                            "the requests of the connection that controlled this Runner did \
                             not end within {:?}",
                            self.claim_wait
                        ),
                    ))
                } else {
                    None
                };
                if let Some(refusal) = refusal {
                    control.claiming = None;
                    return Err(refusal);
                }
            }
            tokio::select! {
                _ = released.changed() => {}
                () = tokio::time::sleep(CLAIM_POLL) => {}
                () = self.stop.requested() => {}
            }
        }
    }

    /// The answer to an accepted Claim: the events numbered so far, then every process, so an
    /// exit numbered up to `events_through` is in the inventory.
    fn claimed(&self, epoch: u64, barrier: BTreeSet<String>) -> RunnerClaim {
        let events_through = *self.numbered.lock().unwrap_or_else(PoisonError::into_inner);
        RunnerClaim {
            epoch,
            previous_controller: !barrier.is_empty(),
            barrier: barrier.into_iter().collect(),
            processes: self.inventory(),
            events_through,
        }
    }

    /// Every process the Runner holds, those that ended included.
    fn inventory(&self) -> Vec<RunnerInventoryEntry> {
        self.runner
            .inventory()
            .into_iter()
            .map(|(workspace_id, status)| RunnerInventoryEntry {
                workspace_id,
                status,
            })
            .collect()
    }

    /// Admit connection `connection`'s request `op`, sent under `epoch`: only the controller's,
    /// with the current epoch when it changes a process or the workspace (P1R-C03), and nothing
    /// that starts work in a workspace where an earlier controller did, until this one
    /// reconciled it (P1R-C04).
    fn admit(&self, connection: u64, epoch: Option<u64>, op: &RunnerOp) -> Result<(), ErrorBody> {
        let mut control = self.control();
        let Some(current) = control
            .controller
            .as_ref()
            .filter(|controller| controller.connection == connection)
            .map(|controller| controller.epoch)
        else {
            return Err(stale_controller(
                "this connection does not control the independent Runner: another connection \
                 claimed it, or this one never did; nothing was started or changed",
            ));
        };
        if fenced(op) && epoch != Some(current) {
            return Err(stale_controller(format!(
                "the request was sent under controller epoch {}, and the Runner is controlled \
                 under epoch {current}: nothing was started or changed",
                epoch.map_or_else(|| "none".to_string(), |epoch| epoch.to_string()),
            )));
        }
        if let Some(workspace_id) = starts_work(op) {
            if control
                .controller
                .as_ref()
                .is_some_and(|controller| controller.barrier.contains(workspace_id))
            {
                return Err(recovery_pending(workspace_id));
            }
            control.started_in.insert(workspace_id.to_string());
        }
        Ok(())
    }

    /// The controller `connection` reconciled these workspaces (P1R-C04): it starts work there
    /// from now on. Admitted as a fenced request first.
    fn reconciled(&self, connection: u64, workspace_ids: &[String]) {
        let mut control = self.control();
        if let Some(controller) = control
            .controller
            .as_mut()
            .filter(|controller| controller.connection == connection)
        {
            for workspace_id in workspace_ids {
                controller.barrier.remove(workspace_id);
            }
        }
    }

    /// What the Runner knows of each of `process_ids` (P1R-C04).
    fn recall(&self, process_ids: &[ProcessId]) -> Vec<RecalledProcess> {
        process_ids
            .iter()
            .map(|process_id| RecalledProcess {
                process_id: process_id.clone(),
                recalled: self.runner.recall(process_id),
            })
            .collect()
    }

    /// Connection `connection` lets go of the Runner, if it controls it. Whether it did.
    fn release(&self, connection: u64) -> bool {
        let mut control = self.control();
        if !control
            .controller
            .as_ref()
            .is_some_and(|controller| controller.connection == connection)
        {
            return false;
        }
        control.controller = None;
        drop(control);
        self.released.send_modify(|released| *released += 1);
        true
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

/// Read a connection's requests ahead of their answers, so the connection's end is seen while a
/// request runs: `ended` is set once the stream ends or cannot be read.
async fn read_ahead(
    mut read: tokio::net::unix::OwnedReadHalf,
    frames: mpsc::Sender<Vec<u8>>,
    ended: Arc<AtomicBool>,
) {
    while let Ok(Some(payload)) = read_frame(&mut read).await {
        if frames.send(payload).await.is_err() {
            return;
        }
    }
    ended.store(true, Ordering::SeqCst);
}

/// One connection of an independent Runner, from its accept to its end. Every request it sent
/// has ended when this returns, its `Registration`, `Prepare` and `Launch` requests included,
/// and it controls the Runner no longer.
async fn connection(shared: Arc<Shared>, stream: UnixStream, id: u64) -> Ended {
    let peer = Peer::of(&stream);
    let (read, write) = stream.into_split();
    let write = Arc::new(Mutex::new(write));
    let ended = Arc::new(AtomicBool::new(false));
    let (frames_in, mut frames) = mpsc::channel(READ_AHEAD);
    let reader = tokio::spawn(read_ahead(read, frames_in, ended.clone()));
    let hello = hello(
        &shared.registrar,
        RunnerLifecycle::Independent,
        &shared.identity,
    );
    let mut forwarder: Option<(oneshot::Sender<()>, tokio::task::JoinHandle<()>)> = None;
    let mut beside: JoinSet<()> = JoinSet::new();
    let mut cache: VecDeque<(String, WireEnvelope)> = VecDeque::new();
    let stop = shared.stop.clone();
    let ending = loop {
        let frame = tokio::select! {
            biased;
            () = stop.requested() => break Ending::Stop(None),
            frame = frames.recv() => frame,
        };
        let Some(payload) = frame else {
            break Ending::Lost;
        };
        let (request_id, epoch, op) = match parse_request(&payload) {
            Ok(request) => request,
            Err(response) => {
                let mut writer = write.lock().await;
                let _ = write_frame(&mut *writer, &response).await;
                break Ending::Lost;
            }
        };
        let response = match op {
            Some(RunnerOp::Detach) => break Ending::Detach(request_id),
            Some(RunnerOp::Stop) => match shared.admit(id, epoch, &RunnerOp::Stop) {
                Ok(()) => {
                    stop.request();
                    break Ending::Stop(Some(request_id));
                }
                Err(refusal) => WireEnvelope::response(request_id, Err(refusal)),
            },
            Some(RunnerOp::Claim { store_id }) => {
                let result = shared.claim(id, &peer, &ended, &store_id).await;
                let claimed = matches!(result, RunnerOpResult::Claimed(_));
                let response = WireEnvelope::response(request_id, Ok(result));
                {
                    let mut writer = write.lock().await;
                    if write_frame(&mut *writer, &response).await.is_err() {
                        break Ending::Lost;
                    }
                }
                // The controller's exit events follow its Claim's answer.
                if claimed {
                    let (stop_events, events_stopped) = oneshot::channel();
                    forwarder = Some((
                        stop_events,
                        tokio::spawn(forward_events(
                            shared.events.clone(),
                            write.clone(),
                            events_stopped,
                        )),
                    ));
                }
                continue;
            }
            op @ (Some(RunnerOp::Hello | RunnerOp::Replay { .. }) | None) => {
                answer(&mut cache, &shared.runner, request_id, op, &hello).await
            }
            Some(op) => {
                // A request to start a process that created none is remembered as refused, once
                // it is answered (P1R-C04).
                let started = started_process(&op).cloned();
                let response = match shared.admit(id, epoch, &op) {
                    Err(refusal) => WireEnvelope::response(request_id, Err(refusal)),
                    Ok(()) => match op {
                        RunnerOp::Recall { process_ids } => WireEnvelope::response(
                            request_id,
                            Ok(RunnerOpResult::Recalled(shared.recall(&process_ids))),
                        ),
                        RunnerOp::Reconciled { workspace_ids } => {
                            shared.reconciled(id, &workspace_ids);
                            WireEnvelope::response(request_id, Ok(RunnerOpResult::Reconciled))
                        }
                        RunnerOp::Acknowledge { process_ids } => {
                            shared.runner.acknowledge(&process_ids);
                            WireEnvelope::response(request_id, Ok(RunnerOpResult::Acknowledged))
                        }
                        #[cfg(feature = "devguard")]
                        op if answered_beside(&op) => {
                            let (write, registrar, runner) = (
                                write.clone(),
                                shared.registrar.clone(),
                                shared.runner.clone(),
                            );
                            beside.spawn(async move {
                                let result = answer_beside(op, registrar, runner.clone()).await;
                                if let Some(process_id) = started {
                                    runner.note_received(&process_id.0);
                                }
                                let response = WireEnvelope::response(request_id, result);
                                let mut writer = write.lock().await;
                                let _ = write_frame(&mut *writer, &response).await;
                            });
                            continue;
                        }
                        op => {
                            answer(&mut cache, &shared.runner, request_id, Some(op), &hello).await
                        }
                    },
                };
                if let Some(process_id) = started {
                    shared.runner.note_received(&process_id.0);
                }
                response
            }
        };
        let mut writer = write.lock().await;
        if write_frame(&mut *writer, &response).await.is_err() {
            break Ending::Lost;
        }
    };
    reader.abort();
    // It reads no further request: a Claim waits for its requests from now on, rather than
    // being refused as one made while it controls the Runner.
    ended.store(true, Ordering::SeqCst);
    if let Ending::Detach(request_id) = &ending {
        let response = WireEnvelope::response(request_id.clone(), Ok(RunnerOpResult::Detached));
        let mut writer = write.lock().await;
        let _ = write_frame(&mut *writer, &response).await;
    }
    // The connection's requests end before anything else: no later Claim, and no stop,
    // overlaps them.
    while beside.join_next().await.is_some() {}
    if let Some((stop_events, forwarding)) = forwarder {
        let _ = stop_events.send(());
        let _ = forwarding.await;
    }
    let controlled = shared.release(id);
    match ending {
        Ending::Stop(Some(request_id)) => Ended {
            served: None,
            stop_answer: Some((request_id, write)),
        },
        ending => {
            let _ = write.lock().await.shutdown().await;
            Ended {
                served: match ending {
                    Ending::Detach(_) if controlled => Some(Served::Detached),
                    Ending::Lost if controlled => Some(Served::Lost),
                    _ => None,
                },
                stop_answer: None,
            }
        }
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
        RunnerOp::Hello => unreachable!("hello is answered before dispatch"),
        RunnerOp::Replay { .. } => unreachable!("replay is handled before dispatch"),
        RunnerOp::Claim { .. }
        | RunnerOp::Detach
        | RunnerOp::Stop
        | RunnerOp::Recall { .. }
        | RunnerOp::Reconciled { .. }
        | RunnerOp::Acknowledge { .. } => {
            unreachable!("an independent Runner's requests are handled before dispatch")
        }
        #[cfg(feature = "devguard")]
        RunnerOp::Registration => unreachable!("registration is handled before dispatch"),
        #[cfg(feature = "devguard")]
        RunnerOp::Prepare { .. } => unreachable!("preparation is handled before dispatch"),
        #[cfg(feature = "devguard")]
        RunnerOp::Launch { .. } => unreachable!("launch is handled before dispatch"),
        #[cfg(feature = "devguard")]
        RunnerOp::Lookup { .. } => unreachable!("lookup is handled before dispatch"),
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
    fn wire_protocol_is_v11() {
        assert_eq!(WIRE_PROTOCOL, 11);
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

        /// An independent Runner keeps its registration across its controllers and states it in
        /// each `Hello`, and a Claim made once its controller detached waits for that
        /// controller's `Registration` requests, which run beside its others, so the next
        /// controller starts after them (P1R-C01, P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_independent_runner_answers_registration_before_the_next_claim() {
            use super::independent::{Host, STORE};
            use std::time::Instant;
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
            let registration = OwnerRegistration::new(owner, ResourceOwner::Worker);
            let host = Host::start_with(move |independent| {
                independent.with_registration(Some(registration), None)
            });
            let stated = |hello| match hello {
                Ok(RunnerOpResult::Hello {
                    lifecycle,
                    registration,
                    admission,
                    launch,
                    ..
                }) => (lifecycle, registration, admission, launch),
                other => panic!("unexpected {other:?}"),
            };
            let mut first = host.connect_raw().await;
            assert_eq!(
                stated(first.call(RunnerOp::Hello, None).await),
                (RunnerLifecycle::Independent, true, true, false)
            );
            assert_eq!(first.claim(STORE).await.unwrap().epoch, 1);
            let registering = first.send(RunnerOp::Registration, Some(1)).await;
            let detaching = first.send(RunnerOp::Detach, None).await;
            // Both are answered before the connection ends; the next connection claims the
            // Runner as soon as the detach is answered.
            let mut answered = Vec::new();
            let mut claiming = None;
            while let Some(reply) = first.next_response().await {
                let id = reply.request_id.unwrap();
                if id == detaching {
                    let mut next = host.connect_raw().await;
                    claiming = Some(tokio::spawn(async move {
                        let hello = next.call(RunnerOp::Hello, None).await;
                        let claim = next.claim(STORE).await;
                        (hello, claim, Instant::now())
                    }));
                }
                answered.push((id, Instant::now()));
            }
            let registered_at = answered
                .iter()
                .find(|(id, _)| *id == registering)
                .expect("the registration is answered")
                .1;
            let (hello, claim, claimed_at) =
                claiming.expect("the detach is answered").await.unwrap();
            assert_eq!(
                stated(hello),
                (RunnerLifecycle::Independent, true, true, false)
            );
            let claim = claim.unwrap();
            assert_eq!((claim.epoch, claim.previous_controller), (2, false));
            assert!(registered_at <= claimed_at);
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

    /// A managed worker refuses `Claim`, `Detach`, `Stop`, `Recall`, `Reconciled`,
    /// `Acknowledge` and with DevGuard `Lookup`, and keeps serving; its `Hello` states its
    /// lifecycle and identity (P1R-C01, P1R-C03, P1R-C04).
    #[tokio::test]
    async fn a_managed_worker_refuses_what_only_an_independent_runner_answers() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let (mut read, mut write) = client.into_split();
        let mut requests = vec![
            (
                "rrpc-claim",
                RunnerOp::Claim {
                    store_id: "store-1".into(),
                },
            ),
            ("rrpc-detach", RunnerOp::Detach),
            ("rrpc-stop", RunnerOp::Stop),
            (
                "rrpc-recall",
                RunnerOp::Recall {
                    process_ids: vec![ProcessId("proc-1".into())],
                },
            ),
            (
                "rrpc-reconciled",
                RunnerOp::Reconciled {
                    workspace_ids: vec!["demo".into()],
                },
            ),
            (
                "rrpc-acknowledge",
                RunnerOp::Acknowledge {
                    process_ids: vec![ProcessId("proc-1".into())],
                },
            ),
        ];
        #[cfg(feature = "devguard")]
        requests.push((
            "rrpc-lookup",
            RunnerOp::Lookup {
                attempts: vec![crate::admission::AttemptRef {
                    attempt_id: "cs-1".into(),
                    process_id: ProcessId("proc-1".into()),
                }],
            },
        ));
        requests.push(("rrpc-hello", RunnerOp::Hello));
        for (id, op) in requests {
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
                    ..
                }) => {
                    assert_eq!(lifecycle, RunnerLifecycle::Managed);
                    assert_eq!(identity, Some(RunnerIdentity::this_process()));
                }
                None => assert_eq!(reply.error.unwrap().code, ErrorCode::InvalidCommand),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// A request's epoch, an event's number and a Claim cross the wire; a request sent under no
    /// epoch states none (P1R-C03).
    #[test]
    fn an_epoch_an_event_number_and_a_claim_cross_the_wire() {
        let request = WireEnvelope::request(
            "rrpc-1".into(),
            RunnerOp::Terminate {
                process_id: ProcessId("proc-1".into()),
            },
        )
        .with_epoch(Some(7));
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["epoch"], 7);
        let parsed: WireEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.epoch, Some(7));
        let bare =
            serde_json::to_value(WireEnvelope::request("rrpc-2".into(), RunnerOp::Hello)).unwrap();
        assert!(bare.get("epoch").is_none(), "{bare}");
        let event = WireEnvelope::event(
            3,
            RunnerEvent::ProcessExited {
                process_id: ProcessId("proc-1".into()),
            },
        );
        let parsed: WireEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&event).unwrap()).unwrap();
        assert_eq!(parsed.seq, Some(3));
        assert_eq!(
            serde_json::to_value(RunnerOp::Claim {
                store_id: "store-1".into()
            })
            .unwrap(),
            serde_json::json!({ "op": "claim", "data": { "store_id": "store-1" } })
        );
        let claimed = RunnerOpResult::Claimed(RunnerClaim {
            epoch: 2,
            previous_controller: true,
            barrier: vec!["demo".into()],
            processes: vec![RunnerInventoryEntry {
                workspace_id: "demo".into(),
                status: RunnerProcessStatus {
                    process_id: ProcessId("proc-1".into()),
                    state: codespace_domain::ProcessState::Running,
                    exit_code: None,
                    termination: None,
                    signal: None,
                    output_total: 4,
                    output_retained_from: 0,
                    eof: false,
                },
            }],
            events_through: 5,
        });
        let json = serde_json::to_value(&claimed).unwrap();
        assert_eq!(json["type"], "claimed");
        assert_eq!(json["data"]["epoch"], 2);
        assert_eq!(json["data"]["events_through"], 5);
        assert_eq!(json["data"]["barrier"], serde_json::json!(["demo"]));
        assert_eq!(json["data"]["processes"][0]["workspace_id"], "demo");
        match serde_json::from_value(json.clone()).unwrap() {
            RunnerOpResult::Claimed(claim) => assert_eq!(claim.processes.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
        // A Claim's answer without a barrier states none (P1R-C04).
        let mut without = json;
        without["data"].as_object_mut().unwrap().remove("barrier");
        match serde_json::from_value(without).unwrap() {
            RunnerOpResult::Claimed(claim) => assert!(claim.barrier.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
        let refused = RunnerOpResult::ClaimRefused {
            refusal: ClaimRefusal::ControllerActive,
            message: "another connection controls this Runner".into(),
        };
        assert_eq!(
            serde_json::to_value(&refused).unwrap()["data"]["refusal"],
            "controller_active"
        );
        for refusal in [
            ClaimRefusal::PeerUid,
            ClaimRefusal::StoreMismatch,
            ClaimRefusal::ControllerActive,
            ClaimRefusal::RequestsRunning,
            ClaimRefusal::Stopping,
        ] {
            assert_eq!(
                serde_json::to_value(refusal).unwrap(),
                serde_json::json!(refusal.as_str())
            );
        }
    }

    /// An independent Runner across its connections, through the gateway's client and through
    /// connections that send frames as no gateway's would (P1R-C01, P1R-C03).
    pub(super) mod independent {
        use super::*;
        use crate::{
            LinkEvent, LinkObserver, RunnerApplyPatchRequest, RunnerReadProcess, RunnerWriteStdin,
            UdsRunner, CLAIM_START_BOUND,
        };
        use codespace_domain::{ProcessState, ProcessTermination, Profile, WorkspaceId};
        use std::path::{Path, PathBuf};
        use std::time::Instant;
        use tokio::task::AbortHandle;

        /// The operations database the gateways here claim the Runner for.
        pub(in crate::wire::tests) const STORE: &str = "store-test";

        /// A short private directory: a socket's path must fit `sun_path` on macOS.
        fn short_directory() -> tempfile::TempDir {
            tempfile::Builder::new()
                .prefix("cs-")
                .tempdir_in("/tmp")
                .unwrap()
        }

        /// An independent Runner listening on a socket, as the worker binary does. Dropping it
        /// stops the Runner.
        pub(in crate::wire::tests) struct Host {
            pub(in crate::wire::tests) socket: PathBuf,
            _dir: tempfile::TempDir,
            served: mpsc::UnboundedReceiver<Served>,
            pub(in crate::wire::tests) stop: StopHandle,
            pub(in crate::wire::tests) runner: InProcessRunner,
            claims: Arc<std::sync::Mutex<Vec<ClaimNote>>>,
        }

        impl Host {
            pub(in crate::wire::tests) fn start() -> Self {
                Self::start_with(|independent| independent)
            }

            pub(in crate::wire::tests) fn start_with(
                configure: impl FnOnce(IndependentRunner) -> IndependentRunner,
            ) -> Self {
                let dir = short_directory();
                let socket = dir.path().join("runner.sock");
                let listener = Arc::new(tokio::net::UnixListener::bind(&socket).unwrap());
                let (runner, events) = host_worker();
                let claims = Arc::new(std::sync::Mutex::new(Vec::new()));
                let noted = claims.clone();
                let independent = configure(
                    IndependentRunner::new(runner.clone(), events).with_claim_observer(Arc::new(
                        move |note| noted.lock().unwrap().push(note),
                    )),
                );
                let stop = independent.stop_handle();
                let (report, served) = mpsc::unbounded_channel();
                tokio::spawn(independent.run(
                    move || {
                        let listener = listener.clone();
                        async move { listener.accept().await.map(|(stream, _)| stream) }
                    },
                    move |ending| {
                        if let Ok(ending) = ending {
                            let _ = report.send(ending);
                        }
                    },
                ));
                Self {
                    socket,
                    _dir: dir,
                    served,
                    stop,
                    runner,
                    claims,
                }
            }

            /// A gateway's client after its handshake, which has not claimed the Runner.
            async fn connect_unclaimed(&self, on_exit: ShellRelease) -> UdsRunner {
                UdsRunner::connect(&self.socket, on_exit).await.unwrap()
            }

            /// A gateway's client that claimed the Runner.
            async fn connect(&self) -> UdsRunner {
                self.connect_reporting_exits(Arc::new(|_: &str| {})).await
            }

            async fn connect_reporting_exits(&self, on_exit: ShellRelease) -> UdsRunner {
                let gateway = self.connect_unclaimed(on_exit).await;
                gateway.claim(STORE).await.unwrap();
                gateway
            }

            /// A connection the gateway's client does not speak for.
            pub(in crate::wire::tests) async fn connect_raw(&self) -> Raw {
                Raw::new(tokio::net::UnixStream::connect(&self.socket).await.unwrap())
            }

            /// How the next controlling connection ended, or the stop.
            async fn served(&mut self) -> Served {
                tokio::time::timeout(Duration::from_secs(20), self.served.recv())
                    .await
                    .expect("the connection ends")
                    .expect("the Runner reports it")
            }

            /// What the Runner noted of each Claim so far.
            fn claims(&self) -> Vec<ClaimNote> {
                self.claims.lock().unwrap().clone()
            }
        }

        impl Drop for Host {
            fn drop(&mut self) {
                self.stop.request();
            }
        }

        /// A connection that sends requests frame by frame.
        pub(in crate::wire::tests) struct Raw {
            read: tokio::net::unix::OwnedReadHalf,
            write: tokio::net::unix::OwnedWriteHalf,
            next: u64,
        }

        impl Raw {
            fn new(stream: tokio::net::UnixStream) -> Self {
                let (read, write) = stream.into_split();
                Self {
                    read,
                    write,
                    next: 1,
                }
            }

            /// Send `op` under `epoch`; the request's id.
            pub(in crate::wire::tests) async fn send(
                &mut self,
                op: RunnerOp,
                epoch: Option<u64>,
            ) -> String {
                let id = format!("rrpc-raw-{}", self.next);
                self.next += 1;
                write_frame(
                    &mut self.write,
                    &WireEnvelope::request(id.clone(), op).with_epoch(epoch),
                )
                .await
                .unwrap();
                id
            }

            /// The next response, past any event, or none once the connection ended.
            pub(in crate::wire::tests) async fn next_response(&mut self) -> Option<WireEnvelope> {
                loop {
                    let frame =
                        tokio::time::timeout(Duration::from_secs(20), read_frame(&mut self.read))
                            .await
                            .expect("a response or the end")
                            .ok()??;
                    let envelope: WireEnvelope = serde_json::from_slice(&frame).unwrap();
                    if envelope.kind == WireKind::Response {
                        return Some(envelope);
                    }
                }
            }

            /// Send `op` under `epoch` and return its answer.
            pub(in crate::wire::tests) async fn call(
                &mut self,
                op: RunnerOp,
                epoch: Option<u64>,
            ) -> Result<RunnerOpResult, ErrorBody> {
                let id = self.send(op, epoch).await;
                let reply = self.next_response().await.expect("an answer");
                assert_eq!(reply.request_id.as_deref(), Some(id.as_str()));
                match (reply.result, reply.error) {
                    (Some(result), None) => Ok(result),
                    (None, Some(error)) => Err(error),
                    other => panic!("unexpected {other:?}"),
                }
            }

            /// Claim the Runner for the operations database `store_id`.
            pub(in crate::wire::tests) async fn claim(
                &mut self,
                store_id: &str,
            ) -> Result<RunnerClaim, (ClaimRefusal, String)> {
                match self
                    .call(
                        RunnerOp::Claim {
                            store_id: store_id.into(),
                        },
                        None,
                    )
                    .await
                {
                    Ok(RunnerOpResult::Claimed(claim)) => Ok(claim),
                    Ok(RunnerOpResult::ClaimRefused { refusal, message }) => {
                        Err((refusal, message))
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }

        /// A relay between clients and a Runner's socket, which cuts its connections, holds
        /// new ones, or loses one answer of the next one, as a connection between them can
        /// fail (P1R-C03).
        struct Relay {
            socket: PathBuf,
            _dir: tempfile::TempDir,
            target: Arc<std::sync::Mutex<PathBuf>>,
            links: Arc<std::sync::Mutex<Vec<AbortHandle>>>,
            held: Arc<watch::Sender<bool>>,
            /// The next connection is cut at this answer of the Runner, which is lost.
            lose: Arc<std::sync::Mutex<Option<usize>>>,
            accepting: AbortHandle,
        }

        impl Relay {
            fn start(target: &Path) -> Self {
                let dir = short_directory();
                let socket = dir.path().join("relay.sock");
                let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                let target = Arc::new(std::sync::Mutex::new(target.to_path_buf()));
                let links: Arc<std::sync::Mutex<Vec<AbortHandle>>> = Arc::default();
                let held = Arc::new(watch::Sender::new(false));
                let lose: Arc<std::sync::Mutex<Option<usize>>> = Arc::default();
                let accepting = tokio::spawn({
                    let (target, links, held, lose) =
                        (target.clone(), links.clone(), held.clone(), lose.clone());
                    async move {
                        while let Ok((client, _)) = listener.accept().await {
                            let mut released = held.subscribe();
                            let target = target.lock().unwrap().clone();
                            let lose = lose.lock().unwrap().take();
                            let link = tokio::spawn(async move {
                                let _ = released.wait_for(|held| !*held).await;
                                if let Ok(runner) = tokio::net::UnixStream::connect(&target).await {
                                    relay_link(client, runner, lose).await;
                                }
                            });
                            links.lock().unwrap().push(link.abort_handle());
                        }
                    }
                })
                .abort_handle();
                Self {
                    socket,
                    _dir: dir,
                    target,
                    links,
                    held,
                    lose,
                    accepting,
                }
            }

            /// Cut every connection: both of its ends see it end.
            fn cut(&self) {
                for link in self.links.lock().unwrap().drain(..) {
                    link.abort();
                }
            }

            /// Hold each new connection, unanswered, until let go.
            fn hold(&self, held: bool) {
                self.held.send_replace(held);
            }

            /// Lose the next connection at the Runner's `answer`th answer, counted from 1.
            fn lose_answer(&self, answer: usize) {
                *self.lose.lock().unwrap() = Some(answer);
            }

            /// Relay new connections to the socket `target`.
            fn retarget(&self, target: &Path) {
                *self.target.lock().unwrap() = target.to_path_buf();
            }
        }

        impl Drop for Relay {
            fn drop(&mut self) {
                self.accepting.abort();
                self.cut();
            }
        }

        /// One relayed connection, until the Runner ends it or the relay loses an answer.
        async fn relay_link(
            client: tokio::net::UnixStream,
            runner: tokio::net::UnixStream,
            lose: Option<usize>,
        ) {
            let (mut client_read, mut client_write) = client.into_split();
            let (mut runner_read, mut runner_write) = runner.into_split();
            let upstream = async move {
                let _ = tokio::io::copy(&mut client_read, &mut runner_write).await;
                let _ = runner_write.shutdown().await;
                std::future::pending::<()>().await
            };
            let downstream = async move {
                let mut answers = 0;
                while let Ok(Some(frame)) = read_frame(&mut runner_read).await {
                    answers += 1;
                    if Some(answers) == lose {
                        return;
                    }
                    let length = u32::try_from(frame.len()).unwrap().to_be_bytes();
                    if client_write.write_all(&length).await.is_err()
                        || client_write.write_all(&frame).await.is_err()
                    {
                        return;
                    }
                }
                let _ = client_write.shutdown().await;
            };
            tokio::select! {
                () = upstream => {}
                () = downstream => {}
            }
        }

        fn workspace(root: &Path) -> Workspace {
            workspace_named(root, "demo")
        }

        fn workspace_named(root: &Path, id: &str) -> Workspace {
            Workspace::new(
                WorkspaceId(id.into()),
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

        fn exits() -> (ShellRelease, mpsc::UnboundedReceiver<String>) {
            let (exits, exited_ids) = mpsc::unbounded_channel();
            (
                Arc::new(move |process_id: &str| {
                    let _ = exits.send(process_id.to_string());
                }),
                exited_ids,
            )
        }

        async fn next_exit(exited_ids: &mut mpsc::UnboundedReceiver<String>) -> Option<String> {
            tokio::time::timeout(Duration::from_secs(10), exited_ids.recv())
                .await
                .expect("an exit event")
        }

        /// No further exit reaches the client.
        async fn no_more_exits(exited_ids: &mut mpsc::UnboundedReceiver<String>) {
            if let Ok(exit) =
                tokio::time::timeout(Duration::from_millis(300), exited_ids.recv()).await
            {
                assert_eq!(exit, None, "an exit delivered again");
            }
        }

        fn link_events() -> (LinkObserver, mpsc::UnboundedReceiver<LinkEvent>) {
            let (noted, events) = mpsc::unbounded_channel();
            (
                Arc::new(move |event| {
                    let _ = noted.send(event);
                }),
                events,
            )
        }

        /// The next event of a client's connection, past any failed try.
        async fn next_link_event(events: &mut mpsc::UnboundedReceiver<LinkEvent>) -> LinkEvent {
            loop {
                match tokio::time::timeout(Duration::from_secs(20), events.recv())
                    .await
                    .expect("a link event")
                    .expect("the client reports it")
                {
                    LinkEvent::Retrying { .. } => {}
                    event => return event,
                }
            }
        }

        async fn disconnected(gateway: &UdsRunner, process_id: &ProcessId) -> ErrorBody {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match gateway.process_status(process_id).await {
                    Err(RunnerError::Execution(body))
                        if body.code == ErrorCode::RunnerDisconnected =>
                    {
                        return body
                    }
                    other => assert!(Instant::now() < deadline, "{other:?}"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// After a detach the next gateway claims the Runner under the next epoch, is told
        /// every process it inherits and each workspace where work was started, reads the
        /// output and status of the same process_id, writes to it and resizes it, and the
        /// process times out at its original deadline. That gateway starts nothing new in that
        /// workspace, also once the inherited processes ended, until it states that it
        /// reconciled it (P1R-C04); in a workspace where no work was started it starts work at
        /// once. The handover's latency is measured.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn the_next_gateway_inherits_the_processes_and_starts_work_once_it_reconciled() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let first = host.connect_unclaimed(Arc::new(|_: &str| {})).await;
            let hello = first.hello();
            assert_eq!(hello.lifecycle, RunnerLifecycle::Independent);
            assert_eq!(hello.identity, Some(RunnerIdentity::this_process()));
            // Nothing is sent under a Claim before there is one.
            assert_eq!(first.dispatch_epoch().unwrap(), None);
            let claim = first.claim(STORE).await.unwrap();
            assert_eq!(
                claim,
                RunnerClaim {
                    epoch: 1,
                    previous_controller: false,
                    barrier: Vec::new(),
                    processes: Vec::new(),
                    events_through: 0,
                }
            );
            assert_eq!(first.claimed(), Some(claim));
            assert_eq!(first.dispatch_epoch().unwrap(), Some(1));
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

            let (on_exit, mut exited_ids) = exits();
            let second = host.connect_unclaimed(on_exit).await;
            assert_eq!(second.hello().identity, hello.identity);
            let asked = Instant::now();
            let inherited = second.claim(STORE).await.unwrap();
            eprintln!(
                "p1r-c03 claim latency: a handover after a detach was granted in {:?}",
                asked.elapsed()
            );
            assert_eq!(
                (
                    inherited.epoch,
                    inherited.previous_controller,
                    inherited.events_through
                ),
                (2, true, 0)
            );
            assert_eq!(inherited.barrier, ["demo"]);
            let held: Vec<_> = inherited
                .processes
                .iter()
                .map(|entry| {
                    (
                        entry.workspace_id.as_str(),
                        entry.status.process_id.0.as_str(),
                        entry.status.state,
                    )
                })
                .collect();
            assert_eq!(
                held,
                [
                    ("demo", "proc-pipe", ProcessState::Running),
                    ("demo", "proc-tty", ProcessState::Running)
                ]
            );
            // The client sends new work; the Runner refuses it in the workspace (P1R-C04).
            assert_eq!(second.dispatch_epoch().unwrap(), Some(2));
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
            // Nothing new starts in the workspace, and it is not changed.
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
            // Work starts at once in a workspace where none was started.
            let elsewhere = tempfile::tempdir().unwrap();
            second
                .exec(
                    &workspace_named(elsewhere.path(), "elsewhere"),
                    command(&["/bin/sh", "-c", "exit 0"], "proc-elsewhere", 5_000, false),
                )
                .await
                .unwrap();
            // An inherited process still ends on request.
            second.terminate(&tty).await.unwrap();
            assert_eq!(
                exited(&second, &tty).await.termination,
                Some(ProcessTermination::Terminated)
            );
            // The three exits reached this controller.
            let mut ended = vec![
                next_exit(&mut exited_ids).await.unwrap(),
                next_exit(&mut exited_ids).await.unwrap(),
                next_exit(&mut exited_ids).await.unwrap(),
            ];
            ended.sort();
            assert_eq!(ended, ["proc-elsewhere", "proc-pipe", "proc-tty"]);
            // The barrier stays once the inherited processes ended, for every later controller,
            // and covers the workspace this one worked in.
            second.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);
            let third = host.connect().await;
            let claim = third.claimed().unwrap();
            assert_eq!(
                (claim.epoch, claim.previous_controller, claim.events_through),
                (3, true, 3)
            );
            assert_eq!(claim.barrier, ["demo", "elsewhere"]);
            assert!(
                claim
                    .processes
                    .iter()
                    .all(|entry| entry.status.state == ProcessState::Exited),
                "{claim:?}"
            );
            assert_eq!(
                code(third.exec(&ws, touch.clone()).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            assert_eq!(
                code(third.apply_patch(&ws, patch.clone()).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            assert!(!marker.exists());
            assert!(!root.path().join("added.txt").exists());
            // Stating that it reconciled the workspace lifts the barrier there, for this
            // controller, and only under its own epoch.
            assert_eq!(
                code(
                    third
                        .at_epoch(2)
                        .reconciled(&["demo".into()])
                        .await
                        .unwrap_err()
                ),
                ErrorCode::StaleController
            );
            third.reconciled(&["demo".into()]).await.unwrap();
            match third.apply_patch(&ws, patch).await {
                Ok(_) => assert!(root.path().join("added.txt").exists()),
                // Where the patch helper is not built, it refuses the patch, not the barrier.
                Err(err) => assert_ne!(code(err), ErrorCode::RecoveryPending),
            }
            third.exec(&ws, touch).await.unwrap();
            exited(&third, &ProcessId("proc-new".into())).await;
            assert!(marker.exists());
            let stopped = third.stop_runner().await.unwrap();
            assert_eq!(stopped.terminated, 0);
            assert_eq!(stopped.processes.len(), 4, "{stopped:?}");
            assert_eq!(host.served().await, Served::Stopped { terminated: 0 });
            let claimed: Vec<_> = host
                .claims()
                .into_iter()
                .map(|note| match note {
                    ClaimNote::Claimed {
                        epoch,
                        previous_controller,
                        peer_pid,
                    } => {
                        assert!(
                            peer_pid.is_none_or(|pid| pid == std::process::id() as i32),
                            "{peer_pid:?}"
                        );
                        (epoch, previous_controller)
                    }
                    other => panic!("unexpected {other:?}"),
                })
                .collect();
            assert_eq!(claimed, [(1, false), (2, true), (3, true)]);
        }

        /// Only a controller that starts work in a workspace bars the next one from starting
        /// work there: one that only read leaves the next gateway free to.
        #[tokio::test]
        async fn a_controller_that_starts_nothing_leaves_no_barrier() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let reader = host.connect().await;
            reader.find(&ws, None, None, None).await.unwrap();
            reader.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);
            let worker = host.connect().await;
            let claim = worker.claimed().unwrap();
            assert!(!claim.previous_controller);
            assert!(claim.barrier.is_empty());
            worker
                .exec(
                    &ws,
                    command(&["/bin/sh", "-c", "exit 0"], "proc-ok", 5_000, false),
                )
                .await
                .unwrap();
            worker.close().await;
            assert_eq!(host.served().await, Served::Lost);
            let claim = host.connect().await.claimed().unwrap();
            assert!(claim.previous_controller);
            assert_eq!(claim.barrier, ["demo"]);
        }

        /// A Runner tells its controller, for each process asked about, whether a request to
        /// start it arrived and whether it created it, with its status while it holds it; a
        /// request that its barrier refused is one it received and created nothing for
        /// (P1R-C04). Only its controller asks, and only under its own epoch does it
        /// acknowledge an end.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_independent_runner_recalls_whether_it_created_each_process() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let first = host.connect().await;
            let running = ProcessId("proc-running".into());
            let failed = ProcessId("proc-failed".into());
            let never = ProcessId("proc-never".into());
            first
                .exec(
                    &ws,
                    command(&["/bin/sleep", "30"], &running.0, 60_000, false),
                )
                .await
                .unwrap();
            // Refused before any process is created, with or without the Linux sandbox.
            let no_program = command(&[""], &failed.0, 5_000, false);
            assert_eq!(
                code(first.exec(&ws, no_program).await.unwrap_err()),
                ErrorCode::InvalidCommand
            );
            first.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);

            let second = host.connect().await;
            let barred = ProcessId("proc-barred".into());
            let quick = command(&["/bin/sh", "-c", "exit 0"], &barred.0, 5_000, false);
            assert_eq!(
                code(second.exec(&ws, quick).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            let asked = [running.clone(), failed, never, barred];
            let recalled = second.recall(&asked).await.unwrap();
            assert_eq!(
                recalled
                    .iter()
                    .map(|recalled| recalled.process_id.clone())
                    .collect::<Vec<_>>(),
                asked
            );
            match &recalled[0].recalled {
                Recalled::Held {
                    workspace_id,
                    status,
                } => assert_eq!(
                    (workspace_id.as_str(), status.state),
                    ("demo", ProcessState::Running)
                ),
                other => panic!("unexpected {other:?}"),
            }
            assert_eq!(
                recalled[1..]
                    .iter()
                    .map(|recalled| recalled.recalled.clone())
                    .collect::<Vec<_>>(),
                [Recalled::Refused, Recalled::NotReceived, Recalled::Refused]
            );
            // A connection that does not control the Runner is not told.
            let mut raw = host.connect_raw().await;
            let refused = raw
                .call(
                    RunnerOp::Recall {
                        process_ids: vec![running.clone()],
                    },
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(refused.code, ErrorCode::StaleController);
            second.terminate(&running).await.unwrap();
            exited(&second, &running).await;
            match &second.recall(std::slice::from_ref(&running)).await.unwrap()[0].recalled {
                Recalled::Held { status, .. } => assert_eq!(status.state, ProcessState::Exited),
                other => panic!("unexpected {other:?}"),
            }
            assert_eq!(
                code(
                    second
                        .at_epoch(1)
                        .acknowledge(std::slice::from_ref(&running))
                        .await
                        .unwrap_err()
                ),
                ErrorCode::StaleController
            );
            second
                .acknowledge(std::slice::from_ref(&running))
                .await
                .unwrap();
        }

        /// The controller has the Runner look up a governed execution's attempt with the
        /// Runner's own DevGuard session: an attempt the Runner never admitted is not asked
        /// about, and a connection that does not control the Runner is not told (P1R-C04).
        #[cfg(feature = "devguard")]
        #[tokio::test]
        async fn an_independent_runner_looks_up_attempts_for_its_controller_only() {
            use crate::admission::{AttemptLookup, AttemptRef, LookedUp};
            let host = Host::start();
            let mut controller = host.connect_raw().await;
            let epoch = controller.claim(STORE).await.unwrap().epoch;
            let attempts = vec![AttemptRef {
                attempt_id: "cs-1".into(),
                process_id: ProcessId("proc-1".into()),
            }];
            let lookup = RunnerOp::Lookup {
                attempts: attempts.clone(),
            };
            match controller.call(lookup, Some(epoch)).await {
                Ok(RunnerOpResult::LookedUp(found)) => assert_eq!(
                    found,
                    [AttemptLookup {
                        attempt_id: "cs-1".into(),
                        process_id: ProcessId("proc-1".into()),
                        found: LookedUp::NotAdmitted,
                    }]
                ),
                other => panic!("unexpected {other:?}"),
            }
            let mut other = host.connect_raw().await;
            let refused = other
                .call(RunnerOp::Lookup { attempts }, None)
                .await
                .unwrap_err();
            assert_eq!(refused.code, ErrorCode::StaleController);
        }

        /// A Claim's answer lists every process the Runner holds, ended ones included, and the
        /// number of the last exit it reflects; an exit an earlier controller received is not
        /// sent again, and later ones follow (P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_claim_states_the_inventory_and_the_exits_it_reflects() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let (on_exit, mut exited_ids) = exits();
            let first = host.connect_reporting_exits(on_exit).await;
            first
                .exec(
                    &ws,
                    command(&["/bin/sh", "-c", "exit 3"], "proc-a", 60_000, false),
                )
                .await
                .unwrap();
            assert_eq!(next_exit(&mut exited_ids).await.as_deref(), Some("proc-a"));
            first
                .exec(&ws, command(&["/bin/sleep", "30"], "proc-b", 60_000, false))
                .await
                .unwrap();
            first.detach().await.unwrap();
            assert_eq!(host.served().await, Served::Detached);

            let (on_exit, mut exited_ids) = exits();
            let second = host.connect_reporting_exits(on_exit).await;
            let claim = second.claimed().unwrap();
            assert_eq!((claim.epoch, claim.events_through), (2, 1));
            let listed: Vec<_> = claim
                .processes
                .iter()
                .map(|entry| {
                    (
                        entry.workspace_id.as_str(),
                        entry.status.process_id.0.as_str(),
                        entry.status.state,
                        entry.status.exit_code,
                    )
                })
                .collect();
            assert_eq!(
                listed,
                [
                    ("demo", "proc-a", ProcessState::Exited, Some(3)),
                    ("demo", "proc-b", ProcessState::Running, None)
                ]
            );
            second.terminate(&ProcessId("proc-b".into())).await.unwrap();
            assert_eq!(next_exit(&mut exited_ids).await.as_deref(), Some("proc-b"));
            no_more_exits(&mut exited_ids).await;
        }

        /// A Claim is refused while the connection that controls the Runner is open, however
        /// long it says nothing, and on that connection itself; once it ends, the next Claim
        /// takes over (D6, P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_claim_is_refused_while_the_controller_is_open() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let controller = host.connect().await;
            let mut other = host.connect_raw().await;
            let (refusal, message) = other.claim(STORE).await.unwrap_err();
            assert_eq!(refusal, ClaimRefusal::ControllerActive);
            assert!(message.contains("still open"), "{message}");
            match controller.claim(STORE).await.unwrap_err() {
                crate::ClaimError::Refused { refusal, message } => {
                    assert_eq!(refusal, ClaimRefusal::ControllerActive);
                    assert!(message.contains("already controls"), "{message}");
                }
                other => panic!("unexpected {other}"),
            }
            // The controller goes on under its Claim.
            assert_eq!(controller.dispatch_epoch().unwrap(), Some(1));
            controller.find(&ws, None, None, None).await.unwrap();
            controller.close().await;
            assert_eq!(host.served().await, Served::Lost);
            let claim = other.claim(STORE).await.unwrap();
            assert_eq!((claim.epoch, claim.previous_controller), (2, false));
            assert!(matches!(
                host.claims().as_slice(),
                [
                    ClaimNote::Claimed { epoch: 1, .. },
                    ClaimNote::Refused {
                        refusal: ClaimRefusal::ControllerActive,
                        ..
                    },
                    ClaimNote::Refused {
                        refusal: ClaimRefusal::ControllerActive,
                        ..
                    },
                    ClaimNote::Claimed { epoch: 2, .. },
                ]
            ));
        }

        /// Of connections that claim the Runner at once, one controls it: the others are
        /// refused, and their changes too, until it ends (P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn of_claims_made_at_once_one_wins() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let mut claiming = JoinSet::new();
            for _ in 0..8 {
                let mut raw = host.connect_raw().await;
                claiming.spawn(async move {
                    let claim = raw.claim(STORE).await;
                    (raw, claim)
                });
            }
            let (mut winners, mut losers) = (Vec::new(), Vec::new());
            while let Some(joined) = claiming.join_next().await {
                match joined.unwrap() {
                    (raw, Ok(claim)) => winners.push((raw, claim)),
                    (raw, Err((refusal, message))) => {
                        assert_eq!(refusal, ClaimRefusal::ControllerActive, "{message}");
                        losers.push(raw);
                    }
                }
            }
            assert_eq!(winners.len(), 1);
            let (mut winner, claim) = winners.pop().unwrap();
            assert_eq!(claim.epoch, 1);
            let sleep = |id: &str| RunnerOp::Exec {
                workspace: ws.clone(),
                request: command(&["/bin/sleep", "30"], id, 60_000, false),
            };
            assert!(matches!(
                winner.call(sleep("proc-winner"), Some(1)).await,
                Ok(RunnerOpResult::Exec(_))
            ));
            for (index, loser) in losers.iter_mut().enumerate() {
                let refusal = loser
                    .call(sleep(&format!("proc-loser-{index}")), Some(1))
                    .await
                    .unwrap_err();
                assert_eq!(refusal.code, ErrorCode::StaleController, "{refusal:?}");
            }
            assert_eq!(host.runner.inventory().len(), 1);
            assert!(matches!(
                winner.call(RunnerOp::Detach, None).await,
                Ok(RunnerOpResult::Detached)
            ));
            assert_eq!(host.served().await, Served::Detached);
            let claim = losers[0].claim(STORE).await.unwrap();
            assert_eq!((claim.epoch, claim.previous_controller), (2, true));
        }

        /// A Claim from a peer of another user is refused, as the Runner reads the user from the
        /// connection's credentials, and the connection controls nothing (P1R-C03).
        #[tokio::test]
        async fn a_claim_from_another_user_is_refused() {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let uid = unsafe { libc::geteuid() };
            let host = Host::start_with(move |independent| independent.with_owner_uid(uid ^ 1));
            let mut raw = host.connect_raw().await;
            let (refusal, message) = raw.claim(STORE).await.unwrap_err();
            assert_eq!(refusal, ClaimRefusal::PeerUid);
            assert!(
                message.contains(&format!("uid {uid}"))
                    && message.contains(&format!("uid {}", uid ^ 1)),
                "{message}"
            );
            let refusal = raw
                .call(
                    RunnerOp::ProcessStatus {
                        process_id: ProcessId("proc-1".into()),
                    },
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(refusal.code, ErrorCode::StaleController);
            match host.claims().as_slice() {
                [ClaimNote::Refused {
                    refusal: ClaimRefusal::PeerUid,
                    peer_pid,
                    ..
                }] => assert!(
                    peer_pid.is_none_or(|pid| pid == std::process::id() as i32),
                    "{peer_pid:?}"
                ),
                other => panic!("unexpected {other:?}"),
            }
        }

        /// The Runner keeps the operations database it was first claimed for and refuses a
        /// Claim for another, or for none; that database's gateway claims it again (P1R-C03).
        #[tokio::test]
        async fn a_claim_for_another_operations_database_is_refused() {
            let mut host = Host::start();
            let mut first = host.connect_raw().await;
            assert_eq!(
                first.claim("").await.unwrap_err().0,
                ClaimRefusal::StoreMismatch
            );
            assert_eq!(first.claim(STORE).await.unwrap().epoch, 1);
            assert!(matches!(
                first.call(RunnerOp::Detach, None).await,
                Ok(RunnerOpResult::Detached)
            ));
            assert_eq!(host.served().await, Served::Detached);
            let mut other = host.connect_raw().await;
            let (refusal, message) = other.claim("store-other").await.unwrap_err();
            assert_eq!(refusal, ClaimRefusal::StoreMismatch);
            assert!(
                message.contains(STORE) && message.contains("store-other"),
                "{message}"
            );
            let mut again = host.connect_raw().await;
            assert_eq!(again.claim(STORE).await.unwrap().epoch, 2);
        }

        /// Only the controller's requests are answered beyond `Hello`, `Claim` and `Detach`,
        /// and each that changes a process or the workspace must carry the epoch of its Claim:
        /// one under no epoch or another, and one prepared under an earlier Claim that arrives
        /// after the next one, are refused with STALE_CONTROLLER, and nothing starts or changes
        /// (P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn requests_outside_the_current_claim_are_refused() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let marker = root.path().join("started");
            let touch = |id: &str| RunnerOp::Exec {
                workspace: ws.clone(),
                request: command(
                    &["/usr/bin/touch", &marker.display().to_string()],
                    id,
                    5_000,
                    false,
                ),
            };
            let patch = RunnerOp::ApplyPatch {
                workspace: ws.clone(),
                request: RunnerApplyPatchRequest {
                    patch: "*** Begin Patch\n*** Add File: added.txt\n+added\n*** End Patch\n"
                        .into(),
                    expected_versions: Default::default(),
                    check_only: false,
                },
            };
            let mut unclaimed = host.connect_raw().await;
            assert!(matches!(
                unclaimed.call(RunnerOp::Hello, None).await,
                Ok(RunnerOpResult::Hello { .. })
            ));
            let mut controller = host.connect_raw().await;
            assert_eq!(controller.claim(STORE).await.unwrap().epoch, 1);
            // A connection that never claimed the Runner, whatever epoch it names.
            for op in [
                touch("proc-a"),
                patch.clone(),
                RunnerOp::ProcessStatus {
                    process_id: ProcessId("proc-a".into()),
                },
                RunnerOp::Find {
                    workspace: ws.clone(),
                    glob: None,
                    offset: None,
                    limit: None,
                },
                RunnerOp::TerminateWorkspace {
                    workspace_id: "demo".into(),
                },
                RunnerOp::Stop,
            ] {
                let refusal = unclaimed.call(op, Some(1)).await.unwrap_err();
                assert_eq!(refusal.code, ErrorCode::StaleController, "{refusal:?}");
            }
            // The controller's change under no epoch, or another.
            for epoch in [None, Some(0), Some(2)] {
                for op in [touch("proc-b"), patch.clone(), RunnerOp::Stop] {
                    let refusal = controller.call(op, epoch).await.unwrap_err();
                    assert_eq!(refusal.code, ErrorCode::StaleController, "{refusal:?}");
                }
            }
            // A query needs no epoch.
            assert!(matches!(
                controller
                    .call(
                        RunnerOp::Find {
                            workspace: ws.clone(),
                            glob: None,
                            offset: None,
                            limit: None,
                        },
                        None
                    )
                    .await,
                Ok(RunnerOpResult::Find(_))
            ));
            assert!(host.runner.inventory().is_empty());
            assert!(!marker.exists() && !root.path().join("added.txt").exists());
            assert!(!host.stop.is_requested());
            // Under its epoch, the controller's change is made.
            assert!(matches!(
                controller
                    .call(
                        RunnerOp::Exec {
                            workspace: ws.clone(),
                            request: command(&["/bin/sleep", "30"], "proc-c", 60_000, false),
                        },
                        Some(1)
                    )
                    .await,
                Ok(RunnerOpResult::Exec(_))
            ));
            assert!(matches!(
                controller.call(RunnerOp::Detach, None).await,
                Ok(RunnerOpResult::Detached)
            ));
            assert_eq!(host.served().await, Served::Detached);
            // A change prepared under the first Claim, sent after the second.
            let mut next = host.connect_raw().await;
            assert_eq!(next.claim(STORE).await.unwrap().epoch, 2);
            for op in [
                RunnerOp::Terminate {
                    process_id: ProcessId("proc-c".into()),
                },
                touch("proc-d"),
            ] {
                let refusal = next.call(op, Some(1)).await.unwrap_err();
                assert_eq!(refusal.code, ErrorCode::StaleController, "{refusal:?}");
            }
            assert_eq!(
                host.runner
                    .process_status(&ProcessId("proc-c".into()))
                    .await
                    .unwrap()
                    .state,
                ProcessState::Running
            );
            assert_eq!(host.runner.inventory().len(), 1);
            assert!(!marker.exists());
        }

        /// A Claim made once its controller's connection ended waits for that connection's
        /// requests, which may still change a process, and is granted once they end (P1R-C03,
        /// risk 12). The wait is measured.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_claim_waits_for_the_requests_of_the_controller_before_it() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let first = host
                .connect()
                .await
                .with_call_deadline(Duration::from_millis(200));
            // A process that never reads its stdin: a large write to it waits for its timeout,
            // which runs from its spawn.
            let stuck = ProcessId("proc-stuck".into());
            let stuck_since = Instant::now();
            first
                .exec(&ws, command(&["/bin/sleep", "30"], &stuck.0, 1_500, false))
                .await
                .unwrap();
            let err = first
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
            let mut next = host.connect_raw().await;
            let asked = Instant::now();
            let claim = next.claim(STORE).await.unwrap();
            let waited = asked.elapsed();
            eprintln!(
                "p1r-c03 claim latency: waited {waited:?} for the earlier controller's write to \
                 end with its process's 1.5 s timeout"
            );
            assert_eq!(claim.epoch, 2);
            assert!(
                stuck_since.elapsed() >= Duration::from_millis(1_500),
                "granted before the write ended: {:?}",
                stuck_since.elapsed()
            );
            assert!(waited < CLAIM_WAIT_BOUND, "{waited:?}");
            assert_eq!(host.served().await, Served::Lost);
        }

        /// Past its bound, a Claim that waits for the requests of the controller before it is
        /// refused and changes nothing: the next Claim, once they ended, is the Runner's
        /// second (P1R-C03, risk 12). The wait is measured.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_claim_past_its_wait_bound_is_refused_and_changes_nothing() {
            let mut host = Host::start_with(|independent| {
                independent.with_claim_wait(Duration::from_millis(300))
            });
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let first = host
                .connect()
                .await
                .with_call_deadline(Duration::from_millis(200));
            let stuck = ProcessId("proc-stuck".into());
            first
                .exec(&ws, command(&["/bin/sleep", "30"], &stuck.0, 3_000, false))
                .await
                .unwrap();
            assert!(first
                .write_stdin(RunnerWriteStdin {
                    process_id: stuck.clone(),
                    data: "x".repeat(4 * 1024 * 1024),
                })
                .await
                .is_err());
            let mut next = host.connect_raw().await;
            let asked = Instant::now();
            let (refusal, message) = next.claim(STORE).await.unwrap_err();
            let waited = asked.elapsed();
            eprintln!(
                "p1r-c03 claim latency: refused after {waited:?} past a wait bound of 300 ms"
            );
            assert_eq!(refusal, ClaimRefusal::RequestsRunning, "{message}");
            assert!(message.contains("did not end within"), "{message}");
            assert!(
                waited >= Duration::from_millis(300) && waited < Duration::from_secs(2),
                "{waited:?}"
            );
            assert_eq!(host.served().await, Served::Lost);
            let claim = next.claim(STORE).await.unwrap();
            assert_eq!((claim.epoch, claim.previous_controller), (2, true));
        }

        /// A client that claimed its Runner claims it again once its connection is lost,
        /// refusing every request with RUNNER_RECONNECTING meanwhile. The processes keep
        /// running, nothing runs twice, each exit reaches the client once, the Claim again is
        /// the next epoch, and a request prepared under the lost Claim is refused (P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_client_claims_its_runner_again_after_a_lost_connection() {
            let host = Host::start();
            let relay = Relay::start(&host.socket);
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let (on_exit, mut exited_ids) = exits();
            let (observer, mut events) = link_events();
            let (gateway, claim) =
                UdsRunner::claim_independent(&relay.socket, STORE, on_exit, observer)
                    .await
                    .unwrap();
            assert_eq!((claim.epoch, claim.previous_controller), (1, false));
            let long = ProcessId("proc-long".into());
            let gated = ProcessId("proc-gated".into());
            gateway
                .exec(&ws, command(&["/bin/sleep", "30"], &long.0, 60_000, false))
                .await
                .unwrap();
            gateway
                .exec(
                    &ws,
                    command(
                        &["/bin/sh", "-c", "while [ ! -e go ]; do sleep 0.02; done"],
                        &gated.0,
                        60_000,
                        false,
                    ),
                )
                .await
                .unwrap();
            relay.hold(true);
            relay.cut();
            assert_eq!(next_link_event(&mut events).await, LinkEvent::Lost);
            // Nothing is sent meanwhile.
            assert_eq!(
                code(gateway.process_status(&long).await.unwrap_err()),
                ErrorCode::RunnerReconnecting
            );
            assert_eq!(
                gateway.dispatch_epoch().unwrap_err().code,
                ErrorCode::RunnerReconnecting
            );
            let touch = command(&["/usr/bin/touch", "marker"], "proc-new", 5_000, false);
            assert_eq!(
                code(gateway.exec(&ws, touch.clone()).await.unwrap_err()),
                ErrorCode::RunnerReconnecting
            );
            // A process that ends while the client reconnects.
            std::fs::write(root.path().join("go"), "").unwrap();
            exited(&host.runner, &gated).await;
            let mut claims = gateway.claims();
            relay.hold(false);
            match next_link_event(&mut events).await {
                LinkEvent::Reclaimed(claim) => {
                    assert_eq!((claim.epoch, claim.previous_controller), (2, true));
                    assert_eq!(claim.barrier, ["demo"]);
                    // The new Claim's inventory accounts for the exit: its event, which the
                    // Runner sends again, is not delivered (P1R-C04).
                    assert_eq!(claim.events_through, 1);
                    let gated_status = claim
                        .processes
                        .iter()
                        .find(|entry| entry.status.process_id == gated)
                        .map(|entry| entry.status.state);
                    assert_eq!(gated_status, Some(ProcessState::Exited), "{claim:?}");
                    assert_eq!(claim.processes.len(), 2, "{claim:?}");
                }
                other => panic!("unexpected {other:?}"),
            }
            assert_eq!(gateway.claimed().map(|claim| claim.epoch), Some(2));
            // Whoever reconciles after a Claim is told of each one.
            claims.changed().await.unwrap();
            assert_eq!(claims.borrow().as_ref().map(|claim| claim.epoch), Some(2));
            assert_eq!(
                gateway.process_status(&long).await.unwrap().state,
                ProcessState::Running
            );
            // A request prepared under the lost Claim is refused, and nothing new starts.
            assert_eq!(
                code(
                    gateway
                        .at_epoch(1)
                        .exec(&ws, touch.clone())
                        .await
                        .unwrap_err()
                ),
                ErrorCode::StaleController
            );
            assert_eq!(
                code(gateway.exec(&ws, touch).await.unwrap_err()),
                ErrorCode::RecoveryPending
            );
            gateway.terminate(&long).await.unwrap();
            assert_eq!(
                next_exit(&mut exited_ids).await.as_deref(),
                Some("proc-long")
            );
            no_more_exits(&mut exited_ids).await;
            assert_eq!(host.runner.inventory().len(), 2);
            assert!(!root.path().join("marker").exists());
        }

        /// A Claim whose answer is lost is asked again over a new connection: the Runner let go
        /// of the lost connection it had granted the Claim to, and grants the next one
        /// (P1R-C03). The time to the second grant is measured.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_claim_whose_answer_is_lost_is_asked_again() {
            let host = Host::start();
            let relay = Relay::start(&host.socket);
            // The `Hello` is answered; the Claim's answer is lost with its connection.
            relay.lose_answer(2);
            let (observer, mut events) = link_events();
            let asked = Instant::now();
            let (gateway, claim) = UdsRunner::claim_independent(
                &relay.socket,
                STORE,
                Arc::new(|_: &str| {}),
                observer,
            )
            .await
            .unwrap();
            let elapsed = asked.elapsed();
            eprintln!(
                "p1r-c03 claim latency: a Claim whose answer was lost was granted again after \
                 {elapsed:?}"
            );
            assert_eq!(
                claim.epoch, 2,
                "the lost connection held the first: {claim:?}"
            );
            assert!(elapsed < CLAIM_START_BOUND, "{elapsed:?}");
            assert!(matches!(events.try_recv(), Ok(LinkEvent::Retrying { .. })));
            assert_eq!(gateway.dispatch_epoch().unwrap(), Some(2));
            let granted = host
                .claims()
                .into_iter()
                .filter(|note| matches!(note, ClaimNote::Claimed { .. }))
                .count();
            assert_eq!(granted, 2);
        }

        /// While a client reconnects, another process of the same user can claim the Runner
        /// (D6): the client's Claims are refused while that connection is open, and it claims
        /// the Runner once that connection ends (P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_runner_claimed_by_another_connection_meanwhile_is_claimed_once_it_ends() {
            let host = Host::start();
            let relay = Relay::start(&host.socket);
            let (observer, mut events) = link_events();
            let (gateway, _) = UdsRunner::claim_independent(
                &relay.socket,
                STORE,
                Arc::new(|_: &str| {}),
                observer,
            )
            .await
            .unwrap();
            relay.hold(true);
            relay.cut();
            assert_eq!(next_link_event(&mut events).await, LinkEvent::Lost);
            let mut other = host.connect_raw().await;
            assert_eq!(other.claim(STORE).await.unwrap().epoch, 2);
            relay.hold(false);
            let refused = match tokio::time::timeout(Duration::from_secs(20), events.recv())
                .await
                .unwrap()
                .unwrap()
            {
                LinkEvent::Retrying { reason, .. } => reason,
                other => panic!("unexpected {other:?}"),
            };
            assert!(refused.contains("still open"), "{refused}");
            assert_eq!(
                code(
                    gateway
                        .process_status(&ProcessId("proc-1".into()))
                        .await
                        .unwrap_err()
                ),
                ErrorCode::RunnerReconnecting
            );
            drop(other);
            match next_link_event(&mut events).await {
                LinkEvent::Reclaimed(claim) => assert_eq!(claim.epoch, 3),
                other => panic!("unexpected {other:?}"),
            }
            assert_eq!(gateway.dispatch_epoch().unwrap(), Some(3));
        }

        /// A client stops claiming its Runner again once another Runner answers at the socket
        /// or nothing accepts connections there, and refuses every request with
        /// RUNNER_DISCONNECTED from then on, saying why (P1R-C03).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_client_that_cannot_claim_its_runner_again_is_disconnected() {
            let host = Host::start();
            let other = Host::start_with(|independent| {
                independent.with_identity(RunnerIdentity {
                    boot_id: None,
                    pid: 1,
                    started_at_ms: 1,
                    nonce: "another-runner".into(),
                })
            });
            let relay = Relay::start(&host.socket);
            let (observer, mut events) = link_events();
            let (gateway, _) = UdsRunner::claim_independent(
                &relay.socket,
                STORE,
                Arc::new(|_: &str| {}),
                observer,
            )
            .await
            .unwrap();
            relay.retarget(&other.socket);
            relay.cut();
            assert_eq!(next_link_event(&mut events).await, LinkEvent::Lost);
            match next_link_event(&mut events).await {
                LinkEvent::Gone(reason) => {
                    assert!(reason.contains("another Runner answers"), "{reason}")
                }
                other => panic!("unexpected {other:?}"),
            }
            let refusal = disconnected(&gateway, &ProcessId("proc-1".into())).await;
            assert!(
                refusal.message.contains("another Runner answers"),
                "{refusal:?}"
            );
            // The other Runner was asked nothing but its `Hello`.
            assert!(other.claims().is_empty());

            let mut stopping = Host::start();
            let (observer, mut events) = link_events();
            let (gateway, _) = UdsRunner::claim_independent(
                &stopping.socket,
                STORE,
                Arc::new(|_: &str| {}),
                observer,
            )
            .await
            .unwrap();
            stopping.stop.request();
            assert_eq!(stopping.served().await, Served::Stopped { terminated: 0 });
            assert_eq!(next_link_event(&mut events).await, LinkEvent::Lost);
            match next_link_event(&mut events).await {
                LinkEvent::Gone(reason) => {
                    assert!(reason.contains("nothing accepts connections"), "{reason}")
                }
                other => panic!("unexpected {other:?}"),
            }
            disconnected(&gateway, &ProcessId("proc-1".into())).await;
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
            let stopped = gateway.stop_runner().await.unwrap();
            assert!(
                started.elapsed() < STOP_DRAIN_BOUND,
                "{:?}",
                started.elapsed()
            );
            assert_eq!(stopped.terminated, 2);
            // The answer states how each process ended (P1R-C04).
            let ended: Vec<_> = stopped
                .processes
                .iter()
                .map(|entry| {
                    (
                        entry.status.process_id.0.as_str(),
                        entry.status.state,
                        entry.status.termination,
                    )
                })
                .collect();
            assert_eq!(
                ended,
                [
                    (
                        "proc-pipe",
                        ProcessState::Exited,
                        Some(ProcessTermination::Terminated)
                    ),
                    (
                        "proc-tty",
                        ProcessState::Exited,
                        Some(ProcessTermination::Terminated)
                    )
                ]
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

        /// A stop request, as the Runner's own SIGTERM or SIGINT makes, ends the controller's
        /// connection and the processes; the gateway's later requests are refused, and a Claim
        /// meanwhile is refused.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_stop_request_ends_the_connection_and_the_processes() {
            let mut host = Host::start();
            let root = tempfile::tempdir().unwrap();
            let ws = workspace(root.path());
            let gateway = host.connect().await;
            let mut other = host.connect_raw().await;
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
            // The other connection ended with the stop.
            assert!(other.next_response().await.is_none());
        }

        /// A connection that ends without a word is a loss: the processes keep running, and an
        /// exit while no gateway controls the Runner reaches the next controller in its Claim's
        /// inventory, not as an event (P1R-C04); later exits come as events.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_lost_connection_keeps_the_processes_and_the_next_claim_accounts_for_exits() {
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
            let (on_exit, mut exited_ids) = exits();
            let second = host.connect_reporting_exits(on_exit).await;
            let claim = second.claimed().unwrap();
            assert!(claim.previous_controller);
            assert_eq!(claim.events_through, 1);
            let listed: Vec<_> = claim
                .processes
                .iter()
                .map(|entry| (entry.status.process_id.0.as_str(), entry.status.state))
                .collect();
            assert_eq!(
                listed,
                [
                    ("proc-long", ProcessState::Running),
                    ("proc-quick", ProcessState::Exited)
                ]
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
            no_more_exits(&mut exited_ids).await;
        }

        /// A controller whose connection ends right after its `Detach` or `Stop` still detaches
        /// or stops: the Runner acts on what it read, though its answer is lost.
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
            for (epoch, op, ending) in [
                (2, RunnerOp::Detach, Served::Detached),
                (3, RunnerOp::Stop, Served::Stopped { terminated: 1 }),
            ] {
                let mut raw = host.connect_raw().await;
                assert_eq!(raw.claim(STORE).await.unwrap().epoch, epoch);
                raw.send(op, Some(epoch)).await;
                drop(raw);
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
