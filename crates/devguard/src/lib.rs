//! Opt-in DevGuard adapter: bounded sessions through DevGuard's generic client.
//!
//! [`probe`] opens one bounded session to a DevGuard authority through DevGuard's generic
//! client: connect, `Hello`, `Authenticate` as an operator-provisioned consumer, then `Status`,
//! and closes it (CSRG-U1). [`Owner::register`] registers the process that owns CodeSpace's
//! executions in such a session (CSRG-U2), and [`handoff`] hands the consumer secret to the UDS
//! worker that owns them. Nothing here admits or launches anything. Every outcome is a
//! [`Status`] or a [`Registration`], never an error for the caller.
//!
//! DevGuard's transport bounds the session: connecting and each frame read or write have a
//! 250 ms deadline, so a probe ends within about 1.75 s and a registration within about 2.25 s.
//!
//! A [`Status`] or [`Registration`] holds only this crate's own enumerations, flags, process
//! IDs and DevGuard's protocol number. DevGuard's messages and its free-text readiness reason
//! are not kept.
//!
//! The consumer secret is read from its private file for each session, or handed to the worker
//! once, held in DevGuard's redacting `Secret` and sent only in the `Authenticate` frame. It
//! never enters a [`Status`], the environment or an argument, and this crate logs nothing. The
//! file is opened close-on-exec and closed before the session ends. The session socket comes
//! from DevGuard's `connect_timeout`, which macOS cannot create close-on-exec atomically;
//! CodeSpace's spawners keep it out of their children (#79).

pub mod handoff;
mod registration;

pub use registration::{Owner, OwnerCredential, OwnerSettings, Registration, RegistrationState};

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use devguard_client::protocol::{CallerCredential, ServiceStatus, SessionRole};
use devguard_client::Client;
use devguard_contract as contract;
use devguard_contract::{Compatibility, Error, Secret, PROTOCOL_VERSION};

/// Operator settings for one DevGuard consumer. None of them is a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The authority's socket. DevGuard's own is `/private/tmp/devguard-<uid>/authority.sock`.
    pub socket: PathBuf,
    /// The consumer's id in DevGuard's operator configuration.
    pub consumer: String,
    /// The consumer's generation in DevGuard's operator configuration.
    pub generation: String,
    /// The absolute path of a private regular file of this user, with one link, holding
    /// exactly the consumer's 64-character secret, as DevGuard writes its own credential
    /// files. Every directory on the path must be a real directory, owned by this user or root
    /// and not writable by group or others, apart from a root-owned sticky `/tmp`; the file's
    /// own directory must be this user's.
    pub credential_file: PathBuf,
}

/// What a probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The session authenticated and the authority reported its status.
    Available,
    /// No authority answered at the socket, or a response was missing or invalid.
    Unavailable,
    /// The socket's peer, or the identity it declared, is not the expected authority.
    UntrustedAuthority,
    /// The authority offers no protocol or capability set this adapter accepts.
    Incompatible,
    /// The authority refused the consumer credential.
    CredentialRefused,
    /// The consumer settings or secret could not be used, so no session was opened.
    CredentialUnavailable,
}

/// DevGuard's error codes, without their messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Unauthorized,
    InvalidRequest,
    AttemptConflict,
    ResourceUnavailable,
    ResourceControlUnavailable,
    ResourcePolicyUnsupported,
    InvalidTransition,
    NotFound,
    ReconciliationRequired,
    JournalInvalid,
}

impl ErrorCode {
    pub const ALL: [Self; 10] = [
        Self::Unauthorized,
        Self::InvalidRequest,
        Self::AttemptConflict,
        Self::ResourceUnavailable,
        Self::ResourceControlUnavailable,
        Self::ResourcePolicyUnsupported,
        Self::InvalidTransition,
        Self::NotFound,
        Self::ReconciliationRequired,
        Self::JournalInvalid,
    ];

    /// The code's name on DevGuard's wire.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::InvalidRequest => "invalid_request",
            Self::AttemptConflict => "attempt_conflict",
            Self::ResourceUnavailable => "resource_unavailable",
            Self::ResourceControlUnavailable => "resource_control_unavailable",
            Self::ResourcePolicyUnsupported => "resource_policy_unsupported",
            Self::InvalidTransition => "invalid_transition",
            Self::NotFound => "not_found",
            Self::ReconciliationRequired => "reconciliation_required",
            Self::JournalInvalid => "journal_invalid",
        }
    }
}

