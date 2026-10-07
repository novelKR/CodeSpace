//! Preparation of a governed execution by its owner (CSRG-U3).
//!
//! A workspace whose `resources.participation` is `required` runs nothing that the resource
//! authority has not admitted. The execution owner, which holds the process slots and the
//! registered identity, prepares each execution in this order:
//!
//! 1. a CodeSpace process slot, taken before anything is created ([`InProcessRunner::reserve_slot`]);
//! 2. the attempt's identity and the digest of its meaning;
//! 3. DevGuard's admission of that attempt, in a bounded session of the registered owner;
//! 4. a one-shot [`PreparedExecution`], which owns the meaning, the slot and the attempt.
//!
//! Admission state is never a process handle: nothing here creates a process. CodeSpace keeps
//! authorization, workspace policy, process slots and handles, PTY selection, output, timeout,
//! termination and reaping; DevGuard keeps admission, reservations and the attempt's durable
//! identity and accounting.
//!
//! Managed launch (`BeginLaunch`, its permit and carrier, the helper) is U4's. Until it exists
//! [`PreparedExecution::launch`] cannot launch: it cancels the unstarted attempt and answers
//! [`PreparationOutcome::LaunchUnavailable`]. Nothing is ever run outside the authority instead.
//!
//! Uncertainty stays conservative. A timeout, a lost reply or an EOF is not taken as a refusal
//! or as proof that no attempt exists: the attempt is looked up once, cancelled when the lookup
//! shows it prepared, and otherwise reported as unknown. An uncertain attempt is never
//! admitted again and never rebuilt into a [`PreparedExecution`]; DevGuard cancels nothing on
//! CodeSpace's behalf because a local task ended, and CodeSpace does not claim a release it did
//! not observe.
//!
//! Cancelling is the one settlement used, and only where its precondition holds: this owner
//! never asked DevGuard to launch the attempt, so a cancellation finds it prepared or settled.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codespace_devguard as devguard;
use codespace_devguard::admission::{self as dg, Admission, Answer, Meaning};
use codespace_domain::{
    ErrorBody, ErrorCode, ProcessId, ResourceAuthorityErrorCode, ResourceRegistrationState,
};
use codespace_policy::{EnforcementLevel, Participation, ResourceRequest, Workspace};
use serde::{Deserialize, Serialize};

use crate::process::{spawn_env, SlotReservation};
use crate::registration::{error_code, registration_state};
use crate::{InProcessRunner, RunnerCwd, RunnerExecRequest};

/// The authority that admits, finds and cancels an owner's attempts: DevGuard's, through the
/// registered owner. Each call is one bounded, blocking session.
pub trait AttemptAuthority: Send + Sync + 'static {
    fn consumer(&self) -> &str;
    fn generation(&self) -> &str;
    fn admit(&self, admission: &Admission) -> Answer;
    fn lookup(&self, admission: &Admission) -> Answer;
    fn cancel(&self, admission: &Admission) -> Answer;
}

impl AttemptAuthority for devguard::Owner {
    fn consumer(&self) -> &str {
        devguard::Owner::consumer(self)
    }
    fn generation(&self) -> &str {
        devguard::Owner::generation(self)
    }
    fn admit(&self, admission: &Admission) -> Answer {
        devguard::Owner::admit(self, admission)
    }
    fn lookup(&self, admission: &Admission) -> Answer {
        devguard::Owner::lookup(self, admission)
    }
    fn cancel(&self, admission: &Admission) -> Answer {
        devguard::Owner::cancel(self, admission)
    }
}

/// The identity of one attempt, which both CodeSpace and DevGuard hold: the consumer and
/// generation it is admitted under, its attempt ID (minted for it, never a transport request
/// ID), the CodeSpace process ID it would become, the workspace, whether it has a PTY, what it
/// asks for, and the digest of everything it means. A prepared attempt is bound to this
/// identity and to nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptIdentity {
    pub consumer: String,
    pub generation: String,
    pub attempt_id: String,
    pub process_id: ProcessId,
    pub workspace_id: String,
    pub tty: bool,
    pub resources: ResourceRequest,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quantities {
    pub cpu_milli: u64,
    pub memory_bytes: u64,
    pub tasks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Levels {
    pub cpu: EnforcementLevel,
    pub memory: EnforcementLevel,
    pub pids: EnforcementLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMethod {
    Accounting,
    QosAndPriority,
    CgroupV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    ObservedProcessGroup,
    ContainedCgroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    pub level: EnforcementLevel,
    pub method: ControlMethod,
}

/// How the authority would control the execution: what it supports for this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supported {
    pub scope: ScopeKind,
    pub cpu: Control,
    pub memory: Control,
    pub pids: Control,
}

/// What the authority holds for the execution, and for how long it holds it unlaunched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reserved {
    pub quantities: Quantities,
    pub prepared_ttl_ms: u64,
}

/// Whether resources were applied to a launched execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applied {
    /// Nothing was launched, so nothing was applied. Always so before managed launch (U4).
    NotLaunched,
}

/// The four stages of an admission, kept apart: what was requested and the weakest control
/// required, what the authority supports for it, what it reserved, and what was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionAccount {
    pub requested: Quantities,
    pub required: Levels,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported: Option<Supported>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved: Option<Reserved>,
    pub applied: Applied,
}

