//! Execution-owner registration (CSRG-U2).
//!
//! The process that owns CodeSpace's executions registers itself: the gateway in InProcess
//! mode, the worker in UDS mode. DevGuard takes the registered process identity from the
//! session's peer, so a process can only ever register itself; [`Owner`] also checks that the
//! identity DevGuard registered is its own.
//!
//! An [`Owner`] has one instance identity for its whole life. Each [`Owner::register`] opens a
//! bounded session (connect, `Hello`, `Authenticate`, `Status`, `Register` with that same
//! instance, then close), as DevGuard's sessions cannot wait idle. DevGuard keeps the instance
//! active while the session lasts and suspect after it; registering again reactivates the same
//! instance. Nothing is admitted or launched.
//!
//! The owner registers as a control service, whose control costs DevGuard covers with the
//! consumer's static control reservation, so it requires the role `control_service` and the
//! capability `static_control_reservations`.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use devguard_client::protocol::{CallerCredential, SessionRole};
use devguard_client::Client;
use devguard_contract as contract;
use devguard_contract::{Compatibility, Error, InstanceIdentity, PROTOCOL_VERSION};

use crate::handoff::HandedCredential;
use crate::{classify, effective_uid, read_secret, report, ErrorCode, State, Status, Step};

/// Where an owner's consumer secret comes from.
#[derive(Debug)]
pub enum OwnerCredential {
    /// The private credential file, read for each session, as a status probe reads it. The
    /// in-process owner uses it.
    File(PathBuf),
    /// The secret this process was handed when it started. The UDS worker uses it.
    Handed(HandedCredential),
    /// No secret could be read or received: every session is reported as
    /// `credential_unavailable` without being opened.
    Unavailable,
}

/// Where the authority is and which consumer the owner is. None of these is a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerSettings {
    pub socket: PathBuf,
    pub consumer: String,
    pub generation: String,
}

/// What a registration session found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationState {
    /// DevGuard registered this process under the owner's instance identity.
    Registered,
    /// No authority answered, or a response was missing or invalid.
    Unavailable,
    /// The socket's peer, or the identity it declared, is not the expected authority.
    UntrustedAuthority,
    /// The authority offers no protocol or capability set registration needs.
    Incompatible,
    /// The consumer settings or secret could not be used, so no session was opened.
    CredentialUnavailable,
    /// The authority refused the consumer credential: a wrong consumer, generation or secret.
    CredentialRefused,
    /// The consumer is not a control service.
    RoleMismatch,
    /// The authority is not ready to register instances.
    NotReady,
    /// The authority refused the registration.
    Refused,
    /// The identity DevGuard registered is not this owner process, or it changed.
    OwnerMismatch,
}

/// One registration session's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The authority's status from the same session, as a status probe reports it.
    pub status: Status,
    pub state: RegistrationState,
    /// DevGuard's code for the step that failed; its message is not kept.
    pub error_code: Option<ErrorCode>,
    /// The owner's process ID as DevGuard registered it, when registered.
    pub pid: Option<u32>,
}

/// The execution owner: one instance identity, registered again by each session.
#[derive(Debug)]
pub struct Owner {
    settings: OwnerSettings,
    credential: OwnerCredential,
    instance_id: String,
    pid: u32,
    /// The identity of the first registration. Every later one must be the same.
    registered: Mutex<Option<InstanceIdentity>>,
}

impl Owner {
    /// The owner is this process. Its instance identity is minted here, once.
    pub fn new(settings: OwnerSettings, credential: OwnerCredential) -> Self {
        Self {
            settings,
            credential,
            instance_id: mint_instance_id(),
            pid: std::process::id(),
            registered: Mutex::new(None),
        }
    }