impl From<contract::ErrorCode> for ErrorCode {
    fn from(code: contract::ErrorCode) -> Self {
        match code {
            contract::ErrorCode::Unauthorized => Self::Unauthorized,
            contract::ErrorCode::InvalidRequest => Self::InvalidRequest,
            contract::ErrorCode::AttemptConflict => Self::AttemptConflict,
            contract::ErrorCode::ResourceUnavailable => Self::ResourceUnavailable,
            contract::ErrorCode::ResourceControlUnavailable => Self::ResourceControlUnavailable,
            contract::ErrorCode::ResourcePolicyUnsupported => Self::ResourcePolicyUnsupported,
            contract::ErrorCode::InvalidTransition => Self::InvalidTransition,
            contract::ErrorCode::NotFound => Self::NotFound,
            contract::ErrorCode::ReconciliationRequired => Self::ReconciliationRequired,
            contract::ErrorCode::JournalInvalid => Self::JournalInvalid,
        }
    }
}

/// The capabilities a DevGuard authority states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    DurableAdmission,
    FencedLaunch,
    PerResourceEvidence,
    StaticControlReservations,
    MacosCooperative,
    LinuxCgroupV2,
    ParentLease,
    UpgradeDrain,
}

impl Capability {
    pub const ALL: [Self; 8] = [
        Self::DurableAdmission,
        Self::FencedLaunch,
        Self::PerResourceEvidence,
        Self::StaticControlReservations,
        Self::MacosCooperative,
        Self::LinuxCgroupV2,
        Self::ParentLease,
        Self::UpgradeDrain,
    ];

    /// The capability's name on DevGuard's wire.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::DurableAdmission => "durable_admission",
            Self::FencedLaunch => "fenced_launch",
            Self::PerResourceEvidence => "per_resource_evidence",
            Self::StaticControlReservations => "static_control_reservations",
            Self::MacosCooperative => "macos_cooperative",
            Self::LinuxCgroupV2 => "linux_cgroup_v2",
            Self::ParentLease => "parent_lease",
            Self::UpgradeDrain => "upgrade_drain",
        }
    }
}

impl From<contract::Capability> for Capability {
    fn from(capability: contract::Capability) -> Self {
        match capability {
            contract::Capability::DurableAdmission => Self::DurableAdmission,
            contract::Capability::FencedLaunch => Self::FencedLaunch,
            contract::Capability::PerResourceEvidence => Self::PerResourceEvidence,
            contract::Capability::StaticControlReservations => Self::StaticControlReservations,
            contract::Capability::MacosCooperative => Self::MacosCooperative,
            contract::Capability::LinuxCgroupV2 => Self::LinuxCgroupV2,
            contract::Capability::ParentLease => Self::ParentLease,
            contract::Capability::UpgradeDrain => Self::UpgradeDrain,
        }
    }
}

/// The session role DevGuard granted the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Workload,
    ControlService,
    Administrator,
}

impl Role {
    pub const ALL: [Self; 3] = [Self::Workload, Self::ControlService, Self::Administrator];

    /// The role's name on DevGuard's wire.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Workload => "workload",
            Self::ControlService => "control_service",
            Self::Administrator => "administrator",
        }
    }
}

impl From<SessionRole> for Role {
    fn from(role: SessionRole) -> Self {
        match role {
            SessionRole::Workload => Self::Workload,
            SessionRole::ControlService => Self::ControlService,
            SessionRole::Administrator => Self::Administrator,
        }
    }
}

/// The authority's own report, present when the state is [`State::Available`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub protocol: u32,
    pub capabilities: Vec<Capability>,
    pub role: Role,
    pub storage_validated: bool,
    pub registration_ready: bool,
    pub execution_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    /// DevGuard's code for the step that failed; its message is not kept. `None` when
    /// available, and when the credential could not be used locally.
    pub error_code: Option<ErrorCode>,
    pub report: Option<Report>,
}

/// One bounded, status-only session as this user's consumer.
pub fn probe(settings: &Settings) -> Status {
    probe_with(settings, effective_uid(), status_only())
}

/// Status needs no capability, and this adapter speaks DevGuard's protocol 1 only.
fn status_only() -> Compatibility {
    Compatibility {
        minimum_protocol: PROTOCOL_VERSION,
        maximum_protocol: PROTOCOL_VERSION,
        required: BTreeSet::new(),
    }
}

/// Where a probe stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Credential,
    Connect,
    Authenticate,
    Status,
}

