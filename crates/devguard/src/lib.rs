//! Opt-in, status-only DevGuard adapter (CSRG-U1).
//!
//! [`probe`] opens one bounded session to a DevGuard authority through DevGuard's generic
//! client: connect, `Hello`, `Authenticate` as an operator-provisioned consumer, then `Status`,
//! and closes it. It registers, admits and launches nothing, and CodeSpace's execution paths
//! never call it. Every outcome is a [`Status`], never an error for the caller.
//!
//! DevGuard's transport bounds the session: connecting and each frame read or write have a
//! 250 ms deadline, so a probe ends within about 1.75 s.
//!
//! The consumer secret is read from its private file for each probe, held in DevGuard's
//! redacting `Secret` and sent only in the `Authenticate` frame. It never enters a [`Status`],
//! the environment or an argument, and this crate logs nothing. The file is opened
//! close-on-exec and closed before the probe returns. The session socket comes from DevGuard's
//! `connect_timeout`, which macOS cannot create close-on-exec atomically; CodeSpace's spawners
//! keep it out of their children (#79).

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use devguard_client::protocol::{CallerCredential, SessionRole};
use devguard_client::Client;
use devguard_contract::{Capability, Compatibility, Error, ErrorCode, Secret, PROTOCOL_VERSION};

/// Operator settings for one DevGuard consumer. None of them is a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The authority's socket. DevGuard's own is `/private/tmp/devguard-<uid>/authority.sock`.
    pub socket: PathBuf,
    /// The consumer's id in DevGuard's operator configuration.
    pub consumer: String,
    /// The consumer's generation in DevGuard's operator configuration.
    pub generation: String,
    /// A private regular file of this user, with one link, holding exactly the consumer's
    /// 64-character secret, as DevGuard writes its own credential files.
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

/// The authority's own report, present when the state is [`State::Available`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub protocol: u32,
    /// DevGuard's capability names, as on its wire.
    pub capabilities: Vec<&'static str>,
    /// The session role DevGuard granted the consumer, as on its wire.
    pub role: &'static str,
    pub storage_validated: bool,
    pub registration_ready: bool,
    pub execution_ready: bool,
    /// DevGuard's own explanation of its readiness.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    /// DevGuard's code for the step that failed, as on its wire; its message is not kept.
    /// `None` when available, and when the credential could not be used locally.
    pub error_code: Option<&'static str>,
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
        report: Some(Report {
            protocol: client.hello.protocol,
            capabilities: client
                .hello
                .capabilities
                .iter()
                .copied()
                .map(capability_name)
                .collect(),
            role: role_name(role),
            storage_validated: status.storage_validated,
            registration_ready: status.registration_ready,
            execution_ready: status.execution_ready,
            reason: status.reason,
        }),
    }
}

fn failed(step: Step, error: &Error) -> Status {
    Status {
        state: classify(step, error.code),
        error_code: (step != Step::Credential).then(|| code_name(error.code)),
        report: None,
    }
}

/// The step and DevGuard's code decide the state; the message never does.
fn classify(step: Step, code: ErrorCode) -> State {
    match (step, code) {
        (Step::Credential, _) => State::CredentialUnavailable,
        // The client refuses a peer UID, a declared identity or protocol bounds that differ.
        (Step::Connect, ErrorCode::Unauthorized) => State::UntrustedAuthority,
        (Step::Connect, ErrorCode::ResourcePolicyUnsupported) => State::Incompatible,
        (Step::Authenticate, ErrorCode::Unauthorized) => State::CredentialRefused,
        _ => State::Unavailable,
    }
}

fn credential(settings: &Settings) -> Result<CallerCredential, Error> {
    devguard_contract::validate_id(&settings.consumer)?;
    devguard_contract::validate_id(&settings.generation)?;
    Ok(CallerCredential::Consumer {
        consumer_id: settings.consumer.clone(),
        generation: settings.generation.clone(),
        secret: read_secret(&settings.credential_file)?,
    })
}

/// DevGuard's reader takes exactly 64 bytes within its deadline and closes the descriptor.
fn read_secret(path: &Path) -> Result<Secret, Error> {
    devguard_client::credential::read_owned(open_private(path)?)
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
        ErrorCode::InvalidRequest,
        "the credential file is not a private file of this user",
    )
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

fn code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::AttemptConflict => "attempt_conflict",
        ErrorCode::ResourceUnavailable => "resource_unavailable",
        ErrorCode::ResourceControlUnavailable => "resource_control_unavailable",
        ErrorCode::ResourcePolicyUnsupported => "resource_policy_unsupported",
        ErrorCode::InvalidTransition => "invalid_transition",
        ErrorCode::NotFound => "not_found",
        ErrorCode::ReconciliationRequired => "reconciliation_required",
        ErrorCode::JournalInvalid => "journal_invalid",
    }
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::DurableAdmission => "durable_admission",
        Capability::FencedLaunch => "fenced_launch",
        Capability::PerResourceEvidence => "per_resource_evidence",
        Capability::StaticControlReservations => "static_control_reservations",
        Capability::MacosCooperative => "macos_cooperative",
        Capability::LinuxCgroupV2 => "linux_cgroup_v2",
        Capability::ParentLease => "parent_lease",
        Capability::UpgradeDrain => "upgrade_drain",
    }
}

