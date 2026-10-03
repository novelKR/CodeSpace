//! Opt-in DevGuard connection.
//!
//! With `--devguard status` (CSRG-U1), `workspace_info` reports the DevGuard authority's
//! status from one bounded session that `codespace-devguard` opens and closes. Probes run one
//! at a time: a call that arrives while one runs waits for the next, so every report comes from
//! a probe that started after the call arrived.
//!
//! With `--devguard register` (CSRG-U2), the process that owns CodeSpace's executions
//! registers itself with DevGuard and `workspace_info` reports that registration: the gateway
//! itself in InProcess mode, the worker it starts in UDS mode, which gets the consumer secret
//! through one private descriptor. The gateway never registers on the worker's behalf, and a
//! worker it did not start cannot register. Nothing is admitted or launched through DevGuard,
//! and no process query, termination or timeout waits for a registration.
//!
//! The consumer secret stays in its file until a session or the worker's start needs it. These
//! settings hold only its path, and only states, DevGuard's error codes and the owner's kind and
//! process ID are logged.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{Args, ValueEnum};
use codespace_devguard::{Settings, State, Status};
use codespace_domain::{
    ResourceAuthorityInfo, ResourceOwner, ResourceParticipation, ResourceRegistrationInfo,
    ResourceRegistrationState,
};
use codespace_runner::registration::{
    handoff, status_info, Owner, OwnerCredential, OwnerRegistration, OwnerSettings,
};
use codespace_runner::RuntimeBackend;
use tokio::sync::Mutex;

use crate::config::RunnerMode;

/// How CodeSpace takes part in DevGuard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum DevGuardMode {
    /// No DevGuard connection.
    #[default]
    Off,
    /// Report DevGuard's status in `workspace_info`; nothing is registered, admitted or launched.
    Status,
    /// The execution owner registers with DevGuard and `workspace_info` reports it; nothing is
    /// admitted or launched.
    Register,
}

/// DevGuard settings. None is a secret: the consumer secret stays in its file.
#[derive(Debug, Clone, Args)]
pub struct DevGuardArgs {
    /// DevGuard participation: `off` (default), `status` or `register`.
    #[arg(
        id = "devguard",
        long = "devguard",
        env = "CODESPACE_DEVGUARD",
        value_enum,
        default_value_t = DevGuardMode::Off
    )]
    pub mode: DevGuardMode,

    /// The authority's socket, normally /private/tmp/devguard-<uid>/authority.sock.
    #[arg(
        id = "devguard_socket",
        long = "devguard-socket",
        env = "CODESPACE_DEVGUARD_SOCKET",
        required_if_eq_any([("devguard", "status"), ("devguard", "register")])
    )]
    pub socket: Option<PathBuf>,

    /// The consumer's id in DevGuard's operator configuration.
    #[arg(
        id = "devguard_consumer",
        long = "devguard-consumer",
        env = "CODESPACE_DEVGUARD_CONSUMER",
        required_if_eq_any([("devguard", "status"), ("devguard", "register")])
    )]
    pub consumer: Option<String>,

    /// The consumer's generation in DevGuard's operator configuration.
    #[arg(
        id = "devguard_generation",
        long = "devguard-generation",
        env = "CODESPACE_DEVGUARD_GENERATION",
        required_if_eq_any([("devguard", "status"), ("devguard", "register")])
    )]
    pub generation: Option<String>,

    /// A private file (0600, one link) holding exactly the consumer's 64-character secret.
    #[arg(
        id = "devguard_credential_file",
        long = "devguard-credential-file",
        env = "CODESPACE_DEVGUARD_CREDENTIAL_FILE",
        required_if_eq_any([("devguard", "status"), ("devguard", "register")])
    )]
    pub credential_file: Option<PathBuf>,
}

impl DevGuardArgs {
    /// The adapter settings, unless participation is off.
    pub fn settings(&self) -> Option<Settings> {
        match self.mode {
            DevGuardMode::Off => None,
            DevGuardMode::Status | DevGuardMode::Register => Some(Settings {
                socket: self.socket.clone()?,
                consumer: self.consumer.clone()?,
                generation: self.generation.clone()?,
                credential_file: self.credential_file.clone()?,
            }),
        }
    }

    /// What a UDS worker the gateway starts needs to register itself, with `register`.
    pub fn worker(&self) -> Option<WorkerSettings> {
        match self.mode {
            DevGuardMode::Register => self.settings().map(|settings| WorkerSettings { settings }),
            DevGuardMode::Off | DevGuardMode::Status => None,
        }
    }
}

/// The DevGuard authority `workspace_info` reports, shared by every handler clone.
pub struct ResourceAuthority {
    settings: Settings,
    last: Mutex<Option<Probed>>,
}

struct Probed {
    started: Instant,
    info: ResourceAuthorityInfo,
}

impl ResourceAuthority {
    pub fn new(settings: Settings) -> Arc<Self> {
        Arc::new(Self {
            settings,
            last: Mutex::new(None),
        })
    }

    /// The status from a probe that started after this call arrived.
    pub async fn status(&self) -> ResourceAuthorityInfo {
        let arrived = Instant::now();
        let mut last = self.last.lock().await;
        if let Some(probed) = last.as_ref().filter(|probed| probed.started >= arrived) {
            return probed.info.clone();
        }
        let started = Instant::now();
        let settings = self.settings.clone();
        // A probe that cannot finish is reported as unavailable, never as a tool error.
        let status = tokio::task::spawn_blocking(move || codespace_devguard::probe(&settings))
            .await
            .unwrap_or(Status {
                state: State::Unavailable,
                error_code: None,
                report: None,
            });
        let info = status_info(status);
        if last.as_ref().map(|probed| probed.info.state) != Some(info.state) {
            tracing::info!(
                state = ?info.state,
                error_code = ?info.error_code,
                "DevGuard status"
            );
        }
        *last = Some(Probed {
            started,
            info: info.clone(),
        });
        info
    }
}

/// Which process owns CodeSpace's executions, and so registers (CSRG-U2).
pub(crate) enum ExecutionOwner {
    /// The gateway runs executions in its own process and registers itself.
    InProcess(Arc<OwnerRegistration>),
    /// The worker the gateway started owns them, registers itself and reports to the gateway.
    Worker,
    /// A worker the gateway did not start: nothing can hand it the consumer secret.
    UnstartedWorker,
}