fn probe_with(settings: &Settings, authority_uid: u32, compatibility: Compatibility) -> Status {
    // The credential is read first, so a local problem opens no session.
    let credential = match credential(settings) {
        Ok(credential) => credential,
        Err(error) => return failed(Step::Credential, &error),
    };
    let mut client = match Client::connect(&settings.socket, authority_uid, compatibility) {
        Ok(client) => client,
        Err(error) => return failed(Step::Connect, &error),
    };
    let role = match client.authenticate(credential) {
        Ok(role) => role,
        Err(error) => return failed(Step::Authenticate, &error),
    };
    let status = match client.status() {
        Ok(status) => status,
        Err(error) => return failed(Step::Status, &error),
    };
    Status {
        state: State::Available,
        error_code: None,
        report: Some(report(&client, role, status)),
    }
}

/// The authority's report from its `Hello` and `Status`, without the free-text reason.
fn report(client: &Client, role: SessionRole, status: ServiceStatus) -> Report {
    Report {
        protocol: client.hello.protocol,
        capabilities: client
            .hello
            .capabilities
            .iter()
            .copied()
            .map(Capability::from)
            .collect(),
        role: role.into(),
        storage_validated: status.storage_validated,
        registration_ready: status.registration_ready,
        execution_ready: status.execution_ready,
    }
}

fn failed(step: Step, error: &Error) -> Status {
    Status {
        state: classify(step, error.code),
        error_code: (step != Step::Credential).then(|| error.code.into()),
        report: None,
    }
}

/// The step and DevGuard's code decide the state; the message never does.
fn classify(step: Step, code: contract::ErrorCode) -> State {
    match (step, code) {
        (Step::Credential, _) => State::CredentialUnavailable,
        // The client refuses a peer UID, a declared identity or protocol bounds that differ.
        (Step::Connect, contract::ErrorCode::Unauthorized) => State::UntrustedAuthority,
        (Step::Connect, contract::ErrorCode::ResourcePolicyUnsupported) => State::Incompatible,
        (Step::Authenticate, contract::ErrorCode::Unauthorized) => State::CredentialRefused,
        _ => State::Unavailable,
    }
}

fn credential(settings: &Settings) -> Result<CallerCredential, Error> {
    contract::validate_id(&settings.consumer)?;
    contract::validate_id(&settings.generation)?;
    Ok(CallerCredential::Consumer {
        consumer_id: settings.consumer.clone(),
        generation: settings.generation.clone(),
        secret: read_secret(&settings.credential_file)?,
    })
}

/// DevGuard's reader takes exactly 64 bytes within its deadline and closes the descriptor.
fn read_secret(path: &Path) -> Result<Secret, Error> {
    secure_directories(path)?;
    devguard_client::credential::read_owned(open_private(path)?)
}

/// Only this user or root can change which file the path names, as DevGuard requires of its
/// own credential directory. The path is absolute, without `..`. Every directory on it is a
/// real directory, owned by this user or root and not writable by group or others, except a
/// root-owned sticky `/tmp`; the file's own directory belongs to this user.
fn secure_directories(path: &Path) -> Result<(), Error> {
    let (true, Some(parent), Some(_)) = (path.is_absolute(), path.parent(), path.file_name())
    else {
        return Err(not_private());
    };
    let uid = effective_uid();
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir => current.push("/"),
            Component::Normal(name) => current.push(name),
            _ => return Err(not_private()),
        }
        let meta = std::fs::symlink_metadata(&current).map_err(|_| not_private())?;
        let shared_tmp = matches!(current.to_str(), Some("/tmp" | "/private/tmp"))
            && meta.uid() == 0
            && meta.mode() & 0o1000 != 0;
        if !meta.is_dir()
            || (meta.uid() != uid && meta.uid() != 0)
            || (!shared_tmp && meta.mode() & 0o022 != 0)
            || (current == parent && meta.uid() != uid)
        {
            return Err(not_private());
        }
    }
    Ok(())
}

/// Open a private regular file of this user with one link, without following a link and
/// close-on-exec. `O_NONBLOCK` keeps a FIFO in its place from blocking the open.
fn open_private(path: &Path) -> Result<OwnedFd, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| not_private())?;
    let meta = file.metadata().map_err(|_| not_private())?;
    if !meta.is_file()
        || meta.uid() != effective_uid()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(not_private());
    }
    Ok(file.into())
}