fn role_name(role: SessionRole) -> &'static str {
    match role {
        SessionRole::Workload => "workload",
        SessionRole::ControlService => "control_service",
        SessionRole::Administrator => "administrator",
    }
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

    const CODES: [ErrorCode; 10] = [
        ErrorCode::Unauthorized,
        ErrorCode::InvalidRequest,
        ErrorCode::AttemptConflict,
        ErrorCode::ResourceUnavailable,
        ErrorCode::ResourceControlUnavailable,
        ErrorCode::ResourcePolicyUnsupported,
        ErrorCode::InvalidTransition,
        ErrorCode::NotFound,
        ErrorCode::ReconciliationRequired,
        ErrorCode::JournalInvalid,
    ];

    /// A DevGuard authority on fixture paths, served as DevGuard's own server tests serve
    /// one: with native host evidence on macOS, and elsewhere without it, so registration and
    /// execution stay closed while `Hello`, `Authenticate` and `Status` are served.
    struct Authority {
        directory: tempfile::TempDir,
        paths: AuthorityPaths,
        generation: String,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<devguard_contract::Result<()>>>,
    }

    impl Authority {
        fn start() -> Self {
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
        fn settings(&self) -> Settings {
            Settings {
                socket: self.paths.socket(),
                consumer: "dev-cli".into(),
                generation: self.generation.clone(),
                credential_file: self.paths.cli_credential(),
            }
        }

        fn secret(&self) -> String {
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
    fn short_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("cs-dg-")
            .tempdir_in(if cfg!(target_os = "macos") {
                "/private/tmp"
            } else {
                "/tmp"
            })
            .unwrap()
    }

    fn private_file(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
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

    #[test]
    fn available_reports_the_authority_status() {
        let authority = Authority::start();
        let status = probe(&authority.settings());
        assert_eq!(
            (status.state, status.error_code),
            (State::Available, None),
            "{status:?}"
        );
        let report = status.report.unwrap();
        assert_eq!(report.protocol, PROTOCOL_VERSION);
        assert_eq!(report.role, "workload");
        assert!(report.storage_validated);
        assert!(!report.reason.is_empty());
        // Registration and execution open only with native host evidence.
        let native = cfg!(target_os = "macos");
        assert_eq!(
            (report.registration_ready, report.execution_ready),
            (native, native)
        );
        assert_eq!(report.capabilities.contains(&"durable_admission"), native);
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
            let status = probe(&settings);
            assert_eq!(
                (status.state, status.error_code, status.report),
                (
                    State::Unavailable,
                    Some("resource_control_unavailable"),
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
        let status = probe(&settings);
        assert_eq!(status.state, State::Unavailable);
        assert!(started.elapsed() < Duration::from_millis(1750));
        drop(holder.join().unwrap());
    }

    #[test]
    fn another_uid_is_an_untrusted_authority() {
        let authority = Authority::start();
        let status = probe_with(
            &authority.settings(),
            effective_uid().wrapping_add(1),
            status_only(),
        );
        assert_eq!(
            (status.state, status.error_code),
            (State::UntrustedAuthority, Some("unauthorized"))
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
            required: BTreeSet::from([Capability::LinuxCgroupV2]),
            ..status_only()
        };
        for compatibility in [protocol, capability] {
            let status = probe_with(&authority.settings(), effective_uid(), compatibility);
            assert_eq!(
                (status.state, status.error_code),
                (State::Incompatible, Some("resource_policy_unsupported"))
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
            let status = probe(&settings);
            assert_eq!(
                (status.state, status.error_code),
                (State::CredentialRefused, Some("unauthorized")),
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
        ];
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
            probe(&settings),
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
                ErrorCode::Unauthorized => State::UntrustedAuthority,
                ErrorCode::ResourcePolicyUnsupported => State::Incompatible,
                _ => State::Unavailable,
            };
            assert_eq!(classify(Step::Connect, code), connect);
            let authenticate = match code {
                ErrorCode::Unauthorized => State::CredentialRefused,
                _ => State::Unavailable,
            };
            assert_eq!(classify(Step::Authenticate, code), authenticate);
            assert_eq!(classify(Step::Status, code), State::Unavailable);
            let error = Error::new(code, "a message that is not kept");
            assert_eq!(failed(Step::Credential, &error).error_code, None);
            assert_eq!(
                failed(Step::Status, &error).error_code,
                Some(code_name(code))
            );
        }
    }

    #[test]
    fn names_are_devguard_wire_names() {
        let wire = |value: serde_json::Value| value.as_str().unwrap().to_owned();
        for code in CODES {
            assert_eq!(wire(serde_json::to_value(code).unwrap()), code_name(code));
        }
        for capability in [
            Capability::DurableAdmission,
            Capability::FencedLaunch,
            Capability::PerResourceEvidence,
            Capability::StaticControlReservations,
            Capability::MacosCooperative,
            Capability::LinuxCgroupV2,
            Capability::ParentLease,
            Capability::UpgradeDrain,
        ] {
            assert_eq!(
                wire(serde_json::to_value(capability).unwrap()),
                capability_name(capability)
            );
        }
        for role in [
            SessionRole::Workload,
            SessionRole::ControlService,
            SessionRole::Administrator,
        ] {
            assert_eq!(wire(serde_json::to_value(role).unwrap()), role_name(role));
        }
    }
}