impl ExecutionOwner {
    fn kind(&self) -> ResourceOwner {
        match self {
            Self::InProcess(_) => ResourceOwner::InProcess,
            Self::Worker | Self::UnstartedWorker => ResourceOwner::Worker,
        }
    }
}

/// The execution owner's registration as `workspace_info` reports it, shared by every handler
/// clone.
pub struct Registration {
    owner: ExecutionOwner,
    /// The gateway's own status probe, for when the owner cannot be asked.
    status: Arc<ResourceAuthority>,
    logged: std::sync::Mutex<Option<ResourceRegistrationInfo>>,
}

impl Registration {
    /// The registration for the gateway's runner mode. In InProcess mode the gateway reads
    /// the consumer secret from its file for each session, as a status probe does.
    pub fn new(settings: Settings, runner: RunnerMode, starts_worker: bool) -> Arc<Self> {
        let owner = match (runner, starts_worker) {
            (RunnerMode::InProcess, _) => ExecutionOwner::InProcess(OwnerRegistration::new(
                Owner::new(
                    OwnerSettings {
                        socket: settings.socket.clone(),
                        consumer: settings.consumer.clone(),
                        generation: settings.generation.clone(),
                    },
                    OwnerCredential::File(settings.credential_file.clone()),
                ),
                ResourceOwner::InProcess,
            )),
            (RunnerMode::Uds, true) => ExecutionOwner::Worker,
            (RunnerMode::Uds, false) => ExecutionOwner::UnstartedWorker,
        };
        Arc::new(Self {
            owner,
            status: ResourceAuthority::new(settings),
            logged: std::sync::Mutex::new(None),
        })
    }

    pub fn owner(&self) -> ResourceOwner {
        self.owner.kind()
    }

    /// The report of a registration session the owner started after this call arrived, or why
    /// the owner could not be asked beside the authority's status from the gateway's own probe.
    pub async fn report(&self, runner: &RuntimeBackend) -> ResourceAuthorityInfo {
        let reported = match (&self.owner, runner) {
            (ExecutionOwner::InProcess(registration), _) => Ok(registration.report().await),
            (ExecutionOwner::Worker, RuntimeBackend::Uds(worker)) => worker.registration().await,
            (ExecutionOwner::Worker, RuntimeBackend::InProcess(_))
            | (ExecutionOwner::UnstartedWorker, _) => {
                Err(ResourceRegistrationState::UnsupportedMode)
            }
        };
        let info = match reported {
            Ok(info) => info,
            Err(state) => {
                let mut info = self.status.status().await;
                info.participation = ResourceParticipation::Registration;
                info.registration = Some(ResourceRegistrationInfo {
                    owner: self.owner.kind(),
                    state,
                    error_code: None,
                    pid: None,
                });
                info
            }
        };
        self.log(&info);
        info
    }

    fn log(&self, info: &ResourceAuthorityInfo) {
        let Ok(mut logged) = self.logged.lock() else {
            return;
        };
        if info.registration.is_some() && *logged != info.registration {
            if let Some(registration) = &info.registration {
                tracing::info!(
                    owner = ?registration.owner,
                    state = ?registration.state,
                    error_code = ?registration.error_code,
                    pid = ?registration.pid,
                    "DevGuard registration"
                );
            }
            logged.clone_from(&info.registration);
        }
    }
}

/// What a worker the gateway starts gets so it registers itself (CSRG-U2). None of it is a
/// secret: the consumer secret goes through one private descriptor.
#[derive(Debug, Clone)]
pub struct WorkerSettings {
    settings: Settings,
}

impl WorkerSettings {
    #[cfg(test)]
    pub(crate) fn from_settings(settings: Settings) -> Self {
        Self { settings }
    }

