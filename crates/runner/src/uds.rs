//! Unix-socket Runner client. Codex types stay out of this crate.
//!
//! `Host` + `UdsRunner` is opt-in **transport** on the same host. It does
//! not claim Linux container isolation. P0 is 1:1: one gateway connection
//! per worker. Replay is connection-local.
//!
//! An independent Runner (P1R-C01) outlives the gateway's connection. The gateway takes control
//! of it with a `Claim` (P1R-C03), and each request that changes a process or the workspace
//! carries the epoch of that Claim. A client that claimed through
//! [`UdsRunner::claim_independent`] connects and claims the same Runner again when its
//! connection is lost, pausing longer after each failed try up to [`RECLAIM_BACKOFF_MAX`], and
//! refuses requests with `RUNNER_RECONNECTING` meanwhile; once it cannot claim that Runner
//! again, it refuses them with `RUNNER_DISCONNECTED`. Any other client refuses every request
//! with `RUNNER_DISCONNECTED` once its connection to an independent Runner ended.
//!
//! The Runner's answer to a Claim holds every process it holds and the number of the last exit
//! event before it took that inventory: the client delivers only the exit events after it
//! (P1R-C04), as the inventory accounts for the earlier ones.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, watch, Mutex};
use tokio::time::timeout;

use crate::wire::{
    read_frame, write_frame, ClaimRefusal, RecalledProcess, RunnerClaim, RunnerEvent,
    RunnerInventoryEntry, RunnerOp, RunnerOpResult, WireEnvelope, WireKind, WIRE_PROTOCOL,
};
use crate::{
    Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerError, RunnerExecRequest,
    RunnerExecResult, RunnerIdentity, RunnerLifecycle, RunnerProcessStatus, RunnerReadProcess,
    RunnerReadResult, RunnerResizeResult, RunnerWriteStdin, ShellRelease,
};

/// Transport deadline for one RPC. Longer than the default exec timeout
/// so `apply_patch` is not cut short; short enough to fail a hung
/// mismatch instead of waiting forever.
pub const RUNNER_CALL_DEADLINE: Duration = Duration::from_secs(120);
/// How long a gateway that starts with an independent Runner keeps claiming it while another
/// connection controls it, or that connection's requests run (P1R-C03).
pub const CLAIM_START_BOUND: Duration = Duration::from_secs(30);
/// The pause after a client's first failed try to claim its independent Runner, doubled after
/// each further one up to [`RECLAIM_BACKOFF_MAX`] (P1R-C03).
const RECLAIM_BACKOFF_START: Duration = Duration::from_millis(100);
/// The longest pause between two tries to claim an independent Runner again (P1R-C03).
pub const RECLAIM_BACKOFF_MAX: Duration = Duration::from_secs(5);

pub type DisconnectHook = Arc<dyn Fn() + Send + Sync>;

/// What the worker's `Hello` stated about its lifecycle (P1R-C01).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerHello {
    pub lifecycle: RunnerLifecycle,
    pub identity: Option<RunnerIdentity>,
}

/// The refusal of a request to an independent Runner that this gateway cannot reach again
/// (P1R-C01, P1R-C03): the request was not sent.
pub fn runner_disconnected(reason: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RunnerDisconnected,
        format!(
            "this gateway cannot reach its independent Runner again ({reason}), so the request \
             was not sent. Restart the gateway once the Runner can be reached; a Runner that did \
             not stop keeps its processes running"
        ),
    )
}

/// The refusal of a request while this gateway claims its independent Runner again
/// (P1R-C03): the request was not sent.
pub fn runner_reconnecting() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::RunnerReconnecting,
        "this gateway lost its connection to the independent Runner and is claiming it again, \
         so the request was not sent; the Runner's processes keep running. Retry once the \
         gateway has reconnected",
    )
}

/// What becomes of a client's connection to the independent Runner it claimed (P1R-C03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// The connection was lost; the client claims the Runner again.
    Lost,
    /// A try to claim the Runner again failed, and why; the next comes after `retry`.
    Retrying { reason: String, retry: Duration },
    /// The Runner was claimed again.
    Reclaimed(RunnerClaim),
    /// The Runner cannot be claimed again, and why: every later request is refused with
    /// `RUNNER_DISCONNECTED`.
    Gone(String),
}

pub type LinkObserver = Arc<dyn Fn(LinkEvent) + Send + Sync>;

/// How a stopped independent Runner left its processes (P1R-C01, P1R-C04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerStopped {
    /// How many processes were still running when the Runner terminated them.
    pub terminated: u32,
    /// Every process the Runner held when it answered, with its last status.
    pub processes: Vec<RunnerInventoryEntry>,
}

/// Why a client does not control an independent Runner (P1R-C03).
#[derive(Debug, Clone)]
pub enum ClaimError {
    /// The Runner refused the Claim.
    Refused {
        refusal: ClaimRefusal,
        message: String,
    },
    /// The worker is not the independent Runner this client claims.
    NotIndependent(String),
    /// The Claim could not be sent, or its answer was lost.
    Transport(RunnerError),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { refusal, message } => {
                write!(
                    f,
                    "the Runner refused the Claim ({}): {message}",
                    refusal.as_str()
                )
            }
            Self::NotIndependent(message) => f.write_str(message),
            Self::Transport(err) => write!(f, "{err}"),
        }
    }
}

/// One connection to the worker.
struct Link {
    writer: Mutex<tokio::net::unix::OwnedWriteHalf>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<WireEnvelope, RunnerError>>>>,
    alive: AtomicBool,
    /// The Runner's answer to this connection's Claim (P1R-C03).
    claim: OnceLock<RunnerClaim>,
}