    /// An owner that claims another instance identity, as a second process might. Only the
    /// tests against an authority with native host evidence use it.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn with_instance_id(
        settings: OwnerSettings,
        credential: OwnerCredential,
        instance_id: String,
    ) -> Self {
        Self {
            instance_id,
            ..Self::new(settings, credential)
        }
    }

    /// The instance identity every session of this owner registers.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// The consumer this owner registers as.
    pub fn consumer(&self) -> &str {
        &self.settings.consumer
    }

    /// The consumer's generation.
    pub fn generation(&self) -> &str {
        &self.settings.generation
    }

    pub(crate) fn settings(&self) -> &OwnerSettings {
        &self.settings
    }

    /// The identity DevGuard registered for this owner, once it has.
    pub(crate) fn registered_identity(&self) -> Option<InstanceIdentity> {
        self.registered
            .lock()
            .ok()
            .and_then(|registered| registered.clone())
    }

    /// One bounded registration session.
    pub fn register(&self) -> Registration {
        self.register_with(effective_uid(), registration(), self.pid)
    }

    pub(crate) fn register_with(
        &self,
        authority_uid: u32,
        compatibility: Compatibility,
        owner_pid: u32,
    ) -> Registration {
        match self.open_with(authority_uid, compatibility, owner_pid) {
            Ok((_, registration)) | Err(registration) => registration,
        }
    }

    /// A registered session, still open for one more request, or the registration that
    /// stopped before it. Only a [`RegistrationState::Registered`] outcome comes with a client.
    pub(crate) fn open_with(
        &self,
        authority_uid: u32,
        compatibility: Compatibility,
        owner_pid: u32,
    ) -> Result<(Client, Registration), Registration> {
        // The credential is read first, so a local problem opens no session.
        let credential = match self.caller_credential() {
            Ok(credential) => credential,
            Err(error) => return Err(failed(Step::Credential, &error)),
        };
        let mut client = match Client::connect(&self.settings.socket, authority_uid, compatibility)
        {
            Ok(client) => client,
            Err(error) => return Err(failed(Step::Connect, &error)),
        };
        let role = match client.authenticate(credential) {
            Ok(role) => role,
            Err(error) => return Err(failed(Step::Authenticate, &error)),
        };
        let service = match client.status() {
            Ok(status) => status,
            Err(error) => return Err(failed(Step::Status, &error)),
        };
        let registration_ready = service.registration_ready;
        let status = Status {
            state: State::Available,
            error_code: None,
            report: Some(report(&client, role, service)),
        };
        let outcome = |state, error_code, pid| Registration {
            status: status.clone(),
            state,
            error_code,
            pid,
        };
        if role != SessionRole::ControlService {
            return Err(outcome(RegistrationState::RoleMismatch, None, None));
        }
        if !registration_ready {
            return Err(outcome(RegistrationState::NotReady, None, None));
        }
        let instance = match client.register(self.instance_id.clone()) {
            Ok(instance) => instance,
            Err(error) => return Err(outcome(refusal(error.code), Some(error.code.into()), None)),
        };
        // DevGuard registers the session's peer. It must be this owner, under its own identity,
        // and every session must register the same identity.
        if instance.instance_id != self.instance_id || instance.process.pid != owner_pid {
            return Err(outcome(RegistrationState::OwnerMismatch, None, None));
        }
        let mut registered = match self.registered.lock() {
            Ok(registered) => registered,
            Err(_) => return Err(outcome(RegistrationState::OwnerMismatch, None, None)),
        };
        match registered.as_ref() {
            Some(first) if *first != instance => {
                return Err(outcome(RegistrationState::OwnerMismatch, None, None))
            }
            Some(_) => {}
            None => *registered = Some(instance.clone()),
        }
        drop(registered);
        let registration = outcome(
            RegistrationState::Registered,
            None,
            Some(instance.process.pid),
        );
        Ok((client, registration))
    }

    fn caller_credential(&self) -> Result<CallerCredential, Error> {
        contract::validate_id(&self.settings.consumer)?;
        contract::validate_id(&self.settings.generation)?;
        let secret = match &self.credential {
            OwnerCredential::File(path) => read_secret(path)?,
            OwnerCredential::Handed(handed) => handed.secret().clone(),
            OwnerCredential::Unavailable => return Err(crate::not_private()),
        };
        Ok(CallerCredential::Consumer {
            consumer_id: self.settings.consumer.clone(),
            generation: self.settings.generation.clone(),
            secret,
        })
    }
}