fn not_private() -> Error {
    Error::new(
        contract::ErrorCode::InvalidRequest,
        "the credential file is not a private file of this user",
    )
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devguard_daemon::config;
    use devguard_daemon::paths::AuthorityPaths;
    use devguard_daemon::server::Server;
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    /// DevGuard's own codes, capabilities and roles at the pin.
    pub(crate) const CODES: [contract::ErrorCode; 10] = [
        contract::ErrorCode::Unauthorized,
        contract::ErrorCode::InvalidRequest,
        contract::ErrorCode::AttemptConflict,
        contract::ErrorCode::ResourceUnavailable,
        contract::ErrorCode::ResourceControlUnavailable,
        contract::ErrorCode::ResourcePolicyUnsupported,
        contract::ErrorCode::InvalidTransition,
        contract::ErrorCode::NotFound,
        contract::ErrorCode::ReconciliationRequired,
        contract::ErrorCode::JournalInvalid,
    ];
    const CAPABILITIES: [contract::Capability; 8] = [
        contract::Capability::DurableAdmission,
        contract::Capability::FencedLaunch,
        contract::Capability::PerResourceEvidence,
        contract::Capability::StaticControlReservations,
        contract::Capability::MacosCooperative,
        contract::Capability::LinuxCgroupV2,
        contract::Capability::ParentLease,
        contract::Capability::UpgradeDrain,
    ];
    const ROLES: [SessionRole; 3] = [
        SessionRole::Workload,
        SessionRole::ControlService,
        SessionRole::Administrator,
    ];

    /// A DevGuard authority on fixture paths, served as DevGuard's own server tests serve
    /// one: with native host evidence on macOS, and elsewhere without it, so registration and
    /// execution stay closed while `Hello`, `Authenticate` and `Status` are served.
    pub(crate) struct Authority {
        pub(crate) directory: tempfile::TempDir,
        pub(crate) paths: AuthorityPaths,
        generation: String,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<devguard_contract::Result<()>>>,
    }

    impl Authority {
        pub(crate) fn start() -> Self {
            let directory = short_directory();
            let paths = AuthorityPaths::fixture(directory.path());
            let config = config::initialize(&paths).unwrap();
            let server = Server::open(&paths).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let signal = stop.clone();
            let worker = Some(std::thread::spawn(move || server.run(signal)));
            Self {
                generation: config.consumers["dev-cli"].generation.clone(),
                directory,
                paths,
                stop,
                worker,
            }
        }

        /// The bootstrap workload consumer, read from the credential file DevGuard wrote.
        pub(crate) fn settings(&self) -> Settings {
            Settings {
                socket: self.paths.socket(),
                consumer: "dev-cli".into(),
                generation: self.generation.clone(),
                credential_file: self.paths.cli_credential(),
            }
        }

        pub(crate) fn secret(&self) -> String {
            std::fs::read_to_string(self.paths.cli_credential()).unwrap()
        }

        fn stop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap().unwrap();
            }
        }
    }

    impl Drop for Authority {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Short enough for a Unix socket path, and private as DevGuard requires.
    pub(crate) fn short_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("cs-dg-")
            .tempdir_in(if cfg!(target_os = "macos") {
                "/private/tmp"
            } else {
                "/tmp"
            })
            .unwrap()
    }

    pub(crate) fn private_file(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(contents).unwrap();
        path
    }

    /// `session` again while it stops at the credential, up to five times. DevGuard reads the
    /// consumer secret within 250 ms of wall time, so a test thread the scheduler holds that
    /// long gets a credential failure although the file is intact; such a session opens nothing.
    /// For sessions whose credential is intact and not what the test checks.
    pub(crate) fn past_the_credential<T>(
        stopped: impl Fn(&T) -> bool,
        mut session: impl FnMut() -> T,
    ) -> T {
        for _ in 1..5 {
            let outcome = session();
            if !stopped(&outcome) {
                return outcome;
            }
        }
        session()
    }

    fn probed(settings: &Settings) -> Status {
        probed_with(settings, effective_uid(), status_only())
    }

    fn probed_with(
        settings: &Settings,
        authority_uid: u32,
        compatibility: Compatibility,
    ) -> Status {
        past_the_credential(
            |status: &Status| status.state == State::CredentialUnavailable,
            || probe_with(settings, authority_uid, compatibility.clone()),
        )
    }

    #[test]
    fn available_reports_the_authority_status() {
        let authority = Authority::start();
        let status = probed(&authority.settings());
        assert_eq!(
            (status.state, status.error_code),
            (State::Available, None),
            "{status:?}"
        );
        let report = status.report.unwrap();
        assert_eq!(report.protocol, PROTOCOL_VERSION);
        assert_eq!(report.role, Role::Workload);
        assert!(report.storage_validated);
        // Registration and execution open only with native host evidence.
        let native = cfg!(target_os = "macos");
        assert_eq!(
            (report.registration_ready, report.execution_ready),
            (native, native)
        );
        assert_eq!(
            report.capabilities.contains(&Capability::DurableAdmission),
            native
        );
        assert_eq!(report.capabilities.is_empty(), !native);
    }

    #[test]
    fn a_missing_or_stopped_authority_is_unavailable() {
        let mut authority = Authority::start();
        let mut missing = authority.settings();
        missing.socket = authority.directory.path().join("absent.sock");
        let settings = authority.settings();
        authority.stop();
        for settings in [missing, settings] {
            let status = probed(&settings);
            assert_eq!(
                (status.state, status.error_code, status.report),
                (
                    State::Unavailable,
                    Some(ErrorCode::ResourceControlUnavailable),
                    None
                ),
                "{settings:?}"
            );
        }
    }

    #[test]
    fn a_silent_endpoint_ends_the_probe_within_its_bounds() {
        let authority = Authority::start();
        let socket = authority.directory.path().join("silent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        // Accept and hold the connection, answering nothing.
        let holder = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));
        let mut settings = authority.settings();
        settings.socket = socket;
        let started = Instant::now();
        let status = probed(&settings);
        assert_eq!(status.state, State::Unavailable);
        assert!(started.elapsed() < Duration::from_millis(1750));
        drop(holder.join().unwrap());
    }

    #[test]
    fn another_uid_is_an_untrusted_authority() {
        let authority = Authority::start();
        let status = probed_with(
            &authority.settings(),
            effective_uid().wrapping_add(1),
            status_only(),
        );
        assert_eq!(
            (status.state, status.error_code),
            (State::UntrustedAuthority, Some(ErrorCode::Unauthorized))
        );
    }

    #[test]
    fn another_protocol_or_a_missing_capability_is_incompatible() {
        let authority = Authority::start();
        let protocol = Compatibility {
            minimum_protocol: PROTOCOL_VERSION + 1,
            maximum_protocol: PROTOCOL_VERSION + 1,
            required: BTreeSet::new(),
        };
        let capability = Compatibility {
            required: BTreeSet::from([contract::Capability::LinuxCgroupV2]),
            ..status_only()
        };
        for compatibility in [protocol, capability] {
            let status = probed_with(&authority.settings(), effective_uid(), compatibility);
            assert_eq!(
                (status.state, status.error_code),
                (
                    State::Incompatible,
                    Some(ErrorCode::ResourcePolicyUnsupported)
                )
            );
        }
    }

    #[test]
    fn a_refused_credential_is_reported_as_refused() {
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
            let status = probed(&settings);
            assert_eq!(
                (status.state, status.error_code),
                (State::CredentialRefused, Some(ErrorCode::Unauthorized)),
                "{settings:?}"
            );
        }
    }

    #[test]
    fn an_unusable_credential_opens_no_session() {
        let authority = Authority::start();
        let dir = authority.directory.path();
        let secret = authority.secret();
        // A listener stands in for the authority, so an attempted session would be seen.
        let socket = dir.join("watched.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();

        let shared = private_file(dir, "shared.secret", secret.as_bytes());
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o640)).unwrap();
        let linked = dir.join("linked.secret");
        std::os::unix::fs::symlink(authority.paths.cli_credential(), &linked).unwrap();
        let hard = dir.join("hard.secret");
        std::fs::hard_link(private_file(dir, "first.secret", secret.as_bytes()), &hard).unwrap();
        let fifo = dir.join("fifo.secret");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: mkfifo reads a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // A private file is not enough when a directory on its path lets someone else change
        // which file the path names.
        let directory = |name: &str, mode: u32| {
            let path = dir.join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            private_file(&path, "consumer.secret", secret.as_bytes())
        };
        let group_writable = directory("group", 0o770);
        let other_writable = directory("other", 0o703);
        let sticky = directory("sticky", 0o1777);
        let real = directory("real", 0o700);
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        let files = [
            dir.join("missing.secret"),
            shared,
            linked,
            hard,
            fifo,
            dir.to_path_buf(),
            private_file(dir, "short.secret", &secret.as_bytes()[..63]),
            private_file(dir, "newline.secret", format!("{secret}\n").as_bytes()),
            private_file(dir, "upper.secret", secret.to_uppercase().as_bytes()),
            group_writable,
            other_writable,
            sticky,
            dir.join("alias/consumer.secret"),
            dir.join("real/../real/consumer.secret"),
            PathBuf::from("consumer.secret"),
        ];
        // The same file through a sound path is accepted.
        assert!(read_secret(&real).is_ok());
        let mut cases: Vec<Settings> = files
            .into_iter()
            .map(|file| Settings {
                socket: socket.clone(),
                credential_file: file,
                ..authority.settings()
            })
            .collect();
        for id in ["", "has space", "a/b"] {
            cases.push(Settings {
                socket: socket.clone(),
                consumer: id.into(),
                ..authority.settings()
            });
            cases.push(Settings {
                socket: socket.clone(),
                generation: id.into(),
                ..authority.settings()
            });
        }
        for settings in cases {
            // A blocking open of the FIFO would never return; fail instead of hanging.
            let (sent, received) = std::sync::mpsc::channel();
            let probed = settings.clone();
            std::thread::spawn(move || sent.send(probe(&probed)));
            let status = received
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or_else(|_| panic!("the probe did not return for {settings:?}"));
            assert_eq!(
                (status.state, status.error_code, status.report),
                (State::CredentialUnavailable, None, None),
                "{settings:?}"
            );
            assert_eq!(
                listener.accept().map(|_| ()).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "a session was attempted for {settings:?}"
            );
        }
    }

    #[test]
    fn the_credential_is_opened_close_on_exec() {
        let authority = Authority::start();
        let fd = open_private(&authority.paths.cli_credential()).unwrap();
        // SAFETY: F_GETFD reads the flags of a descriptor this test owns.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    }

    #[test]
    fn no_status_or_setting_shows_the_secret() {
        let authority = Authority::start();
        let secret = authority.secret();
        let settings = authority.settings();
        let mut refused = authority.settings();
        refused.credential_file = private_file(
            authority.directory.path(),
            "other.secret",
            "1".repeat(64).as_bytes(),
        );
        let statuses = [
            probed(&settings),
            probe(&refused),
            probe_with(&settings, effective_uid().wrapping_add(1), status_only()),
        ];
        assert_eq!(statuses[0].state, State::Available);
        for shown in statuses
            .iter()
            .map(|status| format!("{status:?}"))
            .chain([format!("{settings:?}"), format!("{refused:?}")])
        {
            assert!(!shown.contains(&secret), "{shown}");
        }
    }

    #[test]
    fn the_step_and_code_decide_the_state() {
        for code in CODES {
            assert_eq!(
                classify(Step::Credential, code),
                State::CredentialUnavailable
            );
            let connect = match code {
                contract::ErrorCode::Unauthorized => State::UntrustedAuthority,
                contract::ErrorCode::ResourcePolicyUnsupported => State::Incompatible,
                _ => State::Unavailable,
            };
            assert_eq!(classify(Step::Connect, code), connect);
            let authenticate = match code {
                contract::ErrorCode::Unauthorized => State::CredentialRefused,
                _ => State::Unavailable,
            };
            assert_eq!(classify(Step::Authenticate, code), authenticate);
            assert_eq!(classify(Step::Status, code), State::Unavailable);
            let error = Error::new(code, "a message that is not kept");
            assert_eq!(failed(Step::Credential, &error).error_code, None);
            assert_eq!(
                failed(Step::Status, &error).error_code,
                Some(ErrorCode::from(code))
            );
        }
    }

    #[test]
    fn every_devguard_value_maps_to_one_value_with_its_wire_name() {
        let wire = |value: serde_json::Value| value.as_str().unwrap().to_owned();
        let codes: Vec<ErrorCode> = CODES.into_iter().map(ErrorCode::from).collect();
        assert_eq!(codes, ErrorCode::ALL);
        for code in CODES {
            assert_eq!(
                wire(serde_json::to_value(code).unwrap()),
                ErrorCode::from(code).wire_name()
            );
        }
        let capabilities: Vec<Capability> =
            CAPABILITIES.into_iter().map(Capability::from).collect();
        assert_eq!(capabilities, Capability::ALL);
        for capability in CAPABILITIES {
            assert_eq!(
                wire(serde_json::to_value(capability).unwrap()),
                Capability::from(capability).wire_name()
            );
        }
        let roles: Vec<Role> = ROLES.into_iter().map(Role::from).collect();
        assert_eq!(roles, Role::ALL);
        for role in ROLES {
            assert_eq!(
                wire(serde_json::to_value(role).unwrap()),
                Role::from(role).wire_name()
            );
        }
    }
}