/// Which connection a client's requests go over.
enum State {
    Linked(Arc<Link>),
    /// The connection was lost and the client claims the Runner again (P1R-C03).
    Reclaiming,
    /// The Runner cannot be claimed again, and why (P1R-C03).
    Gone(String),
}

/// Where and for what a client claims its independent Runner again (P1R-C03).
struct Reclaim {
    socket: PathBuf,
    store_id: String,
    observer: LinkObserver,
}

/// How one try to claim an independent Runner ended.
enum Attempt {
    /// Trying again may succeed.
    Retry(String),
    /// Trying again cannot succeed.
    Gone(String),
}

struct Client {
    next_id: AtomicU64,
    on_process_exit: ShellRelease,
    on_disconnect: DisconnectHook,
    /// What the worker's first `Hello` stated (P1R-C01).
    hello: OnceLock<RunnerHello>,
    /// The worker's `Hello` stated that it answers `Registration` (CSRG-U2).
    #[cfg(feature = "devguard")]
    registration: AtomicBool,
    /// The worker's `Hello` stated that it answers `Prepare` (CSRG-U3).
    #[cfg(feature = "devguard")]
    admission: AtomicBool,
    /// The worker's `Hello` stated that it answers `Launch` (CSRG-U4).
    #[cfg(feature = "devguard")]
    launch: AtomicBool,
    state: std::sync::Mutex<State>,
    /// Set once the client claimed an independent Runner that it claims again when its
    /// connection is lost (P1R-C03).
    reclaim: OnceLock<Reclaim>,
    /// The gateway leaves the Runner: no connection is made again.
    leaving: AtomicBool,
    /// The number of the last exit event delivered, or accounted for by a Claim's inventory
    /// (P1R-C03, P1R-C04).
    last_event: AtomicU64,
    /// The Claim of the client's current connection, once it has one (P1R-C04).
    claims: watch::Sender<Option<RunnerClaim>>,
}

pub struct UdsRunner {
    client: Arc<Client>,
    call_deadline: Duration,
    /// The controller epoch this handle's requests carry instead of the current Claim's
    /// (P1R-C03).
    epoch: Option<u64>,
}