/// Registration needs DevGuard's protocol 1 and static control reservations.
pub(crate) fn registration() -> Compatibility {
    Compatibility {
        minimum_protocol: PROTOCOL_VERSION,
        maximum_protocol: PROTOCOL_VERSION,
        required: BTreeSet::from([contract::Capability::StaticControlReservations]),
    }
}

/// A session that ended before `Register`, mapped as a status probe maps it.
fn failed(step: Step, error: &Error) -> Registration {
    let state = match classify(step, error.code) {
        State::Available | State::Unavailable => RegistrationState::Unavailable,
        State::UntrustedAuthority => RegistrationState::UntrustedAuthority,
        State::Incompatible => RegistrationState::Incompatible,
        State::CredentialRefused => RegistrationState::CredentialRefused,
        State::CredentialUnavailable => RegistrationState::CredentialUnavailable,
    };
    let status = crate::failed(step, error);
    Registration {
        error_code: status.error_code,
        status,
        state,
        pid: None,
    }
}

/// What a refused `Register` means. Its message is never kept.
fn refusal(code: contract::ErrorCode) -> RegistrationState {
    match code {
        // The peer or its process identity, the generation, the instance identity or the
        // instance pool refused this owner.
        contract::ErrorCode::Unauthorized
        | contract::ErrorCode::AttemptConflict
        | contract::ErrorCode::ResourceUnavailable
        | contract::ErrorCode::InvalidRequest
        | contract::ErrorCode::ResourcePolicyUnsupported => RegistrationState::Refused,
        _ => RegistrationState::Unavailable,
    }
}

