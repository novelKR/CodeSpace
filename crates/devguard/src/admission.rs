//! Admission of one attempt by the registered execution owner (CSRG-U3).
//!
//! DevGuard keeps attempts for the instance registered in the same session, so each request
//! here is one bounded session: the owner's registration (see [`Owner::register`]), then one
//! `Admit`, `Lookup` or `Cancel` of the attempt, then close. Nothing here launches anything:
//! `BeginLaunch` and its permit are U4's.
//!
//! An [`Answer`] says only what the session established:
//!
//! - [`Answer::NotSent`]: the session ended before the request was written, so nothing was asked.
//! - [`Answer::Refused`]: DevGuard answered the request with an error. Each of its requests is
//!   one journal transaction that commits only before a record is answered, so an error answer
//!   means the request changed nothing.
//! - [`Answer::Attempt`]: DevGuard answered with this owner's record of this attempt, for this
//!   meaning.
//! - [`Answer::Unknown`]: the request may have reached DevGuard and no usable answer came back:
//!   a timeout, a lost reply, an EOF, an answer for another attempt, owner or meaning, or
//!   `resource_control_unavailable`, the code DevGuard's client also gives for every transport
//!   failure. Whether the attempt exists, or changed, is not known.
//!
//! No answer keeps DevGuard's messages.

use std::collections::{BTreeMap, BTreeSet};

use devguard_contract as contract;
use devguard_contract::{
    AdmissionRequest as WireAdmission, AttemptKey, AttemptRecord, Compatibility, ErrorCode as Code,
    ExecutionMeaning, ResourceIntent, PROTOCOL_VERSION,
};

use crate::registration::{Owner, Registration};
use crate::{effective_uid, ErrorCode};

/// The resource profile DevGuard admits interactive executions under.
pub const PROFILE: &str = "interactive";

/// How long DevGuard holds a prepared attempt that is not launched.
pub const PREPARED_TTL_MS: u64 = contract::PREPARED_TTL_MS;

/// How strongly a resource is controlled, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Accounted,
    Cooperative,
    Kernel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    pub cpu: Level,
    pub memory: Level,
    pub pids: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quantities {
    pub cpu_milli: u64,
    pub memory_bytes: u64,
    pub tasks: u64,
}

/// What an execution asks for, and the weakest control it accepts for each resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceRequest {
    pub requested: Quantities,
    pub minimum: Levels,
}

/// What an execution means: everything its digest covers. DevGuard records only the digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meaning {
    /// What is executed, as the owner names it.
    pub executable: String,
    /// Where it runs, as the owner names it.
    pub cwd: String,
    pub argv: Vec<String>,
    /// The environment the owner gives it.
    pub environment: BTreeMap<String, String>,
    pub tty: bool,
    pub timeout_ms: u64,
    pub resources: ResourceRequest,
}

impl Meaning {
    /// DevGuard's semantic digest (encoding version 1), or `None` for a meaning DevGuard does not
    /// encode: an empty argv or program, a zero timeout or a zero quantity.
    pub fn digest(&self) -> Option<String> {
        ExecutionMeaning {
            executable_identity: self.executable.clone(),
            cwd_identity: self.cwd.clone(),
            argv: self.argv.clone(),
            environment_changes: self.environment.clone(),
            tty: self.tty,
            timeout_ms: self.timeout_ms,
            resources: intent(&self.resources),
        }
        .digest()
        .ok()
    }
}

/// One attempt's admission: its identity under the owner's consumer, the digest of its meaning
/// and its resource request. The same value is presented for its lookup and cancellation, so
/// an answer for anything else is never taken for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub attempt_id: String,
    pub digest: String,
    pub resources: ResourceRequest,
}

/// An attempt's phase in DevGuard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Denied,
    Prepared,
    LaunchCommitted,
    ScopeBound,
    RunAuthorized,
    Draining,
    Suspect,
    Released,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Accounting,
    QosAndPriority,
    CgroupV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    ObservedProcessGroup,
    ContainedCgroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Planned {
    pub level: Level,
    pub method: Method,
}

/// How DevGuard would control the attempt: what it supports for this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub scope: ScopeKind,
    pub cpu: Planned,
    pub memory: Planned,
    pub pids: Planned,
}