/// What is known of the authority's attempt when the preparation ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// The authority was never asked: no attempt exists.
    NotAsked,
    /// The authority answered the admission with a refusal: it recorded no attempt.
    NotRecorded,
    /// Denied: nothing was reserved.
    Denied,
    /// Cancelled before launch: its reservation is returned.
    Cancelled,
    /// Expired unlaunched: its reservation is returned.
    Expired,
    /// The authority holds it in another phase.
    Other,
    /// Not known: it may be prepared, holding its reservation until the authority expires it.
    /// Nothing was started for it.
    Unknown,
}

/// How a preparation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PreparationOutcome {
    /// Admitted and prepared; managed launch is not available yet, so the unstarted attempt
    /// was cancelled.
    LaunchUnavailable,
    /// No CodeSpace process slot was free; the authority was not asked.
    SlotUnavailable,
    /// The authority denied it: `resource_unavailable` for a shortage,
    /// `resource_policy_unsupported` when it cannot give the control required, or
    /// `resource_control_unavailable`.
    Denied { denial: ResourceAuthorityErrorCode },
    /// The owner's session ended before the admission was asked.
    AuthorityUnavailable {
        registration: ResourceRegistrationState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<ResourceAuthorityErrorCode>,
    },
    /// The authority refused the admission request.
    Refused {
        error_code: ResourceAuthorityErrorCode,
    },
    /// The execution has no meaning the authority can record: an empty command, a zero
    /// timeout or a zero quantity.
    Invalid,
    /// The prepared attempt's deadline passed before launch.
    Expired,
    /// The launch presented another command than the prepared one.
    DigestMismatch,
    /// Whether the authority admitted it is not known.
    Unknown,
}

/// The owner's report of one preparation. Nothing was started for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedPreparation {
    pub attempt_id: String,
    pub process_id: ProcessId,
    pub outcome: PreparationOutcome,
    pub attempt: AttemptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AdmissionAccount>,
}

impl ManagedPreparation {
    /// The tool error for this preparation. Every preparation ends in one, as none starts
    /// anything (CSRG-U3). Only enumerations and identifiers are stated.
    pub fn into_error_body(self, workspace_id: &str) -> ErrorBody {
        let attempt = format!(
            "attempt `{}` for process_id `{}`: {}; nothing was started",
            self.attempt_id,
            self.process_id.0,
            wire_name(&self.attempt)
        );
        let (code, what) = match self.outcome {
            PreparationOutcome::LaunchUnavailable => (
                ErrorCode::ManagedLaunchUnavailable,
                "the resource authority admitted it, but managed launch is not available yet, \
                 so the unstarted attempt was cancelled"
                    .to_string(),
            ),
            PreparationOutcome::SlotUnavailable => (
                ErrorCode::WorkspaceBusy,
                "live process limit reached; the resource authority was not asked".to_string(),
            ),
            PreparationOutcome::Denied { denial } => (
                match denial {
                    ResourceAuthorityErrorCode::ResourceUnavailable => {
                        ErrorCode::ResourceUnavailable
                    }
                    ResourceAuthorityErrorCode::ResourcePolicyUnsupported => {
                        ErrorCode::ResourcePolicyUnsupported
                    }
                    _ => ErrorCode::ResourceAuthorityUnavailable,
                },
                format!("the resource authority denied it ({})", wire_name(&denial)),
            ),
            PreparationOutcome::AuthorityUnavailable {
                registration,
                error_code,
            } => (
                ErrorCode::ResourceAuthorityUnavailable,
                format!(
                    "the execution owner could not ask the resource authority (registration: {}{})",
                    wire_name(&registration),
                    error_code
                        .map(|code| format!(", {}", wire_name(&code)))
                        .unwrap_or_default()
                ),
            ),
            PreparationOutcome::Refused { error_code } => (
                ErrorCode::ResourceAuthorityUnavailable,
                format!(
                    "the resource authority refused the admission ({})",
                    wire_name(&error_code)
                ),
            ),
            PreparationOutcome::Invalid => (
                ErrorCode::InvalidCommand,
                "the execution has no meaning the resource authority can admit".to_string(),
            ),
            PreparationOutcome::Expired => (
                ErrorCode::ResourceUnavailable,
                "the admission expired before launch".to_string(),
            ),
            PreparationOutcome::DigestMismatch => (
                ErrorCode::Internal,
                "the launch did not match the prepared execution".to_string(),
            ),
            PreparationOutcome::Unknown => (
                ErrorCode::AdmissionUnknown,
                "no answer came back from the resource authority, so whether it admitted the \
                 execution is not known"
                    .to_string(),
            ),
        };
        ErrorBody::new(
            code,
            format!(
                "workspace `{workspace_id}` requires resource participation: {what}; {attempt}"
            ),
        )
    }
}

fn wire_name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|name| name.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Prepares governed executions for the owner that holds both the process slots and the
/// registered identity.
#[derive(Clone)]
pub struct Preparer {
    authority: Arc<dyn AttemptAuthority>,
    runner: InProcessRunner,
    ttl_bound: Duration,
}

impl Preparer {
    pub fn new(authority: Arc<dyn AttemptAuthority>, runner: InProcessRunner) -> Self {
        Self {
            authority,
            runner,
            ttl_bound: Duration::from_millis(devguard_prepared_ttl()),
        }
    }

    /// Hold prepared attempts for at most `bound` here, whatever the authority grants.
    pub fn with_ttl_bound(mut self, bound: Duration) -> Self {
        self.ttl_bound = bound;
        self
    }