/// `codespace-` and 32 random hexadecimal digits. A restarted owner is a new process, which
/// DevGuard would refuse under an earlier owner's instance identity.
fn mint_instance_id() -> String {
    let mut bytes = [0u8; 16];
    let random = std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .is_ok();
    if !random {
        // Unique enough without randomness: this process's ID and the time it minted the ID.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        bytes[..4].copy_from_slice(&std::process::id().to_be_bytes());
        bytes[4..].copy_from_slice(&nanos.to_be_bytes()[4..]);
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("codespace-{hex}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::{past_the_credential, private_file, short_directory, Authority, CODES};
    use crate::{Capability, Role};
    use devguard_client::framing::{read_frame, write_frame};
    use devguard_client::protocol::{
        Frame, Hello, PeerIdentity, Request, Response, ServiceStatus, WireError, FRAME_DEADLINE_MS,
        MAX_FRAME_BYTES, MAX_SESSIONS,
    };
    use devguard_contract::ProcessIdentity;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn instance_identities_are_valid_devguard_ids_and_differ() {
        let first = mint_instance_id();
        let second = mint_instance_id();
        assert_ne!(first, second);
        for id in [&first, &second] {
            contract::validate_id(id).unwrap();
            assert_eq!(id.len(), "codespace-".len() + 32);
            assert!(id["codespace-".len()..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        }
    }

    #[test]
    fn every_register_refusal_maps_to_refused_or_unavailable() {
        for code in CODES {
            let expected = match code {
                contract::ErrorCode::Unauthorized
                | contract::ErrorCode::AttemptConflict
                | contract::ErrorCode::ResourceUnavailable
                | contract::ErrorCode::InvalidRequest
                | contract::ErrorCode::ResourcePolicyUnsupported => RegistrationState::Refused,
                _ => RegistrationState::Unavailable,
            };
            assert_eq!(refusal(code), expected, "{code:?}");
        }
    }

    #[test]
    fn a_session_that_ends_early_keeps_the_status_mapping() {
        for code in CODES {
            let error = Error::new(code, "a message that is not kept");
            for step in [
                Step::Credential,
                Step::Connect,
                Step::Authenticate,
                Step::Status,
            ] {
                let registration = failed(step, &error);
                let status = crate::failed(step, &error);
                assert_eq!(registration.status, status);
                assert_eq!(registration.error_code, status.error_code);
                assert_eq!(registration.pid, None);
                let expected = match status.state {
                    State::Available | State::Unavailable => RegistrationState::Unavailable,
                    State::UntrustedAuthority => RegistrationState::UntrustedAuthority,
                    State::Incompatible => RegistrationState::Incompatible,
                    State::CredentialRefused => RegistrationState::CredentialRefused,
                    State::CredentialUnavailable => RegistrationState::CredentialUnavailable,
                };
                assert_eq!(registration.state, expected);
            }
        }
    }

    /// `owner.register()`, again while it stops at the credential (see `past_the_credential`).
    fn registered(owner: &Owner) -> Registration {
        past_the_credential(stopped_at_the_credential, || owner.register())
    }

    fn registered_with(
        owner: &Owner,
        authority_uid: u32,
        compatibility: Compatibility,
        owner_pid: u32,
    ) -> Registration {
        past_the_credential(stopped_at_the_credential, || {
            owner.register_with(authority_uid, compatibility.clone(), owner_pid)
        })
    }

    fn stopped_at_the_credential(registration: &Registration) -> bool {
        registration.state == RegistrationState::CredentialUnavailable
    }

    fn owner(settings: &crate::Settings) -> Owner {
        Owner::new(
            OwnerSettings {
                socket: settings.socket.clone(),
                consumer: settings.consumer.clone(),
                generation: settings.generation.clone(),
            },
            OwnerCredential::File(settings.credential_file.clone()),
        )
    }

    /// Registration without its capability requirement, to reach later steps on an
    /// authority that states no capability.
    fn any_capability() -> Compatibility {
        Compatibility {
            required: BTreeSet::new(),
            ..registration()
        }
    }

    // A real DevGuard authority on every platform: the steps before `Register`.

    #[test]
    fn a_wrong_consumer_generation_or_secret_is_refused_before_registering() {
        let authority = Authority::start();
        let other = "0".repeat(64);
        let mut secret = authority.settings();
        secret.credential_file =
            private_file(authority.directory.path(), "other.secret", other.as_bytes());
        let mut generation = authority.settings();
        generation.generation = "g-other".into();
        let mut consumer = authority.settings();
        consumer.consumer = "nobody".into();
        for settings in [secret, generation, consumer] {
            let registration = registered_with(
                &owner(&settings),
                effective_uid(),
                any_capability(),
                std::process::id(),
            );
            assert_eq!(
                (
                    registration.state,
                    registration.error_code,
                    registration.pid
                ),
                (
                    RegistrationState::CredentialRefused,
                    Some(ErrorCode::Unauthorized),
                    None
                ),
                "{settings:?}"
            );
        }
    }

    #[test]
    fn another_authority_uid_is_untrusted() {
        let authority = Authority::start();
        let registration = registered_with(
            &owner(&authority.settings()),
            effective_uid().wrapping_add(1),
            registration(),
            std::process::id(),
        );
        assert_eq!(
            (registration.state, registration.error_code),
            (
                RegistrationState::UntrustedAuthority,
                Some(ErrorCode::Unauthorized)
            )
        );
    }

    #[test]
    fn another_protocol_or_a_missing_capability_is_incompatible() {
        let authority = Authority::start();
        let protocol = Compatibility {
            minimum_protocol: PROTOCOL_VERSION + 1,
            maximum_protocol: PROTOCOL_VERSION + 1,
            ..registration()
        };
        let capability = Compatibility {
            required: BTreeSet::from([contract::Capability::LinuxCgroupV2]),
            ..registration()
        };
        for compatibility in [protocol, capability] {
            let registration = registered_with(
                &owner(&authority.settings()),
                effective_uid(),
                compatibility,
                std::process::id(),
            );
            assert_eq!(
                (registration.state, registration.error_code),
                (
                    RegistrationState::Incompatible,
                    Some(ErrorCode::ResourcePolicyUnsupported)
                )
            );
        }
    }

    /// Without native host evidence DevGuard states no capability, so registration, which
    /// needs static control reservations, is incompatible with it (Linux before DG-LINUX).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_authority_without_native_evidence_cannot_register_an_owner() {
        let authority = Authority::start();
        let registration = registered(&owner(&authority.settings()));
        assert_eq!(
            (
                registration.state,
                registration.error_code,
                registration.pid
            ),
            (
                RegistrationState::Incompatible,
                Some(ErrorCode::ResourcePolicyUnsupported),
                None
            )
        );
    }

    #[test]
    fn an_unusable_credential_or_setting_opens_no_session() {
        let authority = Authority::start();
        let dir = authority.directory.path();
        // A listener stands in for the authority, so an attempted session would be seen.
        let socket = dir.join("watched.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let shared = private_file(dir, "shared.secret", authority.secret().as_bytes());
        std::fs::set_permissions(&shared, std::os::unix::fs::PermissionsExt::from_mode(0o640))
            .unwrap();
        let settings = |consumer: &str| OwnerSettings {
            socket: socket.clone(),
            consumer: consumer.into(),
            generation: authority.settings().generation,
        };
        let cases = [
            (settings("dev-cli"), dir.join("missing.secret")),
            (settings("dev-cli"), shared),
            (settings(""), authority.paths.cli_credential()),
            (settings("has space"), authority.paths.cli_credential()),
        ];
        let cases = cases
            .into_iter()
            .map(|(settings, file)| (settings, OwnerCredential::File(file)))
            .chain([(settings("dev-cli"), OwnerCredential::Unavailable)]);
        for (settings, credential) in cases {
            let registration = Owner::new(settings, credential).register();
            assert_eq!(
                (
                    registration.state,
                    registration.error_code,
                    registration.pid
                ),
                (RegistrationState::CredentialUnavailable, None, None)
            );
        }
        assert_eq!(
            listener.accept().map(|_| ()).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "a session was attempted"
        );
    }

    // A scripted authority: replies a real one does not give at will.

    /// The answer to `Register` with an instance identity in session `n` (from 0).
    type Registered = Box<dyn Fn(usize, &str) -> Response + Send + Sync>;

    /// What the scripted authority answers.
    struct Script {
        role: SessionRole,
        registration_ready: bool,
        registered: Registered,
    }

    struct Scripted {
        stop: Arc<AtomicBool>,
        registers: Arc<AtomicUsize>,
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

    fn answer(request: &Request, script: &Script, session: usize) -> Option<Response> {
        Some(match request {
            Request::Hello { .. } => Response::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                authority: me(),
                caller: me(),
                capabilities: BTreeSet::from([contract::Capability::StaticControlReservations]),
                max_frame_bytes: MAX_FRAME_BYTES,
                frame_deadline_ms: FRAME_DEADLINE_MS,
                max_sessions: MAX_SESSIONS,
            }),
            Request::Authenticate { .. } => Response::Authenticated { role: script.role },
            Request::Status => Response::Status(ServiceStatus {
                storage_validated: true,
                registration_ready: script.registration_ready,
                execution_ready: false,
                reason: "a reason that is never kept".into(),
                configuration_fingerprint: "fingerprint".into(),
            }),
            Request::Register { instance_id } => (script.registered)(session, instance_id),
            _ => return None,
        })
    }

    fn scripted(socket: &Path, script: Script) -> Scripted {
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let registers = Arc::new(AtomicUsize::new(0));
        let (signal, count) = (stop.clone(), registers.clone());
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
            registers,
            worker: Some(worker),
        }
    }

    fn serve(stream: &mut UnixStream, script: &Script, session: usize, registers: &AtomicUsize) {
        let deadline = Duration::from_millis(FRAME_DEADLINE_MS);
        while let Ok(request) = read_frame::<Frame<Request>>(stream, deadline) {
            if matches!(request.body, Request::Register { .. }) {
                registers.fetch_add(1, Ordering::SeqCst);
            }
            let Some(body) = answer(&request.body, script, session) else {
                return;
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

    fn identity(instance_id: &str, pid: u32, start_ticks: u64) -> Response {
        Response::Registered {
            instance: InstanceIdentity {
                instance_id: instance_id.to_owned(),
                process: ProcessIdentity {
                    boot_id: "boot".into(),
                    pid,
                    start_ticks,
                },
            },
        }
    }

    fn scripted_owner(dir: &Path, socket: &Path) -> Owner {
        let secret = "5ec2e7d0c0de".repeat(6);
        let credential = private_file(dir, "scripted.secret", &secret.as_bytes()[..64]);
        Owner::new(
            OwnerSettings {
                socket: socket.to_owned(),
                consumer: "codespace".into(),
                generation: "g1".into(),
            },
            OwnerCredential::File(credential),
        )
    }

    #[test]
    fn the_registered_identity_must_be_this_owner_and_stay_the_same() {
        let dir = short_directory();
        let socket = dir.path().join("scripted.sock");
        // Session 0 registers this process; session 1 the same instance with another start;
        // session 2 another process; session 3 another instance identity.
        let _authority = scripted(
            &socket,
            Script {
                role: SessionRole::ControlService,
                registration_ready: true,
                registered: Box::new(|session, instance_id| match session {
                    0 | 4 => identity(instance_id, std::process::id(), 7),
                    1 => identity(instance_id, std::process::id(), 8),
                    2 => identity(instance_id, std::process::id() + 1, 7),
                    _ => identity("codespace-other", std::process::id(), 7),
                }),
            },
        );
        let owner = scripted_owner(dir.path(), &socket);
        let states: Vec<_> = (0..5)
            .map(|_| {
                let registration = registered(&owner);
                (registration.state, registration.pid)
            })
            .collect();
        let me = Some(std::process::id());
        assert_eq!(
            states,
            [
                (RegistrationState::Registered, me),
                (RegistrationState::OwnerMismatch, None),
                (RegistrationState::OwnerMismatch, None),
                (RegistrationState::OwnerMismatch, None),
                (RegistrationState::Registered, me),
            ]
        );
    }

    #[test]
    fn a_workload_consumer_or_an_authority_not_ready_is_not_asked_to_register() {
        let dir = short_directory();
        for (role, ready, expected) in [
            (SessionRole::Workload, true, RegistrationState::RoleMismatch),
            (
                SessionRole::ControlService,
                false,
                RegistrationState::NotReady,
            ),
        ] {
            let socket = dir.path().join(format!("{role:?}-{ready}.sock"));
            let authority = scripted(
                &socket,
                Script {
                    role,
                    registration_ready: ready,
                    registered: Box::new(|_, id| identity(id, std::process::id(), 7)),
                },
            );
            let registration = registered(&scripted_owner(dir.path(), &socket));
            assert_eq!(
                (
                    registration.state,
                    registration.error_code,
                    registration.pid
                ),
                (expected, None, None)
            );
            // The status of the same session is still reported, without its reason.
            assert_eq!(registration.status.state, State::Available);
            let report = registration.status.report.unwrap();
            assert_eq!(report.role, Role::from(role));
            assert_eq!(report.capabilities, [Capability::StaticControlReservations]);
            assert_eq!(report.registration_ready, ready);
            assert_eq!(authority.registers.load(Ordering::SeqCst), 0);
            std::fs::remove_file(dir.path().join("scripted.secret")).unwrap();
        }
    }

    #[test]
    fn every_refused_register_keeps_only_its_code() {
        let dir = short_directory();
        for code in CODES {
            let socket = dir.path().join(format!("{code:?}.sock"));
            let _authority = scripted(
                &socket,
                Script {
                    role: SessionRole::ControlService,
                    registration_ready: true,
                    registered: Box::new(move |_, _| {
                        Response::Error(WireError {
                            code,
                            message: "a message that is never kept".into(),
                        })
                    }),
                },
            );
            let registration = registered(&scripted_owner(dir.path(), &socket));
            assert_eq!(
                (
                    registration.state,
                    registration.error_code,
                    registration.pid
                ),
                (refusal(code), Some(ErrorCode::from(code)), None)
            );
            assert!(!format!("{registration:?}").contains("never kept"));
            std::fs::remove_file(dir.path().join("scripted.secret")).unwrap();
        }
    }

    // DevGuard's fixture authority with native host evidence, as on macOS.

    #[cfg(target_os = "macos")]
    pub(crate) mod native {
        use super::*;
        use devguard_daemon::config::ConsumerConfig;
        use devguard_daemon::fixture::TestAuthority;
        use devguard_daemon::paths::{write_new_private, AuthorityPaths};
        use std::process::Command;

        const CONSUMER: &str = "codespace";
        const GENERATION: &str = "codespace-g1";

        /// DevGuard's fixture authority with a `codespace` control-service consumer, as an
        /// operator provisions CodeSpace, beside its bootstrap `dev-cli` workload consumer.
        pub(crate) struct Provisioned {
            _directory: tempfile::TempDir,
            pub(crate) authority: TestAuthority,
            pub(crate) settings: OwnerSettings,
            pub(crate) credential: PathBuf,
        }

        pub(crate) fn provision(max_instances: u32) -> Provisioned {
            let directory = short_directory();
            let paths = AuthorityPaths::fixture(directory.path());
            let credential = paths.credentials().join("codespace.secret");
            let secret = fresh_secret();
            let consumer: ConsumerConfig = serde_json::from_value(serde_json::json!({
                "generation": GENERATION,
                "role": "control_service",
                "credential_sha256": contract::digest_bytes(secret.as_bytes()),
                "max_instances": max_instances,
                "control_reservation": {"cpu_milli": 10, "memory_bytes": 16 << 20, "tasks": 1},
            }))
            .unwrap();
            let authority = TestAuthority::start_with(directory.path(), |config| {
                write_new_private(&credential, secret.as_bytes()).unwrap();
                config.consumers.insert(CONSUMER.into(), consumer);
            })
            .unwrap();
            Provisioned {
                settings: OwnerSettings {
                    socket: authority.socket(),
                    consumer: CONSUMER.into(),
                    generation: GENERATION.into(),
                },
                _directory: directory,
                authority,
                credential,
            }
        }

        fn fresh_secret() -> String {
            let mut bytes = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .unwrap()
                .read_exact(&mut bytes)
                .unwrap();
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        }

        impl Provisioned {
            pub(crate) fn owner(&self) -> Owner {
                Owner::new(
                    self.settings.clone(),
                    OwnerCredential::File(self.credential.clone()),
                )
            }

            /// `codespace`'s registered instances: identity and process ID. (DevGuard marks an
            /// instance suspect once it sees its session close, which can be after the session
            /// returned here, so whether it is active is not compared.)
            pub(super) fn instances(&self) -> Vec<(String, u32)> {
                let mut instances: Vec<_> = self
                    .authority
                    .instances()
                    .unwrap()
                    .into_iter()
                    .filter(|record| record.consumer_id == CONSUMER)
                    .map(|record| (record.instance.instance_id, record.instance.process.pid))
                    .collect();
                instances.sort();
                instances
            }
        }

        #[test]
        fn the_owner_registers_itself_as_one_instance_across_sessions() {
            let provisioned = provision(2);
            let owner = provisioned.owner();
            let me = std::process::id();
            for _ in 0..3 {
                let registration = registered(&owner);
                assert_eq!(
                    (
                        registration.state,
                        registration.error_code,
                        registration.pid
                    ),
                    (RegistrationState::Registered, None, Some(me))
                );
                let report = registration.status.report.unwrap();
                assert_eq!(report.role, Role::ControlService);
                assert!(report.registration_ready);
                assert!(report
                    .capabilities
                    .contains(&Capability::StaticControlReservations));
                // One instance, and it is this process.
                assert_eq!(
                    provisioned.instances(),
                    [(owner.instance_id().to_owned(), me)]
                );
            }
        }

        #[test]
        fn concurrent_sessions_of_one_owner_register_one_instance() {
            let provisioned = provision(2);
            let owner = Arc::new(provisioned.owner());
            let sessions: Vec<_> = (0..8)
                .map(|_| {
                    let owner = owner.clone();
                    std::thread::spawn(move || registered(&owner))
                })
                .collect();
            for session in sessions {
                let registration = session.join().unwrap();
                assert_eq!(
                    (registration.state, registration.pid),
                    (RegistrationState::Registered, Some(std::process::id()))
                );
            }
            assert_eq!(
                provisioned.instances(),
                [(owner.instance_id().to_owned(), std::process::id())]
            );
        }

        #[test]
        fn a_workload_consumer_is_not_registered() {
            let provisioned = provision(2);
            let workload = Owner::new(
                OwnerSettings {
                    consumer: devguard_daemon::fixture::CONSUMER.into(),
                    generation: provisioned.authority.generation(),
                    ..provisioned.settings.clone()
                },
                OwnerCredential::File(provisioned.authority.paths().cli_credential()),
            );
            let registration = registered(&workload);
            assert_eq!(
                (registration.state, registration.error_code),
                (RegistrationState::RoleMismatch, None)
            );
            assert!(provisioned.authority.instances().unwrap().is_empty());
        }

        #[test]
        fn this_process_cannot_register_as_another_owner() {
            let provisioned = provision(2);
            // As if this process registered on behalf of another one: DevGuard registers the
            // session's peer, which is this process, not the expected owner.
            let registration = registered_with(
                &provisioned.owner(),
                effective_uid(),
                registration(),
                std::process::id() + 1,
            );
            assert_eq!(registration.state, RegistrationState::OwnerMismatch);
        }

        const CHILD: &str = "CODESPACE_DEVGUARD_REGISTRATION_CHILD";

        /// Register in a separate process, under `instance_id` or its own new identity.
        fn register_elsewhere(
            provisioned: &Provisioned,
            instance_id: Option<&str>,
        ) -> (String, u32) {
            let spec = serde_json::json!({
                "socket": provisioned.settings.socket,
                "consumer": provisioned.settings.consumer,
                "generation": provisioned.settings.generation,
                "credential": provisioned.credential,
                "instance_id": instance_id,
            });
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "registration::tests::native::registration_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, spec.to_string())
                .output()
                .unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(output.status.success(), "{stdout}");
            let line = stdout
                .lines()
                .find_map(|line| line.split_once("child-registration ").map(|(_, rest)| rest))
                .unwrap_or_else(|| panic!("{stdout}"));
            let (outcome, pid) = line.rsplit_once(' ').unwrap();
            (outcome.to_owned(), pid.parse().unwrap())
        }

        #[test]
        #[ignore = "run by its parent test as a separate process"]
        fn registration_child() {
            let spec: serde_json::Value =
                serde_json::from_str(&std::env::var(CHILD).unwrap()).unwrap();
            let text = |key: &str| spec[key].as_str().unwrap().to_owned();
            let settings = OwnerSettings {
                socket: text("socket").into(),
                consumer: text("consumer"),
                generation: text("generation"),
            };
            let credential = OwnerCredential::File(text("credential").into());
            let owner = match spec["instance_id"].as_str() {
                Some(id) => Owner::with_instance_id(settings, credential, id.to_owned()),
                None => Owner::new(settings, credential),
            };
            let registration = registered(&owner);
            println!(
                "child-registration {:?} {:?} {:?} {}",
                registration.state,
                registration.error_code,
                registration.pid,
                std::process::id()
            );
        }

        #[test]
        fn each_process_registers_only_its_own_identity() {
            let provisioned = provision(2);
            let owner = provisioned.owner();
            assert_eq!(registered(&owner).state, RegistrationState::Registered);
            // Another process registers itself as a second instance...
            let (outcome, child) = register_elsewhere(&provisioned, None);
            assert_eq!(outcome, format!("Registered None Some({child})"));
            assert_ne!(child, std::process::id());
            let instances = provisioned.instances();
            assert_eq!(instances.len(), 2, "{instances:?}");
            assert!(instances.contains(&(owner.instance_id().to_owned(), std::process::id())));
            assert!(instances.iter().any(|(_, pid)| *pid == child));
            // ...but cannot take this owner's instance identity.
            let (outcome, _) = register_elsewhere(&provisioned, Some(owner.instance_id()));
            assert_eq!(outcome, "Refused Some(AttemptConflict) None");
            assert_eq!(provisioned.instances(), instances);
            // This owner keeps registering as itself.
            assert_eq!(registered(&owner).state, RegistrationState::Registered);
        }

        #[test]
        fn a_full_instance_pool_refuses_another_owner() {
            let provisioned = provision(1);
            let owner = provisioned.owner();
            assert_eq!(registered(&owner).state, RegistrationState::Registered);
            // The pool's one slot is held by this process, which is alive, so reconciling it
            // frees nothing.
            let (outcome, _) = register_elsewhere(&provisioned, None);
            assert_eq!(outcome, "Refused Some(ResourceUnavailable) None");
            assert_eq!(
                provisioned.instances(),
                [(owner.instance_id().to_owned(), std::process::id())]
            );
        }
    }
}