impl Clone for UdsRunner {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            call_deadline: self.call_deadline,
            epoch: self.epoch,
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
        let client = Arc::new(Client::new(on_process_exit, on_disconnect));
        let link = client.open_link(stream);
        *client.state() = State::Linked(link);
        Self {
            client,
            call_deadline: RUNNER_CALL_DEADLINE,
            epoch: None,
        }
    }

    /// This client with another deadline per call than [`RUNNER_CALL_DEADLINE`]. For tests.
    #[doc(hidden)]
    pub fn with_call_deadline(mut self, deadline: Duration) -> Self {
        self.call_deadline = deadline;
        self
    }

    pub async fn handshake(&self) -> Result<(), RunnerError> {
        match self.call(RunnerOp::Hello).await {
            Ok(hello @ RunnerOpResult::Hello { protocol, .. }) if protocol == WIRE_PROTOCOL => {
                if let RunnerOpResult::Hello {
                    lifecycle,
                    identity,
                    ..
                } = &hello
                {
                    let _ = self.client.hello.set(RunnerHello {
                        lifecycle: *lifecycle,
                        identity: identity.clone(),
                    });
                }
                #[cfg(feature = "devguard")]
                if let RunnerOpResult::Hello {
                    registration,
                    admission,
                    launch,
                    ..
                } = hello
                {
                    self.client
                        .registration
                        .store(registration, Ordering::SeqCst);
                    self.client.admission.store(admission, Ordering::SeqCst);
                    self.client.launch.store(launch, Ordering::SeqCst);
                }
                #[cfg(not(feature = "devguard"))]
                let _ = hello;
                Ok(())
            }
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

    /// What the worker's `Hello` stated about its lifecycle (P1R-C01); before the handshake, a
    /// managed worker's defaults.
    pub fn hello(&self) -> RunnerHello {
        self.client.hello.get().cloned().unwrap_or_default()
    }

    /// Take control of the independent Runner over this connection, for the operations
    /// database `store_id` (P1R-C03). The connection's later requests carry the Claim's epoch.
    pub async fn claim(&self, store_id: &str) -> Result<RunnerClaim, ClaimError> {
        let link = self.client.current().map_err(ClaimError::Transport)?;
        let claim = self
            .client
            .claim_on(&link, store_id, self.call_deadline)
            .await?;
        self.client.claims.send_replace(Some(claim.clone()));
        Ok(claim)
    }

    /// The Claim of this client's current connection, and each later one as the client claims
    /// its Runner again (P1R-C04).
    pub fn claims(&self) -> watch::Receiver<Option<RunnerClaim>> {
        self.client.claims.subscribe()
    }

    /// Connect to the independent Runner at `socket` and claim it for the operations database
    /// `store_id` (P1R-C03). While another connection controls the Runner, or that connection's
    /// requests run, and when the Claim's answer was lost, the Claim is asked again over a new
    /// connection with a growing pause for up to [`CLAIM_START_BOUND`]; any other refusal,
    /// nothing accepting connections at `socket`, and a worker that is not an independent
    /// Runner stating its identity, end the try. Once claimed, the client claims the same
    /// Runner again whenever its connection is lost, and tells `observer` how that goes.
    pub async fn claim_independent(
        socket: &Path,
        store_id: &str,
        on_process_exit: ShellRelease,
        observer: LinkObserver,
    ) -> Result<(Self, RunnerClaim), ClaimError> {
        let client = Arc::new(Client::new(on_process_exit, Arc::new(|| {})));
        let runner = Self {
            client: client.clone(),
            call_deadline: RUNNER_CALL_DEADLINE,
            epoch: None,
        };
        let deadline = tokio::time::Instant::now() + CLAIM_START_BOUND;
        let mut backoff = RECLAIM_BACKOFF_START;
        loop {
            let stream = UnixStream::connect(socket).await.map_err(|err| {
                ClaimError::Transport(RunnerError::before_dispatch(format!(
                    "connect {}: {err}",
                    socket.display()
                )))
            })?;
            let link = client.open_link(stream);
            *client.state() = State::Linked(link.clone());
            let claimed = match runner.first_or_same_hello(socket).await {
                Ok(()) => runner.claim(store_id).await,
                Err(err) => Err(err),
            };
            match claimed {
                Ok(claim) => {
                    let _ = client.reclaim.set(Reclaim {
                        socket: socket.to_path_buf(),
                        store_id: store_id.to_string(),
                        observer,
                    });
                    // A connection lost before the client was set to claim again.
                    if !link.alive.load(Ordering::SeqCst) {
                        client.start_reclaim(&link);
                    }
                    return Ok((runner, claim));
                }
                Err(err)
                    if tokio::time::Instant::now() + backoff < deadline
                        && match &err {
                            ClaimError::Refused { refusal, .. } => refusal.is_transient(),
                            // The connection was lost after the Runner's `Hello`, perhaps with
                            // the Claim's answer: a Runner that granted it lets go once that
                            // connection ended.
                            ClaimError::Transport(_) => client.hello.get().is_some(),
                            ClaimError::NotIndependent(_) => false,
                        } =>
                {
                    observer(LinkEvent::Retrying {
                        reason: match err {
                            ClaimError::Refused { message, .. } => message,
                            err => err.to_string(),
                        },
                        retry: backoff,
                    });
                    client.close_link(&link).await;
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECLAIM_BACKOFF_MAX);
                }
                Err(err) => {
                    client.leaving.store(true, Ordering::SeqCst);
                    client.close_link(&link).await;
                    return Err(err);
                }
            }
        }
    }

    /// The handshake of a client's first connection, which must find an independent Runner
    /// that states its identity, or the `Hello` of a later one, which must find the same Runner.
    async fn first_or_same_hello(&self, socket: &Path) -> Result<(), ClaimError> {
        if self.client.hello.get().is_none() {
            self.handshake().await.map_err(ClaimError::Transport)?;
            let hello = self.hello();
            if hello.lifecycle != RunnerLifecycle::Independent {
                return Err(ClaimError::NotIndependent(format!(
                    "the worker at {} is {}, not an independent Runner",
                    socket.display(),
                    hello.lifecycle.as_str()
                )));
            }
            if hello.identity.is_none() {
                return Err(ClaimError::NotIndependent(format!(
                    "the Runner at {} did not state its identity",
                    socket.display()
                )));
            }
            return Ok(());
        }
        let link = self.client.current().map_err(ClaimError::Transport)?;
        match self.client.same_runner(&link).await {
            Ok(()) => Ok(()),
            Err(Attempt::Retry(message)) => {
                Err(ClaimError::Transport(RunnerError::before_dispatch(message)))
            }
            Err(Attempt::Gone(message)) => Err(ClaimError::NotIndependent(message)),
        }
    }

    /// The Claim of this client's current connection, if it claimed the Runner (P1R-C03).
    pub fn claimed(&self) -> Option<RunnerClaim> {
        match &*self.client.state() {
            State::Linked(link) if link.alive.load(Ordering::SeqCst) => link.claim.get().cloned(),
            _ => None,
        }
    }

    /// This client, sending every request under the controller epoch `epoch` instead of the
    /// current Claim's (P1R-C03): a request prepared under one Claim is refused by the Runner
    /// once another Claim replaced it.
    pub fn at_epoch(&self, epoch: u64) -> Self {
        Self {
            epoch: Some(epoch),
            ..self.clone()
        }
    }

    /// The controller epoch under which this client sends a new execution, patch or approval
    /// resume, none for a worker that is not claimed, or why it starts none (P1R-C01,
    /// P1R-C03): the connection to its independent Runner is lost or being made again. Where
    /// an earlier controller started work, the Runner itself refuses new work until the
    /// gateway reconciled that workspace (P1R-C04).
    pub fn dispatch_epoch(&self) -> Result<Option<u64>, ErrorBody> {
        if !self.client.independent() {
            return Ok(None);
        }
        let link = self
            .client
            .current()
            .map_err(RunnerError::into_error_body)?;
        match link.claim.get() {
            Some(claim) => Ok(Some(self.epoch.unwrap_or(claim.epoch))),
            None => Ok(self.epoch),
        }
    }

    /// Why this client starts no new execution, patch or approval resume, if it does not: see
    /// [`Self::dispatch_epoch`].
    pub fn new_work_refusal(&self) -> Option<ErrorBody> {
        self.dispatch_epoch().err()
    }

    /// Leave the independent Runner, whose processes, output and timeouts carry on, and close
    /// the connection (P1R-C01). No connection is made again.
    pub async fn detach(&self) -> Result<(), RunnerError> {
        self.client.leaving.store(true, Ordering::SeqCst);
        let detached = match self.call(RunnerOp::Detach).await {
            Ok(RunnerOpResult::Detached) => Ok(()),
            Ok(other) => Err(unexpected(other)),
            Err(err) => Err(err),
        };
        self.close().await;
        detached
    }

    /// Stop the independent Runner: it ends every process and exits (P1R-C01). How many
    /// processes were still running, and how each process it held ended (P1R-C04). No
    /// connection is made again.
    pub async fn stop_runner(&self) -> Result<RunnerStopped, RunnerError> {
        self.client.leaving.store(true, Ordering::SeqCst);
        let stopped = match self.call(RunnerOp::Stop).await {
            Ok(RunnerOpResult::Stopped {
                terminated,
                processes,
            }) => Ok(RunnerStopped {
                terminated,
                processes,
            }),
            Ok(other) => Err(unexpected(other)),
            Err(err) => Err(err),
        };
        self.close().await;
        stopped
    }

    /// What the independent Runner knows of each of `process_ids` (P1R-C04), in that order.
    pub async fn recall(
        &self,
        process_ids: &[ProcessId],
    ) -> Result<Vec<RecalledProcess>, RunnerError> {
        match self
            .call(RunnerOp::Recall {
                process_ids: process_ids.to_vec(),
            })
            .await?
        {
            RunnerOpResult::Recalled(recalled) if recalled.len() == process_ids.len() => {
                Ok(recalled)
            }
            other => Err(unexpected(other)),
        }
    }

    /// Tell the independent Runner that this gateway reconciled these workspaces (P1R-C04): it
    /// starts work there for this gateway again.
    pub async fn reconciled(&self, workspace_ids: &[String]) -> Result<(), RunnerError> {
        match self
            .call(RunnerOp::Reconciled {
                workspace_ids: workspace_ids.to_vec(),
            })
            .await?
        {
            RunnerOpResult::Reconciled => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Tell the independent Runner that this gateway recorded how these processes ended
    /// (P1R-C04): it keeps them only as long as any other ended process from now on.
    pub async fn acknowledge(&self, process_ids: &[ProcessId]) -> Result<(), RunnerError> {
        match self
            .call(RunnerOp::Acknowledge {
                process_ids: process_ids.to_vec(),
            })
            .await?
        {
            RunnerOpResult::Acknowledged => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Have the independent Runner look up, with the resource authority, the attempts it
    /// admitted for these processes (P1R-C04), in that order.
    #[cfg(feature = "devguard")]
    pub async fn look_up_attempts(
        &self,
        attempts: Vec<crate::admission::AttemptRef>,
    ) -> Result<Vec<crate::admission::AttemptLookup>, RunnerError> {
        let asked = attempts.len();
        match self.call(RunnerOp::Lookup { attempts }).await? {
            RunnerOpResult::LookedUp(found) if found.len() == asked => Ok(found),
            other => Err(unexpected(other)),
        }
    }

    /// Whether the worker's `Hello` stated that it answers `Registration` (CSRG-U2).
    #[cfg(feature = "devguard")]
    pub fn states_registration(&self) -> bool {
        self.client.registration.load(Ordering::SeqCst)
    }

    /// The worker's report of its own DevGuard registration (CSRG-U2), or why it could not be
    /// had: `unsupported_mode` from a worker that cannot register, `owner_unreachable` when the
    /// worker did not answer. A worker that did not state `registration` in its `Hello` is
    /// never asked: it would close the connection.
    #[cfg(feature = "devguard")]
    pub async fn registration(
        &self,
    ) -> Result<codespace_domain::ResourceAuthorityInfo, codespace_domain::ResourceRegistrationState>
    {
        use codespace_domain::ResourceRegistrationState;
        if !self.states_registration() {
            return Err(ResourceRegistrationState::UnsupportedMode);
        }
        match self.call(RunnerOp::Registration).await {
            Ok(RunnerOpResult::Registration(info)) => Ok(info),
            Err(RunnerError::Execution(body))
                if matches!(
                    body.code,
                    ErrorCode::RunnerDisconnected
                        | ErrorCode::RunnerReconnecting
                        | ErrorCode::StaleController
                ) =>
            {
                Err(ResourceRegistrationState::OwnerUnreachable)
            }
            Err(RunnerError::Execution(_)) => Err(ResourceRegistrationState::UnsupportedMode),
            _ => Err(ResourceRegistrationState::OwnerUnreachable),
        }
    }

    /// Whether the worker's `Hello` stated that it answers `Prepare` (CSRG-U3).
    #[cfg(feature = "devguard")]
    pub fn states_admission(&self) -> bool {
        self.client.admission.load(Ordering::SeqCst)
    }

    /// Have the worker prepare a governed execution under its own registration (CSRG-U3). The
    /// worker reports how the preparation ended; it never starts the command. A worker that did
    /// not state `admission` is never asked. When no report comes back the worker may have
    /// asked for admission, so the attempt is unknown; when the request never left, nothing
    /// was asked.
    #[cfg(feature = "devguard")]
    pub async fn prepare(
        &self,
        ws: &Workspace,
        request: RunnerExecRequest,
        attempt_id: String,
    ) -> Result<crate::admission::ManagedPreparation, RunnerError> {
        use crate::admission::{AttemptState, ManagedPreparation, PreparationOutcome};
        use codespace_domain::ResourceRegistrationState;
        if !self.states_admission() {
            return Err(RunnerError::execution(ErrorBody::new(
                ErrorCode::ResourcePolicyUnsupported,
                "this worker cannot prepare governed executions",
            )));
        }
        let unanswered = |outcome, attempt| ManagedPreparation {
            attempt_id: attempt_id.clone(),
            process_id: request.process_id.clone(),
            outcome,
            attempt,
            account: None,
        };
        match self
            .call(RunnerOp::Prepare {
                workspace: ws.clone(),
                request: request.clone(),
                attempt_id: attempt_id.clone(),
            })
            .await
        {
            Ok(RunnerOpResult::Prepared(report)) => Ok(report),
            Ok(other) => Err(unexpected(other)),
            Err(RunnerError::TransportBeforeDispatch { .. }) => Ok(unanswered(
                PreparationOutcome::AuthorityUnavailable {
                    registration: ResourceRegistrationState::OwnerUnreachable,
                    error_code: None,
                },
                AttemptState::NotAsked,
            )),
            Err(RunnerError::TransportAmbiguous { .. }) => Ok(unanswered(
                PreparationOutcome::Unknown,
                AttemptState::Unknown,
            )),
            Err(err) => Err(err),
        }
    }

    /// Whether the worker's `Hello` stated that it answers `Launch` (CSRG-U4).
    #[cfg(feature = "devguard")]
    pub fn states_launch(&self) -> bool {
        self.client.launch.load(Ordering::SeqCst)
    }

    /// Have the worker prepare a governed execution under its own registration and launch it
    /// once through its own launch helper (CSRG-U4). The request is never sent again. When the
    /// request never left, nothing was asked. When no report comes back, the worker may have
    /// launched it: the report is a launch whose dispatch is unknown, for its `process_id`.
    #[cfg(feature = "devguard")]
    pub async fn launch(
        &self,
        ws: &Workspace,
        request: RunnerExecRequest,
        attempt_id: String,
    ) -> Result<crate::launch::LaunchReport, RunnerError> {
        use crate::admission::{AttemptState, ManagedPreparation, PreparationOutcome};
        use crate::launch::{LaunchDispatch, LaunchReport, ManagedLaunch};
        use codespace_domain::ResourceRegistrationState;
        if !self.states_launch() {
            return Err(RunnerError::execution(ErrorBody::new(
                ErrorCode::ResourcePolicyUnsupported,
                "this worker cannot launch governed executions",
            )));
        }
        let process_id = request.process_id.clone();
        match self
            .call(RunnerOp::Launch {
                workspace: ws.clone(),
                request,
                attempt_id: attempt_id.clone(),
            })
            .await
        {
            Ok(RunnerOpResult::Launched(report)) => Ok(report),
            Ok(other) => Err(unexpected(other)),
            Err(RunnerError::TransportBeforeDispatch { .. }) => {
                Ok(LaunchReport::NotLaunched(ManagedPreparation {
                    attempt_id,
                    process_id,
                    outcome: PreparationOutcome::AuthorityUnavailable {
                        registration: ResourceRegistrationState::OwnerUnreachable,
                        error_code: None,
                    },
                    attempt: AttemptState::NotAsked,
                    account: None,
                }))
            }
            Err(RunnerError::TransportAmbiguous { .. }) => {
                Ok(LaunchReport::Launched(ManagedLaunch {
                    attempt_id,
                    process_id,
                    dispatch: LaunchDispatch::Unknown,
                    phases: Vec::new(),
                    account: None,
                    scope_root_pid: None,
                    root: None,
                    deadline_at_ms: None,
                }))
            }
            Err(err) => Err(err),
        }
    }

    /// Close the connection. No connection is made again.
    pub async fn close(&self) {
        self.client.leaving.store(true, Ordering::SeqCst);
        let link = match &*self.client.state() {
            State::Linked(link) => Some(link.clone()),
            _ => None,
        };
        if let Some(link) = link {
            self.client.close_link(&link).await;
        }
    }

    async fn call(&self, op: RunnerOp) -> Result<RunnerOpResult, RunnerError> {
        let link = self.client.current()?;
        let epoch = self
            .epoch
            .or_else(|| link.claim.get().map(|claim| claim.epoch));
        self.client
            .call_on(&link, op, epoch, self.call_deadline)
            .await
    }
}

impl Client {
    fn new(on_process_exit: ShellRelease, on_disconnect: DisconnectHook) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            on_process_exit,
            on_disconnect,
            hello: OnceLock::new(),
            #[cfg(feature = "devguard")]
            registration: AtomicBool::new(false),
            #[cfg(feature = "devguard")]
            admission: AtomicBool::new(false),
            #[cfg(feature = "devguard")]
            launch: AtomicBool::new(false),
            state: std::sync::Mutex::new(State::Gone("not connected".into())),
            reclaim: OnceLock::new(),
            leaving: AtomicBool::new(false),
            last_event: AtomicU64::new(0),
            claims: watch::Sender::new(None),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn independent(&self) -> bool {
        self.hello
            .get()
            .is_some_and(|hello| hello.lifecycle == RunnerLifecycle::Independent)
    }

    /// A new connection over `stream`, read from now on. It carries no request until the
    /// client sends one over it.
    fn open_link(self: &Arc<Self>, stream: UnixStream) -> Arc<Link> {
        let (read, write) = stream.into_split();
        let link = Arc::new(Link {
            writer: Mutex::new(write),
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            claim: OnceLock::new(),
        });
        tokio::spawn(read_loop(read, self.clone(), link.clone()));
        link
    }

    /// The connection a request goes over, or why none can take it.
    fn current(&self) -> Result<Arc<Link>, RunnerError> {
        match &*self.state() {
            State::Linked(link) if link.alive.load(Ordering::SeqCst) => Ok(link.clone()),
            State::Linked(_) if !self.independent() => {
                Err(RunnerError::before_dispatch("runner socket closed"))
            }
            // A lost connection that is about to be made again.
            State::Linked(_)
                if self.reclaim.get().is_some() && !self.leaving.load(Ordering::SeqCst) =>
            {
                Err(RunnerError::execution(runner_reconnecting()))
            }
            State::Linked(_) if self.leaving.load(Ordering::SeqCst) => Err(RunnerError::execution(
                runner_disconnected("this gateway left the Runner"),
            )),
            State::Linked(_) => Err(RunnerError::execution(runner_disconnected(
                "the connection to the Runner was lost",
            ))),
            State::Reclaiming => Err(RunnerError::execution(runner_reconnecting())),
            State::Gone(reason) => Err(RunnerError::execution(runner_disconnected(reason))),
        }
    }

    async fn call_on(
        self: &Arc<Self>,
        link: &Arc<Link>,
        op: RunnerOp,
        epoch: Option<u64>,
        deadline: Duration,
    ) -> Result<RunnerOpResult, RunnerError> {
        let request_id = format!("rrpc-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        link.pending.lock().await.insert(request_id.clone(), tx);
        let envelope = WireEnvelope::request(request_id.clone(), op).with_epoch(epoch);
        let write_result = {
            let mut writer = link.writer.lock().await;
            write_envelope(&mut *writer, &envelope).await
        };
        if let Err(err) = write_result {
            link.pending.lock().await.remove(&request_id);
            if matches!(err, RunnerError::TransportAmbiguous { .. }) {
                self.stop_writing(link).await;
            }
            return Err(err);
        }
        match timeout(deadline, rx).await {
            Ok(Ok(Ok(response))) => decode_response(response),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(RunnerError::ambiguous("runner socket closed")),
            Err(_) => {
                self.close_link(link).await;
                Err(RunnerError::ambiguous("runner call deadline exceeded"))
            }
        }
    }

    /// Claim the Runner over `link` for the operations database `store_id` (P1R-C03).
    async fn claim_on(
        self: &Arc<Self>,
        link: &Arc<Link>,
        store_id: &str,
        deadline: Duration,
    ) -> Result<RunnerClaim, ClaimError> {
        let claim = RunnerOp::Claim {
            store_id: store_id.to_string(),
        };
        match self.call_on(link, claim, None, deadline).await {
            Ok(RunnerOpResult::Claimed(claim)) => {
                let _ = link.claim.set(claim.clone());
                Ok(claim)
            }
            Ok(RunnerOpResult::ClaimRefused { refusal, message }) => {
                Err(ClaimError::Refused { refusal, message })
            }
            Ok(other) => Err(ClaimError::Transport(unexpected(other))),
            Err(err) => Err(ClaimError::Transport(err)),
        }
    }

    /// A write over `link` failed, most often because the Runner had closed the connection: no
    /// request goes over it again. Answers the Runner wrote before it closed are still read, a
    /// `Stop`'s among them, and the requests still waiting end once the stream does, as when
    /// the Runner closes it first.
    async fn stop_writing(&self, link: &Link) {
        link.alive.store(false, Ordering::SeqCst);
        let mut writer = link.writer.lock().await;
        let _ = writer.shutdown().await;
    }

    /// End `link`: no request goes over it again, and those waiting on it end ambiguous. A
    /// client that claims its Runner again does so when `link` is its connection.
    async fn close_link(self: &Arc<Self>, link: &Arc<Link>) {
        link.alive.store(false, Ordering::SeqCst);
        {
            let mut writer = link.writer.lock().await;
            let _ = writer.shutdown().await;
        }
        fail_pending(link, RunnerError::ambiguous("runner socket closed")).await;
        self.start_reclaim(link);
    }

    /// Claim the Runner again once `link`, the client's connection, is lost (P1R-C03): only for
    /// a client set to, and not once the gateway leaves.
    fn start_reclaim(self: &Arc<Self>, link: &Arc<Link>) {
        let Some(reclaim) = self.reclaim.get() else {
            return;
        };
        if self.leaving.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut state = self.state();
            match &*state {
                State::Linked(current) if Arc::ptr_eq(current, link) => *state = State::Reclaiming,
                _ => return,
            }
        }
        (reclaim.observer)(LinkEvent::Lost);
        tokio::spawn(self.clone().reclaim_loop());
    }

    /// Claim the Runner again until it is claimed, it cannot be, or the gateway leaves.
    async fn reclaim_loop(self: Arc<Self>) {
        let Some(reclaim) = self.reclaim.get() else {
            return;
        };
        let mut backoff = RECLAIM_BACKOFF_START;
        loop {
            if self.leaving.load(Ordering::SeqCst) {
                *self.state() = State::Gone("this gateway left the Runner".into());
                return;
            }
            match self
                .link_and_claim(&reclaim.socket, &reclaim.store_id)
                .await
            {
                Ok((link, claim)) => {
                    {
                        let mut state = self.state();
                        if !self.leaving.load(Ordering::SeqCst) {
                            *state = State::Linked(link);
                            drop(state);
                            self.claims.send_replace(Some(claim.clone()));
                            (reclaim.observer)(LinkEvent::Reclaimed(claim));
                            return;
                        }
                        *state = State::Gone("this gateway left the Runner".into());
                    }
                    // The gateway left while the Runner was claimed again: it leaves the Runner
                    // as a detach does.
                    let _ = self
                        .call_on(
                            &link,
                            RunnerOp::Detach,
                            Some(claim.epoch),
                            RUNNER_CALL_DEADLINE,
                        )
                        .await;
                    self.close_link(&link).await;
                    return;
                }
                Err(Attempt::Gone(reason)) => {
                    *self.state() = State::Gone(reason.clone());
                    (reclaim.observer)(LinkEvent::Gone(reason));
                    return;
                }
                Err(Attempt::Retry(reason)) => {
                    (reclaim.observer)(LinkEvent::Retrying {
                        reason,
                        retry: backoff,
                    });
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECLAIM_BACKOFF_MAX);
                }
            }
        }
    }

    /// One try to claim the Runner at `socket` again over a new connection, which is not the
    /// client's until the caller makes it so. Nothing answering at the socket, another Runner
    /// there, or a refusal that waiting cannot end, means the Runner cannot be claimed again.
    async fn link_and_claim(
        self: &Arc<Self>,
        socket: &Path,
        store_id: &str,
    ) -> Result<(Arc<Link>, RunnerClaim), Attempt> {
        let stream = match UnixStream::connect(socket).await {
            Ok(stream) => stream,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(Attempt::Gone(format!(
                    "nothing accepts connections at {}: {err}",
                    socket.display()
                )))
            }
            Err(err) => {
                return Err(Attempt::Retry(format!(
                    "connect {}: {err}",
                    socket.display()
                )))
            }
        };
        let link = self.open_link(stream);
        let claimed = match self.same_runner(&link).await {
            Ok(()) => match self.claim_on(&link, store_id, RUNNER_CALL_DEADLINE).await {
                Ok(claim) => Ok(claim),
                Err(ClaimError::Refused { refusal, message }) if refusal.is_transient() => {
                    Err(Attempt::Retry(message))
                }
                Err(ClaimError::Refused { message, .. }) => Err(Attempt::Gone(message)),
                Err(err) => Err(Attempt::Retry(err.to_string())),
            },
            Err(attempt) => Err(attempt),
        };
        match claimed {
            Ok(claim) => Ok((link, claim)),
            Err(attempt) => {
                self.close_link(&link).await;
                Err(attempt)
            }
        }
    }

    /// The Runner answering over `link` is the one the client's first `Hello` found.
    async fn same_runner(self: &Arc<Self>, link: &Arc<Link>) -> Result<(), Attempt> {
        let expected = self.hello.get().and_then(|hello| hello.identity.clone());
        match self
            .call_on(link, RunnerOp::Hello, None, RUNNER_CALL_DEADLINE)
            .await
        {
            Ok(RunnerOpResult::Hello {
                protocol, identity, ..
            }) if protocol == WIRE_PROTOCOL => {
                if identity.is_some() && identity == expected {
                    Ok(())
                } else {
                    Err(Attempt::Gone(format!(
                        "another Runner answers at its socket (pid {})",
                        identity.map_or_else(|| "unknown".into(), |found| found.pid.to_string())
                    )))
                }
            }
            Ok(other) => Err(Attempt::Gone(format!(
                "the worker at its socket answered Hello with {other:?}"
            ))),
            Err(RunnerError::Execution(body)) => Err(Attempt::Gone(format!(
                "the worker at its socket refused Hello: {}",
                body.message
            ))),
            Err(err) => Err(Attempt::Retry(format!("Hello: {err}"))),
        }
    }

    /// Deliver an exit event once: the Runner sends an event again when its write to a lost
    /// connection failed (P1R-C03).
    fn exited(&self, seq: Option<u64>, process_id: &str) {
        if let Some(seq) = seq {
            if self.last_event.fetch_max(seq, Ordering::SeqCst) >= seq {
                return;
            }
        }
        (self.on_process_exit)(process_id);
    }
}

async fn read_loop(
    mut read: tokio::net::unix::OwnedReadHalf,
    client: Arc<Client>,
    link: Arc<Link>,
) {
    loop {
        match read_frame(&mut read).await {
            Ok(Some(payload)) => match serde_json::from_slice::<WireEnvelope>(&payload) {
                Ok(envelope) => match envelope.kind {
                    WireKind::Event => {
                        if let Some(RunnerEvent::ProcessExited { process_id }) = envelope.event {
                            client.exited(envelope.seq, &process_id.0);
                        }
                    }
                    WireKind::Response => {
                        // The Claim's inventory accounts for every exit numbered up to its
                        // `events_through`, and the controller's events follow its answer
                        // (P1R-C04).
                        if let Some(RunnerOpResult::Claimed(claim)) = &envelope.result {
                            client
                                .last_event
                                .fetch_max(claim.events_through, Ordering::SeqCst);
                        }
                        if let Some(id) = envelope.request_id.clone() {
                            if let Some(tx) = link.pending.lock().await.remove(&id) {
                                let _ = tx.send(Ok(envelope));
                            }
                        }
                    }
                    WireKind::Request => {}
                },
                Err(err) => {
                    fail_pending(
                        &link,
                        RunnerError::ambiguous(format!("invalid runner rpc: {err}")),
                    )
                    .await;
                    break;
                }
            },
            Ok(None) | Err(_) => {
                fail_pending(&link, RunnerError::ambiguous("runner socket closed")).await;
                break;
            }
        }
    }
    link.alive.store(false, Ordering::SeqCst);
    (client.on_disconnect)();
    client.start_reclaim(&link);
}

async fn fail_pending(link: &Link, err: RunnerError) {
    let mut pending = link.pending.lock().await;
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
    async fn read(
        &self,
        ws: &Workspace,
        path: &str,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ReadResult, RunnerError> {
        match self
            .call(RunnerOp::Read {
                workspace: ws.clone(),
                path: path.to_string(),
                offset,
                limit,
            })
            .await?
        {
            RunnerOpResult::Read(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn find(
        &self,
        ws: &Workspace,
        glob: Option<&str>,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<FindResult, RunnerError> {
        match self
            .call(RunnerOp::Find {
                workspace: ws.clone(),
                glob: glob.map(str::to_string),
                offset,
                limit,
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

    async fn resize(
        &self,
        process_id: &ProcessId,
        rows: u16,
        cols: u16,
    ) -> Result<RunnerResizeResult, RunnerError> {
        match self
            .call(RunnerOp::Resize {
                process_id: process_id.clone(),
                rows,
                cols,
            })
            .await?
        {
            RunnerOpResult::Resize(result) => Ok(result),
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

    /// An exit event is delivered once, however often the Runner sends it, and an event the
    /// client already passed is not delivered again; an event without a number is always
    /// delivered (P1R-C03).
    #[tokio::test]
    async fn an_exit_event_is_delivered_once() {
        let (client, server) = UnixStream::pair().unwrap();
        let delivered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook = delivered.clone();
        let runner = UdsRunner::from_stream(
            client,
            Arc::new(move |process_id: &str| hook.lock().unwrap().push(process_id.to_string())),
        );
        let (mut read, mut write) = server.into_split();
        let exited = |process_id: &str| RunnerEvent::ProcessExited {
            process_id: ProcessId(process_id.into()),
        };
        for (seq, process_id) in [(1, "proc-1"), (1, "proc-1"), (3, "proc-3"), (2, "proc-2")] {
            write_frame(&mut write, &WireEnvelope::event(seq, exited(process_id)))
                .await
                .unwrap();
        }
        let mut unnumbered = WireEnvelope::event(0, exited("proc-4"));
        unnumbered.seq = None;
        write_frame(&mut write, &unnumbered).await.unwrap();
        // The client reads its frames in order: once a request is answered, each event sent
        // before the answer was handled.
        let answering = tokio::spawn(async move {
            let frame = read_frame(&mut read).await.unwrap().unwrap();
            let request: WireEnvelope = serde_json::from_slice(&frame).unwrap();
            write_frame(
                &mut write,
                &WireEnvelope::response(request.request_id.unwrap(), Ok(RunnerOpResult::Terminate)),
            )
            .await
            .unwrap();
            (read, write)
        });
        runner.terminate(&ProcessId("proc-5".into())).await.unwrap();
        let _ = answering.await.unwrap();
        assert_eq!(*delivered.lock().unwrap(), ["proc-1", "proc-3", "proc-4"]);
    }

    /// The exits that a Claim's inventory accounts for, numbered up to its `events_through`,
    /// are not delivered after its answer; later ones are, and the Claim is told to whoever
    /// watches the client's Claims (P1R-C04).
    #[tokio::test]
    async fn an_independent_claim_accounts_for_the_exits_before_it() {
        let (client, server) = UnixStream::pair().unwrap();
        let delivered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook = delivered.clone();
        let runner = UdsRunner::from_stream(
            client,
            Arc::new(move |process_id: &str| hook.lock().unwrap().push(process_id.to_string())),
        );
        let mut claims = runner.claims();
        assert!(claims.borrow().is_none());
        let (mut read, mut write) = server.into_split();
        let exited = |process_id: &str| RunnerEvent::ProcessExited {
            process_id: ProcessId(process_id.into()),
        };
        let answering = tokio::spawn(async move {
            let frame = read_frame(&mut read).await.unwrap().unwrap();
            let request: WireEnvelope = serde_json::from_slice(&frame).unwrap();
            assert!(matches!(request.op, Some(RunnerOp::Claim { .. })));
            let claim = RunnerClaim {
                epoch: 4,
                previous_controller: true,
                barrier: vec!["demo".into()],
                processes: Vec::new(),
                events_through: 3,
            };
            write_frame(
                &mut write,
                &WireEnvelope::response(
                    request.request_id.unwrap(),
                    Ok(RunnerOpResult::Claimed(claim)),
                ),
            )
            .await
            .unwrap();
            for (seq, process_id) in [(2, "proc-2"), (3, "proc-3"), (4, "proc-4")] {
                write_frame(&mut write, &WireEnvelope::event(seq, exited(process_id)))
                    .await
                    .unwrap();
            }
            let frame = read_frame(&mut read).await.unwrap().unwrap();
            let request: WireEnvelope = serde_json::from_slice(&frame).unwrap();
            assert_eq!(request.epoch, Some(4));
            write_frame(
                &mut write,
                &WireEnvelope::response(request.request_id.unwrap(), Ok(RunnerOpResult::Terminate)),
            )
            .await
            .unwrap();
            (read, write)
        });
        let claim = runner.claim("store-test").await.unwrap();
        assert_eq!(claim.barrier, ["demo"]);
        claims.changed().await.unwrap();
        assert_eq!(claims.borrow().as_ref(), Some(&claim));
        runner.terminate(&ProcessId("proc-5".into())).await.unwrap();
        let _ = answering.await.unwrap();
        assert_eq!(*delivered.lock().unwrap(), ["proc-4"]);
    }

    /// A Runner that answers a request and then closes the connection, as one that stops does,
    /// fails the write of a request sent after that, but the answer it wrote before it closed
    /// is still read (P1R-C04): a late request does not cost a `Stop` its answer.
    #[tokio::test]
    async fn an_answer_written_before_the_runner_closed_outlives_a_later_failed_write() {
        let (client, server) = UnixStream::pair().unwrap();
        let runner = UdsRunner::from_stream(client, Arc::new(|_: &str| {}));
        let (mut read, mut write) = server.into_split();
        let first = tokio::spawn({
            let runner = runner.clone();
            async move { runner.terminate(&ProcessId("proc-1".into())).await }
        });
        let frame = read_frame(&mut read).await.unwrap().unwrap();
        let request: WireEnvelope = serde_json::from_slice(&frame).unwrap();
        write_frame(
            &mut write,
            &WireEnvelope::response(request.request_id.unwrap(), Ok(RunnerOpResult::Terminate)),
        )
        .await
        .unwrap();
        drop((read, write));
        // This test's runtime runs one task at a time: the client has not read the answer yet.
        let late = runner.terminate(&ProcessId("proc-2".into())).await;
        assert!(
            matches!(late, Err(RunnerError::TransportAmbiguous { .. })),
            "{late:?}"
        );
        first.await.unwrap().unwrap();
        assert!(matches!(
            runner.terminate(&ProcessId("proc-3".into())).await,
            Err(RunnerError::TransportBeforeDispatch { .. })
        ));
    }
}