    /// Prepare the execution `req` in `ws` as attempt `attempt_id`, or report why not. Only a
    /// workspace that requires resource participation is prepared.
    pub async fn prepare(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        attempt_id: String,
    ) -> Result<PreparedExecution, Box<ManagedPreparation>> {
        let report = |outcome, attempt, account| {
            Box::new(ManagedPreparation {
                attempt_id: attempt_id.clone(),
                process_id: req.process_id.clone(),
                outcome,
                attempt,
                account,
            })
        };
        if ws.resources.participation != Participation::Required {
            return Err(report(
                PreparationOutcome::Invalid,
                AttemptState::NotAsked,
                None,
            ));
        }
        // The slot first: no admission is asked without one.
        let slot = match self.runner.reserve_slot(&req.process_id.0) {
            Ok(slot) => slot,
            Err(_) => {
                return Err(report(
                    PreparationOutcome::SlotUnavailable,
                    AttemptState::NotAsked,
                    None,
                ))
            }
        };
        let resources = ws.resources.request();
        let Some(digest) = meaning(ws, &req).digest() else {
            return Err(report(
                PreparationOutcome::Invalid,
                AttemptState::NotAsked,
                None,
            ));
        };
        let identity = AttemptIdentity {
            consumer: self.authority.consumer().to_owned(),
            generation: self.authority.generation().to_owned(),
            attempt_id: attempt_id.clone(),
            process_id: req.process_id.clone(),
            workspace_id: ws.id.0.clone(),
            tty: req.tty,
            resources,
            digest: digest.clone(),
        };
        let admission = Admission {
            attempt_id: attempt_id.clone(),
            digest,
            resources: dg_request(&resources),
        };
        let mut account = AdmissionAccount {
            requested: requested(&resources),
            required: required(&resources),
            supported: None,
            reserved: None,
            applied: Applied::NotLaunched,
        };
        let started = Instant::now();
        match call(&self.authority, &admission, Call::Admit).await {
            Answer::Attempt(attempt) => {
                account.supported = attempt.plan.map(supported);
                account.reserved = attempt.reservation.map(reserved);
                match attempt.phase {
                    dg::Phase::Prepared => {
                        let ttl = attempt
                            .reservation
                            .map(|reservation| Duration::from_millis(reservation.prepared_ttl_ms))
                            .unwrap_or_default()
                            .min(self.ttl_bound);
                        Ok(PreparedExecution {
                            identity,
                            ws: ws.clone(),
                            request: req,
                            account,
                            deadline: started + ttl,
                            admission,
                            authority: self.authority.clone(),
                            slot: Some(slot),
                            settled: false,
                        })
                    }
                    dg::Phase::Denied => Err(report(
                        PreparationOutcome::Denied {
                            denial: attempt
                                .denial
                                .map(error_code)
                                .unwrap_or(ResourceAuthorityErrorCode::ResourcePolicyUnsupported),
                        },
                        AttemptState::Denied,
                        Some(account),
                    )),
                    // A fresh attempt is prepared or denied. Anything else is not understood:
                    // settle it as far as its precondition allows and report it unknown.
                    _ => {
                        let attempt = resolve(&self.authority, &admission).await;
                        Err(report(PreparationOutcome::Unknown, attempt, Some(account)))
                    }
                }
            }
            Answer::Refused(code) => Err(report(
                PreparationOutcome::Refused {
                    error_code: error_code(code),
                },
                AttemptState::NotRecorded,
                Some(account),
            )),
            Answer::NotSent(registration) => Err(report(
                PreparationOutcome::AuthorityUnavailable {
                    registration: registration_state(registration.state),
                    error_code: registration.error_code.map(error_code),
                },
                AttemptState::NotAsked,
                Some(account),
            )),
            Answer::Unknown => {
                let attempt = resolve(&self.authority, &admission).await;
                Err(report(PreparationOutcome::Unknown, attempt, Some(account)))
            }
        }
    }

    /// U3's whole path for a governed execution: prepare it, then present it for launch, which
    /// cannot launch yet and cancels it. It runs as its own task, so a caller that stops
    /// waiting leaves the preparation to settle rather than abandoning a prepared attempt.
    pub async fn prepare_and_dispose(
        &self,
        ws: Workspace,
        req: RunnerExecRequest,
        attempt_id: String,
    ) -> ManagedPreparation {
        let preparer = self.clone();
        let (attempt, process_id) = (attempt_id.clone(), req.process_id.clone());
        let task = tokio::spawn(async move {
            let launch = req.clone();
            match preparer.prepare(&ws, req, attempt_id).await {
                Ok(prepared) => prepared.launch(&launch).await,
                Err(report) => *report,
            }
        });
        task.await.unwrap_or(ManagedPreparation {
            attempt_id: attempt,
            process_id,
            outcome: PreparationOutcome::Unknown,
            attempt: AttemptState::Unknown,
            account: None,
        })
    }
}

/// A prepared, admitted execution that has not started: one attempt, bound to its meaning,
/// holding its CodeSpace slot and its authority reservation until it is launched, cancelled
/// or dropped. It cannot be cloned, and launching or cancelling consumes it, so it is used
/// once. Its deadline is the authority's own, measured from before the admission was asked,
/// so it never outlives the reservation here.
///
/// Dropping it unsettled cancels the attempt in the background; a local task ending is not
/// taken as the attempt's release.
pub struct PreparedExecution {
    identity: AttemptIdentity,
    ws: Workspace,
    request: RunnerExecRequest,
    account: AdmissionAccount,
    deadline: Instant,
    admission: Admission,
    authority: Arc<dyn AttemptAuthority>,
    slot: Option<SlotReservation>,
    settled: bool,
}