    /// Add the worker's DevGuard arguments and hand it the consumer secret, read from its file
    /// as a status probe reads it. Only that worker inherits the descriptor, after
    /// `worker_command` has excluded every other one; the gateway's copy closes when the
    /// command is dropped. When the file cannot be used, the worker gets no descriptor and
    /// reports `credential_unavailable`.
    pub(crate) fn prepare(&self, command: &mut tokio::process::Command) {
        command
            .arg("--devguard-socket")
            .arg(&self.settings.socket)
            .arg("--devguard-consumer")
            .arg(&self.settings.consumer)
            .arg("--devguard-generation")
            .arg(&self.settings.generation);
        if let Some(carrier) = handoff::prepare(&self.settings.credential_file) {
            let fd = carrier.attach(command.as_std_mut());
            command.arg("--devguard-credential-fd").arg(fd.to_string());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::Cli;
    use clap::{CommandFactory, Parser};
    use codespace_devguard::{Capability, ErrorCode, Report, Role};
    use codespace_domain::{
        ProcessId, Profile, ResourceAuthorityCapability, ResourceAuthorityErrorCode,
        ResourceAuthorityProvider, ResourceAuthorityReport, ResourceAuthorityRole,
        ResourceAuthorityState, WorkspaceId,
    };
    use codespace_policy::Workspace;
    use codespace_runner::{InProcessRunner, Runner, RunnerExecRequest, RunnerReadProcess};
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// A well-formed consumer secret that no authority knows.
    pub(crate) const SECRET: &str =
        "5ec2e7d0c0de5ec2e7d0c0de5ec2e7d0c0de5ec2e7d0c0de5ec2e7d0c0de5ec2";

    /// Settings whose credential file holds [`SECRET`], for an endpoint at `socket`.
    pub(crate) fn settings(dir: &Path, socket: PathBuf) -> Settings {
        let credential_file = dir.join("consumer.secret");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&credential_file)
            .unwrap();
        file.write_all(SECRET.as_bytes()).unwrap();
        Settings {
            socket,
            consumer: "codespace".into(),
            generation: "g1".into(),
            credential_file,
        }
    }

    /// `session` again while it stops at the credential, up to five times. DevGuard reads the
    /// consumer secret within 250 ms of wall time, so a test thread the scheduler holds that
    /// long gets a credential failure although the file is intact; such a session opens nothing.
    /// For sessions whose credential is intact and not what the test checks.
    pub(crate) async fn past_the_credential<T, F>(
        stopped: impl Fn(&T) -> bool,
        mut session: impl FnMut() -> F,
    ) -> T
    where
        F: std::future::Future<Output = T>,
    {
        for _ in 1..5 {
            let outcome = session().await;
            if !stopped(&outcome) {
                return outcome;
            }
        }
        session().await
    }

    pub(crate) fn stopped_at_the_credential(info: &ResourceAuthorityInfo) -> bool {
        info.state == ResourceAuthorityState::CredentialUnavailable
    }

    /// Short enough for a Unix socket path.
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

    /// An endpoint that accepts each connection and hands it to `serve` on its own thread.
    pub(crate) struct Endpoint {
        stop: Arc<AtomicBool>,
        accepted: Arc<AtomicUsize>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Endpoint {
        pub(crate) fn start(
            socket: &Path,
            serve: impl Fn(std::os::unix::net::UnixStream) + Send + Sync + 'static,
        ) -> Self {
            let listener = UnixListener::bind(socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let accepted = Arc::new(AtomicUsize::new(0));
            let (signal, count, serve) = (stop.clone(), accepted.clone(), Arc::new(serve));
            let worker = std::thread::spawn(move || {
                while !signal.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            count.fetch_add(1, Ordering::Relaxed);
                            let serve = serve.clone();
                            std::thread::spawn(move || serve(stream));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            });
            Self {
                stop,
                accepted,
                worker: Some(worker),
            }
        }

        pub(crate) fn accepted(&self) -> usize {
            self.accepted.load(Ordering::Relaxed)
        }
    }

    impl Drop for Endpoint {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    #[test]
    fn participation_is_off_unless_asked_and_status_needs_every_setting() {
        let cli = Cli::try_parse_from(["codespace-mcp"]).unwrap();
        assert_eq!(cli.devguard.mode, DevGuardMode::Off);
        assert_eq!(cli.devguard.settings(), None);

        let missing = Cli::try_parse_from(["codespace-mcp", "--devguard", "status"])
            .unwrap_err()
            .to_string();
        for flag in [
            "--devguard-socket",
            "--devguard-consumer",
            "--devguard-generation",
            "--devguard-credential-file",
        ] {
            assert!(missing.contains(flag), "{missing}");
        }

        let cli = Cli::try_parse_from([
            "codespace-mcp",
            "--devguard",
            "status",
            "--devguard-socket",
            "/private/tmp/devguard-501/authority.sock",
            "--devguard-consumer",
            "codespace",
            "--devguard-generation",
            "g1",
            "--devguard-credential-file",
            "/home/user/codespace.secret",
        ])
        .unwrap();
        assert_eq!(
            cli.devguard.settings(),
            Some(Settings {
                socket: "/private/tmp/devguard-501/authority.sock".into(),
                consumer: "codespace".into(),
                generation: "g1".into(),
                credential_file: "/home/user/codespace.secret".into(),
            })
        );
        // Status participation starts no worker with DevGuard settings.
        assert!(cli.devguard.worker().is_none());
    }

    #[test]
    fn registration_needs_every_setting_and_gives_them_to_a_started_worker() {
        let missing = Cli::try_parse_from(["codespace-mcp", "--devguard", "register"])
            .unwrap_err()
            .to_string();
        for flag in [
            "--devguard-socket",
            "--devguard-consumer",
            "--devguard-generation",
            "--devguard-credential-file",
        ] {
            assert!(missing.contains(flag), "{missing}");
        }
        let cli = Cli::try_parse_from([
            "codespace-mcp",
            "--devguard",
            "register",
            "--devguard-socket",
            "/private/tmp/devguard-501/authority.sock",
            "--devguard-consumer",
            "codespace",
            "--devguard-generation",
            "g1",
            "--devguard-credential-file",
            "/home/user/codespace.secret",
        ])
        .unwrap();
        assert_eq!(cli.devguard.mode, DevGuardMode::Register);
        let settings = cli.devguard.settings().unwrap();
        assert_eq!(cli.devguard.worker().unwrap().settings, settings);
        // The owner follows the runner mode; the gateway never stands in for its worker.
        for (runner, starts_worker, owner) in [
            (RunnerMode::InProcess, false, ResourceOwner::InProcess),
            (RunnerMode::Uds, true, ResourceOwner::Worker),
            (RunnerMode::Uds, false, ResourceOwner::Worker),
        ] {
            let registration = Registration::new(settings.clone(), runner, starts_worker);
            assert_eq!(registration.owner(), owner);
        }
    }

    #[test]
    fn no_setting_takes_the_secret_itself() {
        // The secret is named only by its file, so it never appears in argv or environment.
        let command = Cli::command();
        let devguard: Vec<_> = command
            .get_arguments()
            .filter(|arg| arg.get_id().as_str().starts_with("devguard"))
            .collect();
        assert_eq!(devguard.len(), 5);
        for arg in devguard {
            let long = arg.get_long().unwrap();
            assert!(
                !long.contains("secret") && !long.contains("token"),
                "{long}"
            );
        }
    }

    #[test]
    fn every_adapter_state_maps_to_the_same_contract_state() {
        let cases = [
            (State::Available, ResourceAuthorityState::Available),
            (State::Unavailable, ResourceAuthorityState::Unavailable),
            (
                State::UntrustedAuthority,
                ResourceAuthorityState::UntrustedAuthority,
            ),
            (State::Incompatible, ResourceAuthorityState::Incompatible),
            (
                State::CredentialRefused,
                ResourceAuthorityState::CredentialRefused,
            ),
            (
                State::CredentialUnavailable,
                ResourceAuthorityState::CredentialUnavailable,
            ),
        ];
        for (state, expected) in cases {
            let mapped = status_info(Status {
                state,
                error_code: Some(ErrorCode::Unauthorized),
                report: None,
            });
            assert_eq!(mapped.state, expected);
            assert_eq!(
                mapped.error_code,
                Some(ResourceAuthorityErrorCode::Unauthorized)
            );
            // Status participation never governs execution, whatever the authority says.
            assert_eq!(mapped.provider, ResourceAuthorityProvider::Devguard);
            assert_eq!(mapped.participation, ResourceParticipation::Status);
            assert!(!mapped.governs_execution);
        }
        let available = status_info(Status {
            state: State::Available,
            error_code: None,
            report: Some(Report {
                protocol: 1,
                capabilities: vec![Capability::DurableAdmission],
                role: Role::Workload,
                storage_validated: true,
                registration_ready: true,
                execution_ready: true,
            }),
        });
        assert!(!available.governs_execution);
        assert_eq!(
            available.report,
            Some(ResourceAuthorityReport {
                protocol: 1,
                capabilities: vec![ResourceAuthorityCapability::DurableAdmission],
                role: ResourceAuthorityRole::Workload,
                storage_validated: true,
                registration_ready: true,
                execution_ready: true,
            })
        );
    }

    #[test]
    fn reported_codes_capabilities_and_roles_keep_devguard_wire_names() {
        let wire = |value: serde_json::Value| value.as_str().unwrap().to_owned();
        for code in ErrorCode::ALL {
            let reported = status_info(Status {
                state: State::Unavailable,
                error_code: Some(code),
                report: None,
            });
            let reported = serde_json::to_value(reported.error_code.unwrap()).unwrap();
            assert_eq!(wire(reported), code.wire_name());
        }
        for role in Role::ALL {
            let reported = status_info(Status {
                state: State::Available,
                error_code: None,
                report: Some(Report {
                    protocol: 1,
                    capabilities: Capability::ALL.to_vec(),
                    role,
                    storage_validated: true,
                    registration_ready: true,
                    execution_ready: true,
                }),
            })
            .report
            .unwrap();
            assert_eq!(
                wire(serde_json::to_value(reported.role).unwrap()),
                role.wire_name()
            );
            for (reported, each) in reported.capabilities.into_iter().zip(Capability::ALL) {
                assert_eq!(
                    wire(serde_json::to_value(reported).unwrap()),
                    each.wire_name()
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn probes_run_one_at_a_time_and_report_from_after_the_call() {
        let dir = short_directory();
        let socket = dir.path().join("slow.sock");
        // Hold each session 100 ms, answering nothing. A probe ends at that close or at
        // DevGuard's 250 ms deadline, so it lasts at least 100 ms however late the endpoint
        // takes its session: probes that run one at a time take at least 100 ms each. Counting
        // the sessions the endpoint holds at once would not show this, since a late endpoint
        // can still hold a session whose probe has ended.
        let endpoint = Endpoint::start(&socket, |stream| {
            std::thread::sleep(Duration::from_millis(100));
            drop(stream);
        });
        let authority = ResourceAuthority::new(settings(dir.path(), socket));
        let started = Instant::now();
        let calls = (0..8).map(|_| {
            let authority = authority.clone();
            tokio::spawn(async move {
                past_the_credential(stopped_at_the_credential, || authority.status()).await
            })
        });
        for call in calls.collect::<Vec<_>>() {
            assert_eq!(
                call.await.unwrap().state,
                ResourceAuthorityState::Unavailable
            );
        }
        let elapsed = started.elapsed();
        let probes = endpoint.accepted();
        // Calls that arrived together share a probe that started after they arrived.
        assert!((1..=2).contains(&probes), "{probes}");
        assert!(
            elapsed >= Duration::from_millis(100) * probes as u32,
            "{probes} probes in {elapsed:?}"
        );
        past_the_credential(stopped_at_the_credential, || authority.status()).await;
        assert_eq!(endpoint.accepted(), probes + 1, "a later call probes again");
    }

    /// tracing caches whether each log statement is wanted, and with one subscriber, this
    /// test's, it asks the subscriber of the thread that reaches the statement first. Another
    /// test's thread reaching the status log first would turn it off here, so the test runs
    /// alone.
    #[test]
    fn logs_name_the_state_and_never_the_secret() {
        run_alone("devguard::tests::logs_child");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "run alone in a separate process by its parent test"]
    async fn logs_child() {
        #[derive(Clone, Default)]
        struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for Captured {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);

        let dir = short_directory();
        let missing = settings(dir.path(), dir.path().join("absent.sock"));
        let unusable = Settings {
            credential_file: dir.path().join("absent.secret"),
            ..missing.clone()
        };
        let silent_socket = dir.path().join("silent.sock");
        let _silent = Endpoint::start(&silent_socket, |stream| {
            std::thread::sleep(Duration::from_millis(300));
            drop(stream);
        });
        let silent = Settings {
            socket: silent_socket,
            ..missing.clone()
        };
        for settings in [missing, unusable, silent] {
            ResourceAuthority::new(settings).status().await;
        }
        let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("state=Unavailable"), "{logs}");
        assert!(logs.contains("state=CredentialUnavailable"), "{logs}");
        assert!(!logs.contains(SECRET), "{logs}");
    }

    /// What a child reports about its descriptors above 2 and its environment.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(crate) struct ChildReport {
        pub(crate) descriptors: BTreeSet<RawFd>,
        pub(crate) sockets: BTreeSet<RawFd>,
        pub(crate) environment: String,
    }

    /// A script that lists the child's descriptors 3..128 and sockets among them, then its
    /// environment. The descriptor numbers the parent can hold stay below 128 here.
    pub(crate) fn report_script() -> String {
        let numbers: Vec<String> = (3..128).map(|n| n.to_string()).collect();
        format!(
            "fds=; sockets=; for n in {}; do if [ -e /dev/fd/$n ]; then fds=\"$fds $n\"; \
             if [ -S /dev/fd/$n ]; then sockets=\"$sockets $n\"; fi; fi; done; \
             echo \"fds:$fds\"; echo \"sockets:$sockets\"; env",
            numbers.join(" ")
        )
    }

    pub(crate) fn parse(output: &str) -> ChildReport {
        let numbers = |prefix: &str| -> BTreeSet<RawFd> {
            output
                .lines()
                .find_map(|line| line.trim_end_matches('\r').strip_prefix(prefix))
                .unwrap_or_else(|| panic!("no {prefix} line in {output:?}"))
                .split_whitespace()
                .map(|n| n.parse().unwrap())
                .collect()
        };
        ChildReport {
            descriptors: numbers("fds:"),
            sockets: numbers("sockets:"),
            environment: output.to_owned(),
        }
    }

    /// CodeSpace's spawners, and an unguarded spawn as the control.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Spawner {
        Pipe,
        Pty,
        Worker,
        Unguarded,
    }

    struct Children {
        runner: InProcessRunner,
        workspace: Workspace,
        dir: tempfile::TempDir,
        next: AtomicUsize,
    }

    impl Children {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let workspace = Workspace::new(
                WorkspaceId("demo".into()),
                dir.path().to_path_buf(),
                Profile::WorkspaceWrite,
            );
            Self {
                runner: InProcessRunner::new(Arc::new(|_| {})),
                workspace,
                dir,
                next: AtomicUsize::new(0),
            }
        }

        async fn report(&self, spawner: Spawner) -> ChildReport {
            let n = self.next.fetch_add(1, Ordering::Relaxed);
            let output = match spawner {
                Spawner::Pipe | Spawner::Pty => self.execute(n, spawner == Spawner::Pty).await,
                Spawner::Worker => self.worker(n).await,
                Spawner::Unguarded => {
                    let output = tokio::process::Command::new("/bin/sh")
                        .arg("-c")
                        .arg(report_script())
                        .output()
                        .await
                        .unwrap();
                    String::from_utf8(output.stdout).unwrap()
                }
            };
            parse(&output)
        }

        async fn execute(&self, n: usize, tty: bool) -> String {
            let process_id = ProcessId(format!("d6-{n}"));
            let mut request = RunnerExecRequest::for_host(
                vec!["/bin/sh".into(), "-c".into(), report_script()],
                process_id.clone(),
                Profile::WorkspaceWrite,
            );
            request.tty = tty;
            self.runner.exec(&self.workspace, request).await.unwrap();
            for _ in 0..1000 {
                let read = self
                    .runner
                    .read_process(RunnerReadProcess {
                        process_id: process_id.clone(),
                        cursor: 0,
                    })
                    .await
                    .unwrap();
                if read.eof {
                    return read.chunk;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("{process_id:?} did not finish");
        }

        /// The exact command that starts the UDS worker, running the script as `/bin/sh` would.
        async fn worker(&self, n: usize) -> String {
            let report = self.dir.path().join(format!("worker-{n}"));
            let script = self.dir.path().join(format!("worker-{n}.sh"));
            std::fs::write(
                &script,
                format!("{{ {}; }} > '{}'\n", report_script(), report.display()),
            )
            .unwrap();
            let status = crate::runtime::worker_command(Path::new("/bin/sh"), &script)
                .status()
                .await
                .unwrap();
            assert!(status.success());
            std::fs::read_to_string(report).unwrap()
        }
    }

    /// Run the report script as execution `n` of `runner` and return its output.
    pub(crate) async fn execute_report<R: Runner>(
        runner: &R,
        workspace: &Workspace,
        n: usize,
        tty: bool,
    ) -> String {
        let process_id = ProcessId(format!("report-{n}-{tty}"));
        let mut request = RunnerExecRequest::for_host(
            vec!["/bin/sh".into(), "-c".into(), report_script()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        request.tty = tty;
        runner.exec(workspace, request).await.unwrap();
        for _ in 0..1000 {
            let read = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            if read.eof {
                return read.chunk;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("{process_id:?} did not finish");
    }

    /// A connected socket left inheritable, as one is inside DevGuard's connect window on macOS.
    fn inheritable_socket() -> (
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    ) {
        // The server crate has no libc dependency; these values are the same on macOS and Linux.
        const F_GETFD: std::os::raw::c_int = 1;
        const F_SETFD: std::os::raw::c_int = 2;
        const FD_CLOEXEC: std::os::raw::c_int = 1;
        extern "C" {
            fn fcntl(fd: std::os::raw::c_int, cmd: std::os::raw::c_int, ...)
                -> std::os::raw::c_int;
        }
        let (held, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: F_GETFD and F_SETFD change only this descriptor's flags.
        unsafe {
            let flags = fcntl(held.as_raw_fd(), F_GETFD);
            assert!(flags >= 0);
            assert_eq!(fcntl(held.as_raw_fd(), F_SETFD, flags & !FD_CLOEXEC), 0);
        }
        (held, peer)
    }

    #[tokio::test]
    async fn the_child_check_sees_an_inheritable_socket_that_codespace_spawners_exclude() {
        let children = Children::new();
        // Each spawner's descriptors before the socket exists. The worker's shell holds its own
        // from 10 up, which the socket's number can equal, so each child must hold exactly
        // these; one that had the socket would hold one more.
        let spawners = [Spawner::Pipe, Spawner::Pty, Spawner::Worker];
        let mut baseline = Vec::new();
        for spawner in spawners {
            baseline.push(children.report(spawner).await.descriptors);
        }
        let (held, _peer) = inheritable_socket();
        let fd = held.as_raw_fd();
        let control = children.report(Spawner::Unguarded).await;
        assert!(control.sockets.contains(&fd), "{control:?}");
        for (spawner, expected) in spawners.into_iter().zip(baseline) {
            let report = children.report(spawner).await;
            assert_eq!(report.descriptors, expected, "{spawner:?}: {report:?}");
        }
    }

    /// Sessions opened through DevGuard's client while CodeSpace's spawners start children
    /// (W3 C's inheritance case against CodeSpace's own spawners). `CODESPACE_D6_CHILDREN`
    /// sets how many children each spawner starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sessions_opened_while_children_spawn_reach_no_child() {
        let per_spawner: usize = std::env::var("CODESPACE_D6_CHILDREN")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);
        let dir = short_directory();
        let socket = dir.path().join("authority.sock");
        // The endpoint closes each session at once, so each probe opens a session socket
        // through DevGuard's `connect_timeout` and ends quickly. Probes of a missing socket
        // create and close their socket even faster, so more creation windows are open.
        let _endpoint = Endpoint::start(&socket, drop);
        let settings = settings(dir.path(), socket);
        let missing = Settings {
            socket: dir.path().join("missing.sock"),
            ..settings.clone()
        };
        let children = Arc::new(Children::new());

        // Each spawner's descriptors with no session being opened.
        let mut baseline = Vec::new();
        for spawner in [Spawner::Pipe, Spawner::Pty, Spawner::Worker] {
            baseline.push((spawner, children.report(spawner).await.descriptors));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let probes: Vec<_> = [&settings, &settings, &missing, &missing]
            .into_iter()
            .map(|settings| {
                let (stop, settings) = (stop.clone(), settings.clone());
                std::thread::spawn(move || {
                    let mut sessions = 0usize;
                    while !stop.load(Ordering::Relaxed) {
                        let status = codespace_devguard::probe(&settings);
                        // DevGuard reads the consumer secret within 250 ms of wall time. With
                        // the other tests running, this thread can be held past that, and the
                        // probe then ends before it opens a session: nothing for a child to get.
                        if status.state == State::CredentialUnavailable {
                            continue;
                        }
                        assert_eq!(status.state, State::Unavailable, "{status:?}");
                        sessions += 1;
                    }
                    sessions
                })
            })
            .collect();
        let run = |spawner: Spawner| {
            let children = children.clone();
            tokio::spawn(async move {
                let mut reports = Vec::new();
                for _ in 0..per_spawner {
                    reports.push(children.report(spawner).await);
                }
                (spawner, reports)
            })
        };
        let runs = [
            run(Spawner::Pipe),
            run(Spawner::Pty),
            run(Spawner::Worker),
            run(Spawner::Unguarded),
        ];
        let mut results = Vec::new();
        for run in runs {
            results.push(run.await.unwrap());
        }
        stop.store(true, Ordering::Relaxed);
        let sessions: usize = probes.into_iter().map(|probe| probe.join().unwrap()).sum();
        assert!(sessions > 0);

        let mut summary = format!("d6: {sessions} probes");
        for (spawner, reports) in &results {
            let holders = reports
                .iter()
                .filter(|report| !report.sockets.is_empty())
                .count();
            summary += &format!("; {spawner:?} {holders}/{} held a socket", reports.len());
            for report in reports {
                assert!(
                    !report.environment.contains(SECRET),
                    "{spawner:?}: {report:?}"
                );
                if let Some((_, expected)) = baseline.iter().find(|(s, _)| s == spawner) {
                    assert_eq!(&report.descriptors, expected, "{spawner:?}: {report:?}");
                }
            }
        }
        eprintln!("{summary}");
    }

    // CSRG-U2: the execution owner's registration.

    /// A binary that `variable` names. Unset, the test is skipped, unless
    /// `CODESPACE_REQUIRE_DEVGUARD_BINS` is set: CI sets it where the binaries are built.
    pub(crate) fn test_binary(variable: &str) -> Option<PathBuf> {
        match std::env::var_os(variable) {
            Some(path) => Some(PathBuf::from(path)),
            None if std::env::var_os("CODESPACE_REQUIRE_DEVGUARD_BINS").is_some() => {
                panic!("{variable} must name a binary here")
            }
            None => None,
        }
    }

    /// The worker built with the `devguard` feature (`CODESPACE_DEVGUARD_RUNTIME_BIN`), launched
    /// once first: the first launch of a freshly linked binary can take macOS over a second,
    /// longer than a test should spend on the operating system's checks of the file.
    pub(crate) fn devguard_worker() -> Option<PathBuf> {
        static WARMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        let bin = test_binary("CODESPACE_DEVGUARD_RUNTIME_BIN")?;
        WARMED.get_or_init(|| {
            // An unknown argument makes the worker exit at once, before it binds anything.
            let _ = std::process::Command::new(&bin)
                .args(["/nonexistent/runner.sock", "--devguard-warm-up", "now"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
        Some(bin)
    }

    /// DevGuard's fixture authority in its own process (`CODESPACE_DEVGUARD_FIXTURE_BIN`, the
    /// adapter crate's `fixture_authority` example), with a `codespace` control-service
    /// consumer. Without native host evidence it cannot register anyone (Linux).
    pub(crate) struct FixtureAuthority {
        _directory: tempfile::TempDir,
        child: std::process::Child,
        stdin: Option<std::process::ChildStdin>,
        stdout: std::io::BufReader<std::process::ChildStdout>,
        pub(crate) settings: Settings,
        pub(crate) native: bool,
    }

    impl FixtureAuthority {
        pub(crate) fn start(max_instances: u32) -> Option<Self> {
            use std::io::BufRead;
            let bin = test_binary("CODESPACE_DEVGUARD_FIXTURE_BIN")?;
            let directory = short_directory();
            let mut child = std::process::Command::new(bin)
                .arg(directory.path())
                .arg(max_instances.to_string())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take();
            let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            let text = |key: &str| value[key].as_str().unwrap().to_owned();
            Some(Self {
                settings: Settings {
                    socket: text("socket").into(),
                    consumer: text("consumer"),
                    generation: text("generation"),
                    credential_file: text("credential_file").into(),
                },
                native: value["native"].as_bool().unwrap(),
                _directory: directory,
                child,
                stdin,
                stdout,
            })
        }

        /// The authority's registered `codespace` instances: identity and process ID.
        pub(crate) fn instances(&mut self) -> Vec<(String, u32)> {
            use std::io::BufRead;
            let stdin = self.stdin.as_mut().unwrap();
            stdin.write_all(b"instances\n").unwrap();
            stdin.flush().unwrap();
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            let mut instances: Vec<_> = value
                .as_array()
                .unwrap()
                .iter()
                .map(|instance| {
                    (
                        instance["instance_id"].as_str().unwrap().to_owned(),
                        instance["pid"].as_u64().unwrap() as u32,
                    )
                })
                .collect();
            instances.sort();
            instances
        }
    }

    impl Drop for FixtureAuthority {
        fn drop(&mut self) {
            // The fixture stops at the end of its input.
            drop(self.stdin.take());
            let _ = self.child.wait();
        }
    }

    /// Run the ignored test `name` alone, in a new process of this test binary, and return
    /// its output. The stand-in worker is a shell, and bash moves its script to descriptor 255;
    /// alone, the gateway's descriptors keep low numbers, so a carrier is never there.
    fn run_alone(name: &str) -> String {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                name,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
        text
    }

    /// A worker script that reports its arguments, its descriptors and environment, and how
    /// many bytes the descriptor it was handed carries.
    fn reporting_worker(dir: &Path, report: &Path) -> PathBuf {
        let script = dir.join("worker.sh");
        std::fs::write(
            &script,
            format!(
                "fd=; previous=; for argument in \"$@\"; do \
                 if [ \"$previous\" = --devguard-credential-fd ]; then fd=$argument; fi; \
                 previous=$argument; done; \
                 {{ echo \"args: $*\"; {}; \
                 if [ -n \"$fd\" ]; then echo \"carried: $(wc -c <&$fd | tr -d ' ')\"; fi; }} > '{}'\n",
                report_script(),
                report.display()
            ),
        )
        .unwrap();
        script
    }

    /// Start the reporting worker as the gateway starts its worker with DevGuard settings. It
    /// runs under bash: dash, Ubuntu's `/bin/sh`, reads only single-digit descriptors.
    async fn start_reporting_worker(worker: &WorkerSettings, dir: &Path) -> String {
        let report = dir.join("report");
        let script = reporting_worker(dir, &report);
        let mut command = crate::runtime::worker_command(Path::new("/bin/bash"), &script);
        worker.prepare(&mut command);
        assert!(!format!("{command:?}").contains(SECRET));
        let status = command.status().await.unwrap();
        drop(command);
        assert!(status.success());
        std::fs::read_to_string(report).unwrap()
    }

    #[test]
    fn a_started_worker_gets_its_settings_and_one_descriptor_never_the_secret() {
        run_alone("devguard::tests::started_worker_child");
    }

    #[tokio::test]
    #[ignore = "run alone in a separate process by its parent test"]
    async fn started_worker_child() {
        let dir = short_directory();
        let socket = dir.path().join("authority.sock");
        let worker = WorkerSettings::from_settings(settings(dir.path(), socket.clone()));
        // A worker gets no carrier when its secret could not be read (see `past_the_credential`).
        let output = past_the_credential(
            |output: &String| !output.contains("--devguard-credential-fd"),
            || start_reporting_worker(&worker, dir.path()),
        )
        .await;
        assert!(!output.contains(SECRET), "{output}");
        let args = output
            .lines()
            .find_map(|line| line.strip_prefix("args: "))
            .unwrap();
        let fd: RawFd = args
            .rsplit_once("--devguard-credential-fd ")
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            args,
            format!(
                "--devguard-socket {} --devguard-consumer codespace --devguard-generation g1 \
                 --devguard-credential-fd {fd}",
                socket.display()
            )
        );
        // Its one socket is the carrier, holding the secret. (Its other descriptors are the
        // shell's own, whose numbers depend on which ones the carrier left free.)
        assert_eq!(parse(&output).sockets, BTreeSet::from([fd]));
        assert!(output.contains("carried: 64"), "{output}");
    }

    #[tokio::test]
    async fn a_worker_whose_credential_cannot_be_used_gets_no_descriptor() {
        let dir = short_directory();
        let mut unusable = settings(dir.path(), dir.path().join("authority.sock"));
        unusable.credential_file = dir.path().join("missing.secret");
        let worker = WorkerSettings::from_settings(unusable);
        let output = start_reporting_worker(&worker, dir.path()).await;
        assert!(output.contains("--devguard-socket"), "{output}");
        assert!(!output.contains("--devguard-credential-fd"), "{output}");
        assert!(!output.contains("carried:"), "{output}");
        assert_eq!(parse(&output).sockets, BTreeSet::new());
    }

    const CLEANUP: &str = "CODESPACE_DEVGUARD_CLEANUP_CHILD";

    /// The descriptors this process holds, below 1024.
    fn open_descriptors() -> BTreeSet<RawFd> {
        const F_GETFD: std::os::raw::c_int = 1;
        extern "C" {
            fn fcntl(fd: std::os::raw::c_int, cmd: std::os::raw::c_int, ...)
                -> std::os::raw::c_int;
        }
        // SAFETY: F_GETFD only inspects each descriptor number.
        (0..1024)
            .filter(|fd| unsafe { fcntl(*fd, F_GETFD) } >= 0)
            .collect()
    }

    #[test]
    fn a_failed_worker_start_leaves_no_descriptor_behind() {
        // In a process of its own, so no other test opens a descriptor meanwhile.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "devguard::tests::worker_start_cleanup_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CLEANUP, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("no descriptor left after 2 failed starts"),
            "{stdout}"
        );
        assert!(!stdout.contains(SECRET));
    }

    #[test]
    #[ignore = "run alone in a separate process by its parent test"]
    fn worker_start_cleanup_child() {
        assert!(std::env::var_os(CLEANUP).is_some());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = short_directory();
            let worker = WorkerSettings::from_settings(settings(
                dir.path(),
                dir.path().join("authority.sock"),
            ));
            let store = Arc::new(codespace_store::Store::memory().unwrap());
            // Let the runtime open what it keeps for child processes before counting.
            tokio::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("exit 0")
                .status()
                .await
                .unwrap();
            let before = open_descriptors();
            // A worker binary that does not exist, and one that exits before it listens:
            // `/bin/sh` reading the runner socket's path as a script.
            for bin in [dir.path().join("missing-worker"), PathBuf::from("/bin/sh")] {
                let error = crate::runtime::RuntimeProcess::spawn_registered(
                    &bin,
                    Some(dir.path()),
                    Arc::new(|_| {}),
                    store.clone(),
                    &worker,
                )
                .await
                .err()
                .unwrap();
                assert!(!format!("{error:#}").contains(SECRET));
            }
            assert_eq!(open_descriptors(), before);
            println!("no descriptor left after 2 failed starts");
        });
    }

    /// Workers started with a carrier while CodeSpace's spawners start children (#79's
    /// inheritance case against the credential carrier). `CODESPACE_D6_CHILDREN` sets how many
    /// children each spawner starts.
    #[test]
    fn carriers_made_while_children_spawn_reach_only_their_worker() {
        let output = run_alone("devguard::tests::carriers_child");
        eprintln!(
            "{}",
            output
                .lines()
                .find(|line| line.starts_with("carriers:"))
                .unwrap_or("carriers: no summary")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "run alone in a separate process by its parent test"]
    async fn carriers_child() {
        let per_spawner: usize = std::env::var("CODESPACE_D6_CHILDREN")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);
        let dir = short_directory();
        let worker = Arc::new(WorkerSettings::from_settings(settings(
            dir.path(),
            dir.path().join("authority.sock"),
        )));
        // Each started worker checks that it holds a carrier.
        let script = dir.path().join("carrier.sh");
        std::fs::write(
            &script,
            "fd=; previous=; for argument in \"$@\"; do \
             if [ \"$previous\" = --devguard-credential-fd ]; then fd=$argument; fi; \
             previous=$argument; done; [ -n \"$fd\" ] && [ -S /dev/fd/$fd ]\n",
        )
        .unwrap();
        let children = Arc::new(Children::new());
        let mut baseline = Vec::new();
        for spawner in [Spawner::Pipe, Spawner::Pty, Spawner::Worker] {
            baseline.push((spawner, children.report(spawner).await.descriptors));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let starters: Vec<_> = (0..2)
            .map(|_| {
                let (stop, worker, script) = (stop.clone(), worker.clone(), script.clone());
                tokio::spawn(async move {
                    let mut started = 0usize;
                    while !stop.load(Ordering::Relaxed) {
                        let mut command =
                            crate::runtime::worker_command(Path::new("/bin/sh"), &script);
                        worker.prepare(&mut command);
                        let handed = format!("{command:?}").contains("--devguard-credential-fd");
                        // The secret could not be read in time (see `past_the_credential`):
                        // this start makes no carrier to follow.
                        if !handed {
                            continue;
                        }
                        let status = command.status().await.unwrap();
                        drop(command);
                        assert!(
                            status.success(),
                            "a worker did not hold its carrier (handed: {handed}, {status})"
                        );
                        started += 1;
                    }
                    started
                })
            })
            .collect();
        let run = |spawner: Spawner| {
            let children = children.clone();
            tokio::spawn(async move {
                let mut reports = Vec::new();
                for _ in 0..per_spawner {
                    reports.push(children.report(spawner).await);
                }
                (spawner, reports)
            })
        };
        let runs = [
            run(Spawner::Pipe),
            run(Spawner::Pty),
            run(Spawner::Worker),
            run(Spawner::Unguarded),
        ];
        let mut results = Vec::new();
        for run in runs {
            results.push(run.await.unwrap());
        }
        stop.store(true, Ordering::Relaxed);
        let mut started = 0;
        for starter in starters {
            started += starter.await.unwrap();
        }
        assert!(started > 0);

        let mut summary = format!("carriers: {started} workers started");
        for (spawner, reports) in &results {
            let holders = reports
                .iter()
                .filter(|report| !report.sockets.is_empty())
                .count();
            summary += &format!("; {spawner:?} {holders}/{} held a socket", reports.len());
            for report in reports {
                assert!(
                    !report.environment.contains(SECRET),
                    "{spawner:?}: {report:?}"
                );
                if let Some((_, expected)) = baseline.iter().find(|(s, _)| s == spawner) {
                    assert_eq!(&report.descriptors, expected, "{spawner:?}: {report:?}");
                }
            }
        }
        eprintln!("{summary}");
    }

    #[tokio::test]
    async fn each_owner_reports_its_registration_or_why_it_cannot() {
        let dir = short_directory();
        let socket = dir.path().join("authority.sock");
        // The endpoint closes each session at once.
        let endpoint = Endpoint::start(&socket, drop);
        let settings = settings(dir.path(), socket);
        let in_process = RuntimeBackend::in_process(Arc::new(|_| {}));

        // The gateway registers itself when it runs the executions...
        let registration = Registration::new(settings.clone(), RunnerMode::InProcess, false);
        let info = past_the_credential(stopped_at_the_credential, || {
            registration.report(&in_process)
        })
        .await;
        assert_eq!(info.participation, ResourceParticipation::Registration);
        assert!(!info.governs_execution);
        assert_eq!(
            info.registration,
            Some(ResourceRegistrationInfo {
                owner: ResourceOwner::InProcess,
                state: ResourceRegistrationState::Unavailable,
                error_code: Some(ResourceAuthorityErrorCode::ResourceControlUnavailable),
                pid: None,
            })
        );
        let sessions = endpoint.accepted();
        assert_eq!(sessions, 1);

        // ...but never for a worker: one it did not start reports that it cannot register,
        // beside the authority's status from the gateway's own probe.
        for starts_worker in [false, true] {
            let registration = Registration::new(settings.clone(), RunnerMode::Uds, starts_worker);
            let info = past_the_credential(stopped_at_the_credential, || {
                registration.report(&in_process)
            })
            .await;
            assert_eq!(info.participation, ResourceParticipation::Registration);
            assert!(!info.governs_execution);
            assert_eq!(info.state, ResourceAuthorityState::Unavailable);
            assert_eq!(
                info.registration,
                Some(ResourceRegistrationInfo {
                    owner: ResourceOwner::Worker,
                    state: ResourceRegistrationState::UnsupportedMode,
                    error_code: None,
                    pid: None,
                })
            );
        }
        assert!(!format!("{settings:?}").contains(SECRET));
    }

    /// Alone, as `logs_name_the_state_and_never_the_secret` is: other tests' threads reach the
    /// registration log too.
    #[test]
    fn registration_logs_name_the_owner_and_state_never_the_secret() {
        run_alone("devguard::tests::registration_logs_child");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "run alone in a separate process by its parent test"]
    async fn registration_logs_child() {
        #[derive(Clone, Default)]
        struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for Captured {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);

        let dir = short_directory();
        let settings = settings(dir.path(), dir.path().join("absent.sock"));
        let in_process = RuntimeBackend::in_process(Arc::new(|_| {}));
        for (runner, starts_worker) in [(RunnerMode::InProcess, false), (RunnerMode::Uds, false)] {
            let registration = Registration::new(settings.clone(), runner, starts_worker);
            past_the_credential(stopped_at_the_credential, || {
                registration.report(&in_process)
            })
            .await;
            // An unchanged registration is not logged again.
            registration.report(&in_process).await;
        }
        let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("owner=InProcess state=Unavailable"), "{logs}");
        assert!(
            logs.contains("owner=Worker state=UnsupportedMode"),
            "{logs}"
        );
        // One line per owner, and one more for each session that stopped at the credential.
        let registrations: Vec<&str> = logs
            .lines()
            .filter(|line| line.contains("DevGuard registration"))
            .collect();
        let stopped = registrations
            .iter()
            .filter(|line| line.contains("state=CredentialUnavailable"))
            .count();
        assert_eq!(registrations.len(), 2 + stopped, "{logs}");
        assert!(!logs.contains(SECRET), "{logs}");
    }

    /// A worker built without DevGuard says `Hello` without `registration`; one that registers
    /// states it.
    #[tokio::test]
    async fn a_worker_that_does_not_state_registration_is_refused() {
        use codespace_runner::{
            read_frame, write_frame, RunnerOpResult, UdsRunner, WireEnvelope, WIRE_PROTOCOL,
        };
        for registration in [false, true] {
            let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
            tokio::spawn(async move {
                let frame = read_frame(&mut server).await.unwrap().unwrap();
                let request: WireEnvelope = serde_json::from_slice(&frame).unwrap();
                let hello = RunnerOpResult::Hello {
                    protocol: WIRE_PROTOCOL,
                    registration,
                };
                let reply = WireEnvelope::response(request.request_id.unwrap(), Ok(hello));
                write_frame(&mut server, &reply).await.unwrap();
                // Keep the connection open until the client drops it.
                let _ = read_frame(&mut server).await;
            });
            let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
            runner.handshake().await.unwrap();
            let checked = crate::runtime::require_registration(&runner, Path::new("worker"));
            assert_eq!(checked.is_ok(), registration);
            if let Err(error) = checked {
                assert!(error.to_string().contains("cannot register"), "{error}");
            }
        }
    }
}