/// What DevGuard holds for the attempt, and for how long it holds it unlaunched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub quantities: Quantities,
    pub prepared_ttl_ms: u64,
}

/// This owner's record of the attempt, without DevGuard's identities and digests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    pub phase: Phase,
    /// Why it was denied, when denied.
    pub denial: Option<ErrorCode>,
    pub plan: Option<Plan>,
    pub reservation: Option<Reservation>,
    /// Whether DevGuard reports resources applied to a launched scope.
    pub applied: bool,
    /// Whether DevGuard's record proves nothing was ever started for it.
    pub known_not_started: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The session ended before the request was written: nothing was asked.
    NotSent(Registration),
    /// DevGuard answered with this error, so the request changed nothing.
    Refused(ErrorCode),
    /// DevGuard answered with this owner's record of the attempt.
    Attempt(Attempt),
    /// The request may have reached DevGuard and no usable answer came back.
    Unknown,
}

impl Owner {
    /// Ask DevGuard to admit the attempt. Admitting the same attempt again with the same
    /// meaning returns its record; with another meaning, DevGuard refuses it.
    pub fn admit(&self, admission: &Admission) -> Answer {
        self.admit_with(admission, effective_uid(), self.pid())
    }

    /// DevGuard's record of the attempt.
    pub fn lookup(&self, admission: &Admission) -> Answer {
        self.attempt_request(admission, Request::Lookup, effective_uid(), self.pid())
    }

    /// Cancel the attempt. DevGuard cancels a prepared attempt, fences a launched one and leaves
    /// a settled one as it is, and answers its record.
    pub fn cancel(&self, admission: &Admission) -> Answer {
        self.attempt_request(admission, Request::Cancel, effective_uid(), self.pid())
    }

    pub(crate) fn admit_with(&self, admission: &Admission, uid: u32, pid: u32) -> Answer {
        self.attempt_request(admission, Request::Admit, uid, pid)
    }

    fn attempt_request(
        &self,
        admission: &Admission,
        request: Request,
        authority_uid: u32,
        owner_pid: u32,
    ) -> Answer {
        self.attempt_request_after(admission, request, authority_uid, owner_pid, &|| {})
    }

    /// [`Self::attempt_request`], calling `registered` between the registration and the request.
    fn attempt_request_after(
        &self,
        admission: &Admission,
        request: Request,
        authority_uid: u32,
        owner_pid: u32,
        registered: &dyn Fn(),
    ) -> Answer {
        let Some(wire) = self.wire(admission) else {
            return Answer::Refused(ErrorCode::InvalidRequest);
        };
        let Ok(fingerprint) = wire.fingerprint() else {
            return Answer::Refused(ErrorCode::InvalidRequest);
        };
        let (mut client, _) =
            match self.open_with(authority_uid, admission_compatibility(), owner_pid) {
                Ok(opened) => opened,
                Err(registration) => return Answer::NotSent(registration),
            };
        registered();
        let key = wire.key.clone();
        let answered = match request {
            Request::Admit => client.admit(wire),
            Request::Lookup => client.lookup(key.clone()),
            Request::Cancel => client.cancel(key.clone()),
        };
        match answered {
            Ok(record) => self.attempt(&record, &key, &fingerprint),
            Err(error) => answered_error(error.code),
        }
    }

    fn wire(&self, admission: &Admission) -> Option<WireAdmission> {
        let wire = WireAdmission {
            key: AttemptKey {
                consumer_id: self.settings().consumer.clone(),
                consumer_generation: self.settings().generation.clone(),
                attempt_id: admission.attempt_id.clone(),
            },
            execution_digest: admission.digest.clone(),
            intent: intent(&admission.resources),
        };
        wire.validate().ok().map(|()| wire)
    }