impl std::fmt::Debug for PreparedExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedExecution")
            .field("identity", &self.identity)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl PreparedExecution {
    pub fn identity(&self) -> &AttemptIdentity {
        &self.identity
    }

    pub fn account(&self) -> &AdmissionAccount {
        &self.account
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// The command it was prepared for.
    pub fn request(&self) -> &RunnerExecRequest {
        &self.request
    }

    /// Present `request` for launch. Only the prepared command, before its deadline, could be
    /// launched; managed launch is not available yet (U4), so the unstarted attempt is
    /// cancelled in every case and nothing is started.
    pub async fn launch(mut self, request: &RunnerExecRequest) -> ManagedPreparation {
        let outcome = if meaning(&self.ws, request).digest().as_deref()
            != Some(self.identity.digest.as_str())
            || request.process_id != self.identity.process_id
        {
            PreparationOutcome::DigestMismatch
        } else if Instant::now() >= self.deadline {
            PreparationOutcome::Expired
        } else {
            PreparationOutcome::LaunchUnavailable
        };
        let attempt = self.settle().await;
        self.report(outcome, attempt)
    }

    /// Cancel the unstarted attempt and free its slot.
    pub async fn cancel(mut self) -> AttemptState {
        self.settle().await
    }

    async fn settle(&mut self) -> AttemptState {
        self.settled = true;
        let attempt = cancelled(call(&self.authority, &self.admission, Call::Cancel).await);
        self.slot = None;
        attempt
    }

    fn report(&self, outcome: PreparationOutcome, attempt: AttemptState) -> ManagedPreparation {
        ManagedPreparation {
            attempt_id: self.identity.attempt_id.clone(),
            process_id: self.identity.process_id.clone(),
            outcome,
            attempt,
            account: Some(self.account),
        }
    }
}

impl Drop for PreparedExecution {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Prepared and never launched: cancelling is sound. Without a runtime to run the
        // session on, the attempt is left to expire at its deadline.
        let (authority, admission) = (self.authority.clone(), self.admission.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || authority.cancel(&admission));
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Call {
    Admit,
    Lookup,
    Cancel,
}

/// One blocking session on its own thread. A thread that cannot finish is unknown.
async fn call(authority: &Arc<dyn AttemptAuthority>, admission: &Admission, call: Call) -> Answer {
    let (authority, admission) = (authority.clone(), admission.clone());
    tokio::task::spawn_blocking(move || match call {
        Call::Admit => authority.admit(&admission),
        Call::Lookup => authority.lookup(&admission),
        Call::Cancel => authority.cancel(&admission),
    })
    .await
    .unwrap_or(Answer::Unknown)
}

/// After an unknown answer: look the attempt up once, and cancel it when it is prepared.
async fn resolve(authority: &Arc<dyn AttemptAuthority>, admission: &Admission) -> AttemptState {
    match call(authority, admission, Call::Lookup).await {
        Answer::Attempt(attempt) if attempt.phase == dg::Phase::Prepared => {
            cancelled(call(authority, admission, Call::Cancel).await)
        }
        Answer::Attempt(attempt) => state(attempt.phase),
        // Not found is not proof: the admission may still be on its way.
        Answer::Refused(_) | Answer::NotSent(_) | Answer::Unknown => AttemptState::Unknown,
    }
}

fn cancelled(answer: Answer) -> AttemptState {
    match answer {
        Answer::Attempt(attempt) => state(attempt.phase),
        Answer::Refused(_) | Answer::NotSent(_) | Answer::Unknown => AttemptState::Unknown,
    }
}

fn state(phase: dg::Phase) -> AttemptState {
    match phase {
        dg::Phase::Denied => AttemptState::Denied,
        dg::Phase::Cancelled => AttemptState::Cancelled,
        dg::Phase::Expired => AttemptState::Expired,
        // Prepared after a cancellation, or any launched phase, is not what this owner asked for.
        _ => AttemptState::Other,
    }
}

/// What the execution means, as its owner would run it: the command as given, the workspace it
/// runs in with its profile and network policy, the exact environment its child would get, its
/// PTY, its timeout and its resource request. The CodeSpace process ID is part of the
/// attempt's identity, not of its meaning.
pub(crate) fn meaning(ws: &Workspace, req: &RunnerExecRequest) -> Meaning {
    let cwd = match req.cwd {
        RunnerCwd::WorkspaceRoot => ws.root.clone(),
    };
    let environment: BTreeMap<String, String> =
        spawn_env(&cwd, req, crate::linux_sandbox_available())
            .into_iter()
            .collect();
    Meaning {
        executable: req.argv.first().cloned().unwrap_or_default(),
        cwd: format!(
            "codespace-workspace:{}:{}:profile={}:network={}",
            ws.id.0,
            cwd.display(),
            wire_name(&req.policy.workspace_profile),
            wire_name(&req.policy.network),
        ),
        argv: req.argv.clone(),
        environment,
        tty: req.tty,
        timeout_ms: req.timeout_ms,
        resources: dg_request(&ws.resources.request()),
    }
}

/// DevGuard's own prepared lifetime, an upper bound for every preparation here.
fn devguard_prepared_ttl() -> u64 {
    devguard::admission::PREPARED_TTL_MS
}

fn dg_level(level: EnforcementLevel) -> dg::Level {
    match level {
        EnforcementLevel::Accounted => dg::Level::Accounted,
        EnforcementLevel::Cooperative => dg::Level::Cooperative,
        EnforcementLevel::Kernel => dg::Level::Kernel,
    }
}

fn level(level: dg::Level) -> EnforcementLevel {
    match level {
        dg::Level::Accounted => EnforcementLevel::Accounted,
        dg::Level::Cooperative => EnforcementLevel::Cooperative,
        dg::Level::Kernel => EnforcementLevel::Kernel,
    }
}

fn dg_request(request: &ResourceRequest) -> dg::ResourceRequest {
    dg::ResourceRequest {
        requested: dg::Quantities {
            cpu_milli: request.cpu_milli,
            memory_bytes: request.memory_bytes,
            tasks: request.tasks,
        },
        minimum: dg::Levels {
            cpu: dg_level(request.minimum.cpu),
            memory: dg_level(request.minimum.memory),
            pids: dg_level(request.minimum.pids),
        },
    }
}

fn requested(request: &ResourceRequest) -> Quantities {
    Quantities {
        cpu_milli: request.cpu_milli,
        memory_bytes: request.memory_bytes,
        tasks: request.tasks,
    }
}

fn required(request: &ResourceRequest) -> Levels {
    Levels {
        cpu: request.minimum.cpu,
        memory: request.minimum.memory,
        pids: request.minimum.pids,
    }
}

fn control(planned: dg::Planned) -> Control {
    Control {
        level: level(planned.level),
        method: match planned.method {
            dg::Method::Accounting => ControlMethod::Accounting,
            dg::Method::QosAndPriority => ControlMethod::QosAndPriority,
            dg::Method::CgroupV2 => ControlMethod::CgroupV2,
        },
    }
}

fn supported(plan: dg::Plan) -> Supported {
    Supported {
        scope: match plan.scope {
            dg::ScopeKind::ObservedProcessGroup => ScopeKind::ObservedProcessGroup,
            dg::ScopeKind::ContainedCgroup => ScopeKind::ContainedCgroup,
        },
        cpu: control(plan.cpu),
        memory: control(plan.memory),
        pids: control(plan.pids),
    }
}

fn reserved(reservation: dg::Reservation) -> Reserved {
    Reserved {
        quantities: Quantities {
            cpu_milli: reservation.quantities.cpu_milli,
            memory_bytes: reservation.quantities.memory_bytes,
            tasks: reservation.quantities.tasks,
        },
        prepared_ttl_ms: reservation.prepared_ttl_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{Profile, WorkspaceId};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// An authority whose answers the test sets, counting each call.
    #[derive(Default)]
    struct Fake {
        admits: Mutex<VecDeque<Answer>>,
        lookups: Mutex<VecDeque<Answer>>,
        cancels: Mutex<VecDeque<Answer>>,
        admit_delay: Mutex<Duration>,
        calls: [AtomicUsize; 3],
        seen: Mutex<Vec<Admission>>,
    }

    impl Fake {
        fn admitting(answers: impl IntoIterator<Item = Answer>) -> Arc<Self> {
            let fake = Self::default();
            fake.admits.lock().unwrap().extend(answers);
            Arc::new(fake)
        }
        fn then_lookup(self: Arc<Self>, answer: Answer) -> Arc<Self> {
            self.lookups.lock().unwrap().push_back(answer);
            self
        }
        fn then_cancel(self: Arc<Self>, answer: Answer) -> Arc<Self> {
            self.cancels.lock().unwrap().push_back(answer);
            self
        }
        fn slow(self: Arc<Self>, delay: Duration) -> Arc<Self> {
            *self.admit_delay.lock().unwrap() = delay;
            self
        }
        fn count(&self, call: usize) -> usize {
            self.calls[call].load(Ordering::SeqCst)
        }
        fn next(queue: &Mutex<VecDeque<Answer>>, default: Answer) -> Answer {
            queue.lock().unwrap().pop_front().unwrap_or(default)
        }
    }

    const ADMIT: usize = 0;
    const LOOKUP: usize = 1;
    const CANCEL: usize = 2;

    impl AttemptAuthority for Fake {
        fn consumer(&self) -> &str {
            "codespace"
        }
        fn generation(&self) -> &str {
            "g1"
        }
        fn admit(&self, admission: &Admission) -> Answer {
            self.calls[ADMIT].fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(admission.clone());
            std::thread::sleep(*self.admit_delay.lock().unwrap());
            Self::next(&self.admits, prepared())
        }
        fn lookup(&self, admission: &Admission) -> Answer {
            self.calls[LOOKUP].fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(admission.clone());
            Self::next(&self.lookups, Answer::Unknown)
        }
        fn cancel(&self, admission: &Admission) -> Answer {
            self.calls[CANCEL].fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(admission.clone());
            Self::next(&self.cancels, attempt(dg::Phase::Cancelled, None))
        }
    }

    fn attempt(phase: dg::Phase, denial: Option<devguard::ErrorCode>) -> Answer {
        let prepared = phase == dg::Phase::Prepared;
        Answer::Attempt(dg::Attempt {
            phase,
            denial,
            plan: prepared.then_some(dg::Plan {
                scope: dg::ScopeKind::ObservedProcessGroup,
                cpu: dg::Planned {
                    level: dg::Level::Cooperative,
                    method: dg::Method::QosAndPriority,
                },
                memory: dg::Planned {
                    level: dg::Level::Accounted,
                    method: dg::Method::Accounting,
                },
                pids: dg::Planned {
                    level: dg::Level::Accounted,
                    method: dg::Method::Accounting,
                },
            }),
            reservation: prepared.then_some(dg::Reservation {
                quantities: dg::Quantities {
                    cpu_milli: 1_000,
                    memory_bytes: 512 << 20,
                    tasks: 64,
                },
                prepared_ttl_ms: 5_000,
            }),
            applied: false,
            known_not_started: !prepared,
        })
    }

    fn prepared() -> Answer {
        attempt(dg::Phase::Prepared, None)
    }

    fn governed(root: &std::path::Path) -> Workspace {
        let mut ws = Workspace::new(
            WorkspaceId("gov".into()),
            root.to_path_buf(),
            Profile::WorkspaceWrite,
        );
        ws.resources.participation = Participation::Required;
        ws
    }

    fn request(argv: &[&str], process: &str) -> RunnerExecRequest {
        RunnerExecRequest::for_host(
            argv.iter().map(|part| part.to_string()).collect(),
            ProcessId(process.into()),
            Profile::WorkspaceWrite,
        )
    }

    fn runner() -> InProcessRunner {
        InProcessRunner::new(Arc::new(|_| {}))
    }

    /// A command that would leave a mark if it ever ran.
    fn touch(root: &std::path::Path, process: &str) -> RunnerExecRequest {
        request(
            &[
                "/usr/bin/touch",
                &root.join("started").display().to_string(),
            ],
            process,
        )
    }

    fn assert_nothing_started(root: &std::path::Path, runner: &InProcessRunner) {
        assert!(!root.join("started").exists(), "a child ran");
        assert_eq!(runner.occupied_slots(), 0, "a slot is still held");
    }

    #[tokio::test]
    async fn a_prepared_execution_holds_its_identity_slot_and_account_and_launches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()]);
        let preparer = Preparer::new(fake.clone(), runner.clone());
        let req = touch(dir.path(), "proc-1");
        let prepared = preparer
            .prepare(&ws, req.clone(), "cs-attempt-1".into())
            .await
            .unwrap();
        let identity = prepared.identity().clone();
        assert_eq!(
            (
                identity.consumer.as_str(),
                identity.generation.as_str(),
                identity.attempt_id.as_str(),
                identity.process_id.0.as_str(),
                identity.workspace_id.as_str(),
                identity.tty,
            ),
            ("codespace", "g1", "cs-attempt-1", "proc-1", "gov", false)
        );
        assert_eq!(identity.resources, ResourceRequest::default());
        assert_eq!(identity.digest, meaning(&ws, &req).digest().unwrap());
        // The authority was asked for this attempt, under this digest and request.
        let asked = fake.seen.lock().unwrap()[0].clone();
        assert_eq!(asked.attempt_id, "cs-attempt-1");
        assert_eq!(asked.digest, identity.digest);
        assert_eq!(asked.resources, dg_request(&ResourceRequest::default()));
        // Requested, required, supported, reserved and applied stay apart.
        let account = *prepared.account();
        assert_eq!(account.requested, requested(&ResourceRequest::default()));
        assert_eq!(account.required.memory, EnforcementLevel::Accounted);
        assert_eq!(
            account.supported.unwrap().cpu.method,
            ControlMethod::QosAndPriority
        );
        assert_eq!(account.reserved.unwrap().quantities.tasks, 64);
        assert_eq!(account.applied, Applied::NotLaunched);
        // It holds a slot, which is not a process.
        assert_eq!(runner.occupied_slots(), 1);
        assert!(runner.host_workspace_of("proc-1").is_none());
        assert!(prepared.deadline() > Instant::now());
        let report = prepared.launch(&req).await;
        assert_eq!(report.outcome, PreparationOutcome::LaunchUnavailable);
        assert_eq!(report.attempt, AttemptState::Cancelled);
        assert_eq!(report.account, Some(account));
        assert_eq!((fake.count(ADMIT), fake.count(CANCEL)), (1, 1));
        assert_nothing_started(dir.path(), &runner);
        let body = report.into_error_body("gov");
        assert_eq!(body.code, ErrorCode::ManagedLaunchUnavailable);
        assert!(
            body.message.ends_with(
                "attempt `cs-attempt-1` for process_id `proc-1`: cancelled; nothing was started"
            ),
            "{}",
            body.message
        );
    }

    #[tokio::test]
    async fn denials_and_an_unreachable_authority_reserve_nothing_and_cancel_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let unavailable = devguard::Registration {
            status: devguard::Status {
                state: devguard::State::Unavailable,
                error_code: Some(devguard::ErrorCode::ResourceControlUnavailable),
                report: None,
            },
            state: devguard::RegistrationState::Unavailable,
            error_code: Some(devguard::ErrorCode::ResourceControlUnavailable),
            pid: None,
        };
        for (answer, outcome, attempt, code) in [
            (
                attempt(
                    dg::Phase::Denied,
                    Some(devguard::ErrorCode::ResourceUnavailable),
                ),
                PreparationOutcome::Denied {
                    denial: ResourceAuthorityErrorCode::ResourceUnavailable,
                },
                AttemptState::Denied,
                ErrorCode::ResourceUnavailable,
            ),
            (
                attempt(
                    dg::Phase::Denied,
                    Some(devguard::ErrorCode::ResourcePolicyUnsupported),
                ),
                PreparationOutcome::Denied {
                    denial: ResourceAuthorityErrorCode::ResourcePolicyUnsupported,
                },
                AttemptState::Denied,
                ErrorCode::ResourcePolicyUnsupported,
            ),
            (
                Answer::NotSent(unavailable.clone()),
                PreparationOutcome::AuthorityUnavailable {
                    registration: ResourceRegistrationState::Unavailable,
                    error_code: Some(ResourceAuthorityErrorCode::ResourceControlUnavailable),
                },
                AttemptState::NotAsked,
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            (
                Answer::Refused(devguard::ErrorCode::ResourceUnavailable),
                PreparationOutcome::Refused {
                    error_code: ResourceAuthorityErrorCode::ResourceUnavailable,
                },
                AttemptState::NotRecorded,
                ErrorCode::ResourceAuthorityUnavailable,
            ),
        ] {
            let fake = Fake::admitting([answer]);
            let report = Preparer::new(fake.clone(), runner.clone())
                .prepare_and_dispose(ws.clone(), touch(dir.path(), "proc-d"), "cs-d".into())
                .await;
            assert_eq!((report.outcome, report.attempt), (outcome, attempt));
            assert_eq!(report.account.unwrap().reserved, None);
            assert_eq!(
                (fake.count(ADMIT), fake.count(LOOKUP), fake.count(CANCEL)),
                (1, 0, 0)
            );
            assert_eq!(report.into_error_body("gov").code, code);
            assert_nothing_started(dir.path(), &runner);
        }
    }

    #[tokio::test]
    async fn an_occupied_slot_refuses_before_the_authority_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner().with_max_processes(1);
        let held = runner.reserve_slot("proc-held").unwrap();
        let fake = Fake::admitting([]);
        let report = Preparer::new(fake.clone(), runner.clone())
            .prepare_and_dispose(ws, touch(dir.path(), "proc-2"), "cs-2".into())
            .await;
        assert_eq!(
            (report.outcome, report.attempt),
            (PreparationOutcome::SlotUnavailable, AttemptState::NotAsked)
        );
        assert_eq!(fake.count(ADMIT), 0);
        assert_eq!(report.into_error_body("gov").code, ErrorCode::WorkspaceBusy);
        drop(held);
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_requests_over_the_limit_are_refused_before_admission() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner().with_max_processes(3);
        // Admissions are slow, so all requests hold or seek slots together.
        let fake = Fake::admitting([]).slow(Duration::from_millis(200));
        let preparer = Preparer::new(fake.clone(), runner.clone());
        let requests: Vec<_> = (0..8)
            .map(|n| {
                let (preparer, ws) = (preparer.clone(), ws.clone());
                let req = touch(dir.path(), &format!("proc-c{n}"));
                tokio::spawn(async move {
                    preparer
                        .prepare_and_dispose(ws, req, format!("cs-c{n}"))
                        .await
                })
            })
            .collect();
        let mut outcomes = Vec::new();
        for request in requests {
            outcomes.push(request.await.unwrap().outcome);
        }
        let admitted = outcomes
            .iter()
            .filter(|outcome| **outcome == PreparationOutcome::LaunchUnavailable)
            .count();
        let refused = outcomes
            .iter()
            .filter(|outcome| **outcome == PreparationOutcome::SlotUnavailable)
            .count();
        assert_eq!((admitted, refused), (3, 5), "{outcomes:?}");
        assert_eq!(fake.count(ADMIT), 3);
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test]
    async fn an_expired_preparation_is_not_launched_and_is_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()]).then_cancel(attempt(dg::Phase::Expired, None));
        let req = touch(dir.path(), "proc-e");
        let prepared = Preparer::new(fake.clone(), runner.clone())
            .with_ttl_bound(Duration::from_millis(20))
            .prepare(&ws, req.clone(), "cs-e".into())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let report = prepared.launch(&req).await;
        assert_eq!(
            (report.outcome, report.attempt),
            (PreparationOutcome::Expired, AttemptState::Expired)
        );
        assert_eq!(fake.count(CANCEL), 1);
        assert_eq!(
            report.into_error_body("gov").code,
            ErrorCode::ResourceUnavailable
        );
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test]
    async fn a_preparation_cancelled_before_launch_frees_its_slot() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()]);
        let prepared = Preparer::new(fake.clone(), runner.clone())
            .prepare(&ws, touch(dir.path(), "proc-x"), "cs-x".into())
            .await
            .unwrap();
        assert_eq!(runner.occupied_slots(), 1);
        assert_eq!(prepared.cancel().await, AttemptState::Cancelled);
        assert_eq!(fake.count(CANCEL), 1);
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test]
    async fn another_command_is_not_launched_under_the_prepared_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()]);
        let req = touch(dir.path(), "proc-m");
        let first = Preparer::new(fake.clone(), runner.clone())
            .prepare(&ws, req.clone(), "cs-m".into())
            .await
            .unwrap();
        let mut other = req.clone();
        other.argv.push("again".into());
        let report = first.launch(&other).await;
        assert_eq!(
            (report.outcome, report.attempt),
            (PreparationOutcome::DigestMismatch, AttemptState::Cancelled)
        );
        // Neither another process ID nor another PTY choice passes for it either.
        for change in [
            |req: &mut RunnerExecRequest| req.process_id = ProcessId("proc-other".into()),
            |req: &mut RunnerExecRequest| req.tty = true,
        ] {
            let fake = Fake::admitting([prepared()]);
            let each = Preparer::new(fake.clone(), runner.clone())
                .prepare(&ws, req.clone(), "cs-m2".into())
                .await
                .unwrap();
            let mut other = req.clone();
            change(&mut other);
            assert_eq!(
                each.launch(&other).await.outcome,
                PreparationOutcome::DigestMismatch
            );
        }
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test]
    async fn an_unknown_admission_is_looked_up_never_replayed_and_settled_conservatively() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        for (lookup, cancels, expected) in [
            // A timeout or a lost reply after which the attempt is found prepared: cancelled.
            (prepared(), 1, AttemptState::Cancelled),
            // Found already settled: nothing to cancel.
            (attempt(dg::Phase::Expired, None), 0, AttemptState::Expired),
            // Not found is not proof that none exists.
            (
                Answer::Refused(devguard::ErrorCode::NotFound),
                0,
                AttemptState::Unknown,
            ),
            // No answer to the lookup either.
            (Answer::Unknown, 0, AttemptState::Unknown),
        ] {
            let fake = Fake::admitting([Answer::Unknown]).then_lookup(lookup);
            let report = Preparer::new(fake.clone(), runner.clone())
                .prepare_and_dispose(ws.clone(), touch(dir.path(), "proc-u"), "cs-u".into())
                .await;
            assert_eq!(
                (report.outcome, report.attempt),
                (PreparationOutcome::Unknown, expected)
            );
            // Asked once, looked up once: never admitted again, never rebuilt into a
            // prepared execution.
            assert_eq!(
                (fake.count(ADMIT), fake.count(LOOKUP), fake.count(CANCEL)),
                (1, 1, cancels)
            );
            let seen = fake.seen.lock().unwrap();
            assert!(seen.iter().all(|admission| *admission == seen[0]));
            drop(seen);
            assert_eq!(
                report.into_error_body("gov").code,
                ErrorCode::AdmissionUnknown
            );
            assert_nothing_started(dir.path(), &runner);
        }
    }

    #[tokio::test]
    async fn the_slot_and_the_attempt_are_released_separately() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        // The authority answers the admission, then cannot be reached to cancel it.
        let gone = devguard::Registration {
            status: devguard::Status {
                state: devguard::State::Unavailable,
                error_code: Some(devguard::ErrorCode::ResourceControlUnavailable),
                report: None,
            },
            state: devguard::RegistrationState::Unavailable,
            error_code: Some(devguard::ErrorCode::ResourceControlUnavailable),
            pid: None,
        };
        let fake = Fake::admitting([prepared()]).then_cancel(Answer::NotSent(gone));
        let report = Preparer::new(fake.clone(), runner.clone())
            .prepare_and_dispose(ws, touch(dir.path(), "proc-r"), "cs-r".into())
            .await;
        // The slot is free, as no process exists; the attempt is not claimed released.
        assert_eq!(
            (report.outcome, report.attempt),
            (PreparationOutcome::LaunchUnavailable, AttemptState::Unknown)
        );
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_caller_that_stops_waiting_leaves_the_preparation_to_settle() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()]).slow(Duration::from_millis(200));
        let preparer = Preparer::new(fake.clone(), runner.clone());
        let waiting =
            preparer.prepare_and_dispose(ws.clone(), touch(dir.path(), "proc-t"), "cs-t".into());
        // The caller's task ends while the admission is in flight.
        assert!(tokio::time::timeout(Duration::from_millis(50), waiting)
            .await
            .is_err());
        for _ in 0..100 {
            if fake.count(CANCEL) == 1 && runner.occupied_slots() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!((fake.count(ADMIT), fake.count(CANCEL)), (1, 1));
        assert_nothing_started(dir.path(), &runner);

        // A prepared execution dropped unsettled is cancelled in the background.
        let fake = Fake::admitting([prepared()]);
        let prepared = Preparer::new(fake.clone(), runner.clone())
            .prepare(&ws, touch(dir.path(), "proc-t2"), "cs-t2".into())
            .await
            .unwrap();
        drop(prepared);
        for _ in 0..100 {
            if fake.count(CANCEL) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(fake.count(CANCEL), 1);
        assert_nothing_started(dir.path(), &runner);
    }

    #[tokio::test]
    async fn only_a_required_workspace_with_a_valid_meaning_is_prepared() {
        let dir = tempfile::tempdir().unwrap();
        let runner = runner();
        let fake = Fake::admitting([]);
        let preparer = Preparer::new(fake.clone(), runner.clone());
        let mut off = governed(dir.path());
        off.resources.participation = Participation::Off;
        let report = preparer
            .prepare_and_dispose(off, touch(dir.path(), "proc-o"), "cs-o".into())
            .await;
        assert_eq!(report.outcome, PreparationOutcome::Invalid);
        let mut zero = touch(dir.path(), "proc-z");
        zero.timeout_ms = 0;
        let report = preparer
            .prepare_and_dispose(governed(dir.path()), zero, "cs-z".into())
            .await;
        assert_eq!(report.outcome, PreparationOutcome::Invalid);
        assert_eq!(fake.count(ADMIT), 0);
        assert_nothing_started(dir.path(), &runner);
    }

    #[test]
    fn the_meaning_follows_the_command_workspace_and_request() {
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let req = touch(dir.path(), "proc-1");
        let digest = meaning(&ws, &req).digest().unwrap();
        // The process ID is identity, not meaning.
        assert_eq!(
            meaning(&ws, &touch(dir.path(), "proc-2")).digest().unwrap(),
            digest
        );
        let mut other_ws = ws.clone();
        other_ws.id = WorkspaceId("other".into());
        let mut more = ws.clone();
        more.resources.request = Some(ResourceRequest {
            tasks: 65,
            ..ResourceRequest::default()
        });
        let mut tty = req.clone();
        tty.tty = true;
        let mut env = req.clone();
        env.env.overrides.insert("X".into(), "1".into());
        let mut network = req.clone();
        network.policy.network = codespace_policy::NetworkAxis::Enabled;
        for (ws, req) in [
            (&other_ws, &req),
            (&more, &req),
            (&ws, &tty),
            (&ws, &env),
            (&ws, &network),
        ] {
            assert_ne!(meaning(ws, req).digest().unwrap(), digest);
        }
    }
}