    /// The record, when it is this owner's record of this attempt for this meaning.
    fn attempt(&self, record: &AttemptRecord, key: &AttemptKey, fingerprint: &str) -> Answer {
        let ours = self
            .registered_identity()
            .is_some_and(|identity| identity == record.owner);
        if record.key != *key || record.request_fingerprint != fingerprint || !ours {
            return Answer::Unknown;
        }
        Answer::Attempt(Attempt {
            phase: phase(record.phase),
            denial: record.denial.map(ErrorCode::from),
            plan: record.plan.as_ref().map(plan),
            reservation: record.reservation.as_ref().map(|reservation| Reservation {
                quantities: quantities(reservation.quantities),
                prepared_ttl_ms: reservation
                    .prepare_deadline_ms
                    .saturating_sub(reservation.prepared_at.monotonic_ms),
            }),
            applied: record.applied.is_some(),
            known_not_started: record.known_not_started(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Request {
    Admit,
    Lookup,
    Cancel,
}

/// An error answer is definite, except DevGuard's client's own code for a transport failure.
fn answered_error(code: Code) -> Answer {
    match code {
        Code::ResourceControlUnavailable => Answer::Unknown,
        code => Answer::Refused(code.into()),
    }
}

/// Admission sessions need what registration needs, and durable admission.
pub(crate) fn admission_compatibility() -> Compatibility {
    Compatibility {
        minimum_protocol: PROTOCOL_VERSION,
        maximum_protocol: PROTOCOL_VERSION,
        required: BTreeSet::from([
            contract::Capability::StaticControlReservations,
            contract::Capability::DurableAdmission,
        ]),
    }
}

fn intent(resources: &ResourceRequest) -> ResourceIntent {
    ResourceIntent {
        profile: PROFILE.into(),
        requested: contract::Budget {
            cpu_milli: resources.requested.cpu_milli,
            memory_bytes: resources.requested.memory_bytes,
            tasks: resources.requested.tasks,
        },
        minimum: contract::ResourceLevels {
            cpu: level_to(resources.minimum.cpu),
            memory: level_to(resources.minimum.memory),
            pids: level_to(resources.minimum.pids),
        },
    }
}

fn level_to(level: Level) -> contract::EnforcementLevel {
    match level {
        Level::Accounted => contract::EnforcementLevel::Accounted,
        Level::Cooperative => contract::EnforcementLevel::Cooperative,
        Level::Kernel => contract::EnforcementLevel::Kernel,
    }
}

fn level(level: contract::EnforcementLevel) -> Level {
    match level {
        contract::EnforcementLevel::Accounted => Level::Accounted,
        contract::EnforcementLevel::Cooperative => Level::Cooperative,
        contract::EnforcementLevel::Kernel => Level::Kernel,
    }
}

fn quantities(budget: contract::Budget) -> Quantities {
    Quantities {
        cpu_milli: budget.cpu_milli,
        memory_bytes: budget.memory_bytes,
        tasks: budget.tasks,
    }
}

fn planned(resource: &contract::PlannedResource) -> Planned {
    Planned {
        level: level(resource.level),
        method: match resource.method {
            contract::ControlMethod::Accounting => Method::Accounting,
            contract::ControlMethod::QosAndPriority => Method::QosAndPriority,
            contract::ControlMethod::CgroupV2 => Method::CgroupV2,
        },
    }
}

fn plan(plan: &contract::ExecutionPlan) -> Plan {
    Plan {
        scope: match plan.scope_kind {
            contract::ScopeKind::ObservedProcessGroup => ScopeKind::ObservedProcessGroup,
            contract::ScopeKind::ContainedCgroup => ScopeKind::ContainedCgroup,
        },
        cpu: planned(&plan.cpu),
        memory: planned(&plan.memory),
        pids: planned(&plan.pids),
    }
}

fn phase(phase: contract::AttemptPhase) -> Phase {
    match phase {
        contract::AttemptPhase::Denied => Phase::Denied,
        contract::AttemptPhase::Prepared => Phase::Prepared,
        contract::AttemptPhase::LaunchCommitted => Phase::LaunchCommitted,
        contract::AttemptPhase::ScopeBound => Phase::ScopeBound,
        contract::AttemptPhase::RunAuthorized => Phase::RunAuthorized,
        contract::AttemptPhase::Draining => Phase::Draining,
        contract::AttemptPhase::Suspect => Phase::Suspect,
        contract::AttemptPhase::Released => Phase::Released,
        contract::AttemptPhase::Cancelled => Phase::Cancelled,
        contract::AttemptPhase::Expired => Phase::Expired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registration::RegistrationState;
    use crate::tests::{private_file, short_directory, CODES};
    use crate::{OwnerCredential, OwnerSettings};
    use devguard_client::framing::{read_frame, write_frame};
    use devguard_client::protocol::{
        Frame, Hello, PeerIdentity, Request as Wire, Response, ServiceStatus, SessionRole,
        WireError, FRAME_DEADLINE_MS, MAX_FRAME_BYTES, MAX_SESSIONS,
    };
    use devguard_contract::{InstanceIdentity, ProcessIdentity};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    pub(crate) fn request() -> ResourceRequest {
        ResourceRequest {
            requested: Quantities {
                cpu_milli: 500,
                memory_bytes: 64 << 20,
                tasks: 16,
            },
            minimum: Levels {
                cpu: Level::Accounted,
                memory: Level::Accounted,
                pids: Level::Accounted,
            },
        }
    }

    pub(crate) fn meaning(argv: &[&str]) -> Meaning {
        Meaning {
            executable: format!("argv0:{}", argv[0]),
            cwd: "workspace:demo".into(),
            argv: argv.iter().map(|part| part.to_string()).collect(),
            environment: BTreeMap::from([("LANG".into(), "C".into())]),
            tty: false,
            timeout_ms: 30_000,
            resources: request(),
        }
    }

    pub(crate) fn admission(attempt_id: &str, argv: &[&str]) -> Admission {
        Admission {
            attempt_id: attempt_id.into(),
            digest: meaning(argv).digest().unwrap(),
            resources: request(),
        }
    }

    #[test]
    fn the_digest_covers_every_part_of_the_meaning() {
        let base = meaning(&["/bin/echo", "a"]);
        let digest = base.digest().unwrap();
        assert_eq!(digest.len(), 64);
        assert_eq!(base.digest().unwrap(), digest);
        let mut changed = Vec::new();
        let mut each = base.clone();
        each.executable = "argv0:/bin/cat".into();
        changed.push(each);
        let mut each = base.clone();
        each.cwd = "workspace:other".into();
        changed.push(each);
        let mut each = base.clone();
        each.argv.push("b".into());
        changed.push(each);
        let mut each = base.clone();
        each.environment.insert("X".into(), "1".into());
        changed.push(each);
        let mut each = base.clone();
        each.tty = true;
        changed.push(each);
        let mut each = base.clone();
        each.timeout_ms += 1;
        changed.push(each);
        let mut each = base.clone();
        each.resources.requested.tasks += 1;
        changed.push(each);
        let mut each = base.clone();
        each.resources.minimum.cpu = Level::Kernel;
        changed.push(each);
        for meaning in changed {
            assert_ne!(meaning.digest().unwrap(), digest, "{meaning:?}");
        }
        for broken in [
            Meaning {
                argv: Vec::new(),
                ..base.clone()
            },
            Meaning {
                timeout_ms: 0,
                ..base.clone()
            },
            Meaning {
                resources: ResourceRequest {
                    requested: Quantities {
                        tasks: 0,
                        ..request().requested
                    },
                    ..request()
                },
                ..base.clone()
            },
        ] {
            assert_eq!(broken.digest(), None, "{broken:?}");
        }
    }

    #[test]
    fn only_the_transport_code_is_unknown() {
        for code in CODES {
            let expected = match code {
                Code::ResourceControlUnavailable => Answer::Unknown,
                code => Answer::Refused(code.into()),
            };
            assert_eq!(answered_error(code), expected, "{code:?}");
        }
    }

    // A scripted authority: replies, losses and delays a real one does not give at will.

    /// What the scripted authority does with the attempt request of session `n` (from 0).
    enum Reply {
        /// Answer this.
        With(Box<Response>),
        /// Read the request and close the connection: a lost reply.
        Close,
        /// Read the request and answer nothing until the client gives up.
        Silent,
    }

    type Script = Arc<dyn Fn(usize, &Wire, &InstanceIdentity) -> Reply + Send + Sync>;

    struct Scripted {
        stop: Arc<AtomicBool>,
        requests: Arc<AtomicUsize>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Scripted {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn me() -> PeerIdentity {
        PeerIdentity {
            uid: effective_uid(),
            pid: std::process::id(),
        }
    }

    fn instance(instance_id: &str) -> InstanceIdentity {
        InstanceIdentity {
            instance_id: instance_id.to_owned(),
            process: ProcessIdentity {
                boot_id: "boot".into(),
                pid: std::process::id(),
                start_ticks: 7,
            },
        }
    }

    fn scripted(socket: &Path, script: Script) -> Scripted {
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (signal, count) = (stop.clone(), requests.clone());
        let worker = std::thread::spawn(move || {
            let mut session = 0;
            while !signal.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                serve(&mut stream, &script, session, &count);
                session += 1;
            }
        });
        Scripted {
            stop,
            requests,
            worker: Some(worker),
        }
    }

    fn serve(stream: &mut UnixStream, script: &Script, session: usize, requests: &AtomicUsize) {
        let deadline = Duration::from_millis(FRAME_DEADLINE_MS);
        let mut registered = None;
        while let Ok(request) = read_frame::<Frame<Wire>>(stream, deadline) {
            let body = match &request.body {
                Wire::Hello { .. } => Response::Hello(Hello {
                    protocol: PROTOCOL_VERSION,
                    authority: me(),
                    caller: me(),
                    capabilities: admission_compatibility().required,
                    max_frame_bytes: MAX_FRAME_BYTES,
                    frame_deadline_ms: FRAME_DEADLINE_MS,
                    max_sessions: MAX_SESSIONS,
                }),
                Wire::Authenticate { .. } => Response::Authenticated {
                    role: SessionRole::ControlService,
                },
                Wire::Status => Response::Status(ServiceStatus {
                    storage_validated: true,
                    registration_ready: true,
                    execution_ready: true,
                    reason: "a reason that is never kept".into(),
                    configuration_fingerprint: "fingerprint".into(),
                }),
                Wire::Register { instance_id } => {
                    let identity = instance(instance_id);
                    registered = Some(identity.clone());
                    Response::Registered { instance: identity }
                }
                other => {
                    requests.fetch_add(1, Ordering::SeqCst);
                    match script(session, other, registered.as_ref().unwrap()) {
                        Reply::With(response) => *response,
                        Reply::Close => return,
                        Reply::Silent => {
                            std::thread::sleep(Duration::from_millis(3 * FRAME_DEADLINE_MS));
                            return;
                        }
                    }
                }
            };
            let reply = Frame {
                version: request.version,
                request_id: request.request_id,
                body,
            };
            if write_frame(stream, &reply, deadline).is_err() {
                return;
            }
        }
    }

    fn scripted_owner(dir: &Path, socket: &Path) -> Owner {
        let secret = "5ec2e7d0c0de".repeat(6);
        let credential = dir.join("scripted.secret");
        if !credential.exists() {
            private_file(dir, "scripted.secret", &secret.as_bytes()[..64]);
        }
        Owner::new(
            OwnerSettings {
                socket: socket.to_owned(),
                consumer: "codespace".into(),
                generation: "g1".into(),
            },
            OwnerCredential::File(credential),
        )
    }

    /// The record DevGuard keeps when it admits `request` for `owner`.
    fn record(
        request: &Wire,
        owner: &InstanceIdentity,
        phase: contract::AttemptPhase,
    ) -> AttemptRecord {
        let (key, fingerprint) = match request {
            Wire::Admit { request } => (request.key.clone(), request.fingerprint().unwrap()),
            _ => unreachable!("records are scripted for admissions"),
        };
        AttemptRecord {
            key,
            request_fingerprint: fingerprint,
            owner: owner.clone(),
            policy_revision: "revision".into(),
            phase,
            reservation: None,
            plan: None,
            scope: None,
            applied: None,
            denial: None,
            tracking_lost: false,
            release_reason: None,
        }
    }

    fn admitted(owner: &Owner, admission: &Admission) -> Answer {
        // DevGuard reads the credential within 250 ms (see `past_the_credential`).
        crate::tests::past_the_credential(
            |answer: &Answer| {
                matches!(answer, Answer::NotSent(registration)
                    if registration.state == RegistrationState::CredentialUnavailable)
            },
            || owner.admit(admission),
        )
    }

    #[test]
    fn a_session_that_ends_before_the_request_sends_nothing() {
        let dir = short_directory();
        let socket = dir.path().join("closed.sock");
        // The endpoint closes each session at once.
        let listener = UnixListener::bind(&socket).unwrap();
        let closer = std::thread::spawn(move || {
            for _ in 0..5 {
                drop(listener.accept());
            }
        });
        let answer = admitted(
            &scripted_owner(dir.path(), &socket),
            &admission("attempt-1", &["/bin/echo"]),
        );
        let Answer::NotSent(registration) = answer else {
            panic!("{answer:?}");
        };
        assert_eq!(registration.state, RegistrationState::Unavailable);
        drop(closer);
        // Without a usable credential no session opens at all.
        let unusable = Owner::new(
            OwnerSettings {
                socket: dir.path().join("absent.sock"),
                consumer: "codespace".into(),
                generation: "g1".into(),
            },
            OwnerCredential::Unavailable,
        );
        let Answer::NotSent(registration) = unusable.admit(&admission("attempt-1", &["/bin/echo"]))
        else {
            panic!("sent");
        };
        assert_eq!(registration.state, RegistrationState::CredentialUnavailable);
    }

    #[test]
    fn an_invalid_admission_is_refused_without_a_session() {
        let dir = short_directory();
        let socket = dir.path().join("watched.sock");
        let authority = scripted(&socket, Arc::new(|_, _, _| Reply::Close));
        let owner = scripted_owner(dir.path(), &socket);
        for invalid in [
            Admission {
                attempt_id: "has space".into(),
                ..admission("a", &["/bin/echo"])
            },
            Admission {
                digest: "not-a-digest".into(),
                ..admission("a", &["/bin/echo"])
            },
        ] {
            assert_eq!(
                owner.admit(&invalid),
                Answer::Refused(ErrorCode::InvalidRequest)
            );
        }
        assert_eq!(authority.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn every_error_answer_keeps_only_its_code() {
        let dir = short_directory();
        for code in CODES {
            let socket = dir.path().join(format!("{code:?}.sock"));
            let _authority = scripted(
                &socket,
                Arc::new(move |_, _, _| {
                    Reply::With(Box::new(Response::Error(WireError {
                        code,
                        message: "a message that is never kept".into(),
                    })))
                }),
            );
            let owner = scripted_owner(dir.path(), &socket);
            let answer = admitted(&owner, &admission("attempt-1", &["/bin/echo"]));
            assert_eq!(answer, answered_error(code), "{code:?}");
            assert!(!format!("{answer:?}").contains("never kept"));
        }
    }

    #[test]
    fn a_timeout_or_a_lost_reply_is_unknown_not_a_refusal() {
        let dir = short_directory();
        for (name, silent) in [("silent", true), ("lost", false)] {
            let socket = dir.path().join(format!("{name}.sock"));
            let authority = scripted(
                &socket,
                Arc::new(move |_, _, _| if silent { Reply::Silent } else { Reply::Close }),
            );
            let owner = scripted_owner(dir.path(), &socket);
            let started = std::time::Instant::now();
            for request in [Request::Admit, Request::Lookup, Request::Cancel] {
                let answer = crate::tests::past_the_credential(
                    |answer: &Answer| matches!(answer, Answer::NotSent(_)),
                    || {
                        owner.attempt_request(
                            &admission("attempt-1", &["/bin/echo"]),
                            request,
                            effective_uid(),
                            std::process::id(),
                        )
                    },
                );
                assert_eq!(answer, Answer::Unknown, "{name} {request:?}");
            }
            // The request reached the authority each time.
            assert!(authority.requests.load(Ordering::SeqCst) >= 3);
            assert!(started.elapsed() < Duration::from_secs(10));
        }
    }

    #[test]
    fn a_record_for_another_attempt_meaning_or_owner_is_unknown() {
        let dir = short_directory();
        let socket = dir.path().join("records.sock");
        // Session 0 answers the attempt's own record; then one for another attempt, one for
        // another meaning and one of another owner.
        let _authority = scripted(
            &socket,
            Arc::new(|session, request, owner| {
                let mut record = record(request, owner, contract::AttemptPhase::Prepared);
                match session {
                    0 => {}
                    1 => record.key.attempt_id = "attempt-other".into(),
                    2 => record.request_fingerprint = "0".repeat(64),
                    _ => record.owner.process.start_ticks += 1,
                }
                Reply::With(Box::new(Response::Attempt(record)))
            }),
        );
        let owner = scripted_owner(dir.path(), &socket);
        let admission = admission("attempt-1", &["/bin/echo"]);
        let Answer::Attempt(attempt) = admitted(&owner, &admission) else {
            panic!("not admitted");
        };
        assert_eq!(attempt.phase, Phase::Prepared);
        assert!(!attempt.applied);
        for _ in 1..4 {
            assert_eq!(admitted(&owner, &admission), Answer::Unknown);
        }
    }

    // DevGuard's fixture authority with native host evidence, as on macOS.

    #[cfg(target_os = "macos")]
    mod native {
        use super::*;
        use crate::registration::tests::native::{provision, Provisioned};
        use devguard_contract::Budget;

        fn admitting() -> Provisioned {
            let provisioned = provision(2);
            provisioned
                .authority
                .wait_until_admitting(Duration::from_secs(10))
                .unwrap();
            provisioned
        }

        fn charged(provisioned: &Provisioned) -> Budget {
            provisioned.authority.committed().unwrap()
        }

        #[test]
        fn an_admitted_attempt_is_prepared_reserved_and_not_applied_then_cancelled() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let admission = admission("cs-attempt-1", &["/bin/echo", "a"]);
            let Answer::Attempt(attempt) = admitted(&owner, &admission) else {
                panic!("not admitted");
            };
            assert_eq!(attempt.phase, Phase::Prepared, "{attempt:?}");
            assert_eq!(attempt.denial, None);
            assert!(!attempt.applied && !attempt.known_not_started);
            let reservation = attempt.reservation.unwrap();
            assert_eq!(reservation.quantities, request().requested);
            assert_eq!(reservation.prepared_ttl_ms, contract::PREPARED_TTL_MS);
            // What DevGuard supports on macOS, at least what was required.
            let plan = attempt.plan.unwrap();
            assert_eq!(plan.scope, ScopeKind::ObservedProcessGroup);
            assert_eq!(
                (plan.cpu.level, plan.memory.level, plan.pids.level),
                (Level::Cooperative, Level::Accounted, Level::Accounted)
            );
            assert_eq!(plan.cpu.method, Method::QosAndPriority);
            assert_eq!(charged(&provisioned).tasks, request().requested.tasks);
            // Admitting it again returns the same record; looking it up too.
            assert_eq!(admitted(&owner, &admission), Answer::Attempt(attempt));
            assert_eq!(owner.lookup(&admission), Answer::Attempt(attempt));
            let Answer::Attempt(cancelled) = owner.cancel(&admission) else {
                panic!("not cancelled");
            };
            assert_eq!(cancelled.phase, Phase::Cancelled);
            assert!(cancelled.known_not_started);
            assert_eq!(charged(&provisioned), Budget::ZERO);
            // Cancelling again keeps the settled record.
            assert_eq!(owner.cancel(&admission), Answer::Attempt(cancelled));
        }

        #[test]
        fn the_same_attempt_for_another_command_is_refused() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let first = admission("cs-attempt-2", &["/bin/echo", "a"]);
            assert!(matches!(admitted(&owner, &first), Answer::Attempt(_)));
            let other = admission("cs-attempt-2", &["/bin/echo", "b"]);
            assert_eq!(
                admitted(&owner, &other),
                Answer::Refused(ErrorCode::AttemptConflict)
            );
            // Nor is it found or cancelled under the other meaning.
            assert_eq!(owner.lookup(&other), Answer::Unknown);
            assert_eq!(owner.cancel(&other), Answer::Unknown);
            assert!(
                matches!(owner.cancel(&first), Answer::Attempt(a) if a.phase == Phase::Cancelled)
            );
        }

        #[test]
        fn a_shortage_or_an_unsupported_minimum_is_denied_and_reserves_nothing() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let capacity = provisioned.authority.work_capacity().unwrap();
            let mut shortage = admission("cs-short", &["/bin/echo"]);
            shortage.resources.requested.memory_bytes = capacity.memory_bytes + 1;
            shortage.digest = Meaning {
                resources: shortage.resources,
                ..meaning(&["/bin/echo"])
            }
            .digest()
            .unwrap();
            let mut kernel = admission("cs-kernel", &["/bin/echo"]);
            kernel.resources.minimum.memory = Level::Kernel;
            kernel.digest = Meaning {
                resources: kernel.resources,
                ..meaning(&["/bin/echo"])
            }
            .digest()
            .unwrap();
            for (admission, denial) in [
                (shortage, ErrorCode::ResourceUnavailable),
                (kernel, ErrorCode::ResourcePolicyUnsupported),
            ] {
                let Answer::Attempt(attempt) = admitted(&owner, &admission) else {
                    panic!("no record");
                };
                assert_eq!(
                    (attempt.phase, attempt.denial, attempt.reservation),
                    (Phase::Denied, Some(denial), None)
                );
                assert!(attempt.known_not_started);
            }
            assert_eq!(charged(&provisioned), Budget::ZERO);
        }

        #[test]
        fn a_timed_out_admission_may_still_have_reserved() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let admission = admission("cs-slow", &["/bin/echo"]);
            // Once the session is registered, the authority's lock is held past the client's
            // deadline, so the admission waits behind it and the client gives up.
            let holder = std::sync::Mutex::new(None);
            let answer = crate::tests::past_the_credential(
                |answer: &Answer| matches!(answer, Answer::NotSent(_)),
                || {
                    owner.attempt_request_after(
                        &admission,
                        Request::Admit,
                        effective_uid(),
                        std::process::id(),
                        &|| {
                            let held = provisioned
                                .authority
                                .hold_authority(Duration::from_millis(900));
                            *holder.lock().unwrap() = Some(held);
                        },
                    )
                },
            );
            if let Some(held) = holder.lock().unwrap().take() {
                held.join().unwrap();
            }
            assert_eq!(answer, Answer::Unknown);
            // DevGuard admitted it after the client gave up: the timeout was not a refusal.
            let found = (0..20).find_map(|_| match owner.lookup(&admission) {
                Answer::Attempt(attempt) => Some(attempt),
                _ => {
                    std::thread::sleep(Duration::from_millis(50));
                    None
                }
            });
            let attempt = found.expect("the timed-out admission was recorded");
            assert_eq!(attempt.phase, Phase::Prepared);
            assert!(
                matches!(owner.cancel(&admission), Answer::Attempt(a) if a.phase == Phase::Cancelled)
            );
        }

        #[test]
        fn an_unlaunched_attempt_expires_on_its_own() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let admission = admission("cs-expiring", &["/bin/echo"]);
            assert!(
                matches!(admitted(&owner, &admission), Answer::Attempt(a) if a.phase == Phase::Prepared)
            );
            std::thread::sleep(Duration::from_millis(contract::PREPARED_TTL_MS + 300));
            let Answer::Attempt(expired) = owner.lookup(&admission) else {
                panic!("not found");
            };
            assert_eq!(expired.phase, Phase::Expired);
            assert!(expired.known_not_started);
            assert_eq!(charged(&provisioned), Budget::ZERO);
            // Cancelling an expired attempt leaves it expired.
            assert_eq!(owner.cancel(&admission), Answer::Attempt(expired));
        }

        #[test]
        fn another_owner_cannot_see_or_cancel_the_attempt() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let admission = admission("cs-mine", &["/bin/echo"]);
            assert!(matches!(admitted(&owner, &admission), Answer::Attempt(_)));
            // A second owner in this process is another instance of the same consumer.
            let other = provisioned.owner();
            assert_eq!(
                other.lookup(&admission),
                Answer::Refused(ErrorCode::Unauthorized)
            );
            assert_eq!(
                other.cancel(&admission),
                Answer::Refused(ErrorCode::Unauthorized)
            );
            assert!(
                matches!(owner.cancel(&admission), Answer::Attempt(a) if a.phase == Phase::Cancelled)
            );
        }
    }
}
