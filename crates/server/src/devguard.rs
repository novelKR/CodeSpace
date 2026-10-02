//! Opt-in, status-only DevGuard connection (CSRG-U1).
//!
//! With `--devguard status`, `workspace_info` reports the DevGuard authority's status from one
//! bounded session that `codespace-devguard` opens and closes. CodeSpace registers, admits and
//! launches nothing through it, and no execution path consults it. Probes run one at a time:
//! a call that arrives while one runs waits for the next, so every report comes from a probe
//! that started after the call arrived.
//!
//! The consumer secret stays in its file. These settings hold only its path, and only the
//! state and DevGuard's error code are logged.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{Args, ValueEnum};
use codespace_devguard::{Settings, State, Status};
use codespace_domain::{
    ResourceAuthorityInfo, ResourceAuthorityReport, ResourceAuthorityState, ResourceParticipation,
};
use tokio::sync::Mutex;

/// How CodeSpace takes part in DevGuard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum DevGuardMode {
    /// No DevGuard connection.
    #[default]
    Off,
    /// Report DevGuard's status in `workspace_info`; nothing is admitted or launched.
    Status,
}

/// DevGuard settings. None is a secret: the consumer secret stays in its file.
#[derive(Debug, Clone, Args)]
pub struct DevGuardArgs {
    /// DevGuard participation: `off` (default) or `status`.
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
        required_if_eq("devguard", "status")
    )]
    pub socket: Option<PathBuf>,

    /// The consumer's id in DevGuard's operator configuration.
    #[arg(
        id = "devguard_consumer",
        long = "devguard-consumer",
        env = "CODESPACE_DEVGUARD_CONSUMER",
        required_if_eq("devguard", "status")
    )]
    pub consumer: Option<String>,

    /// The consumer's generation in DevGuard's operator configuration.
    #[arg(
        id = "devguard_generation",
        long = "devguard-generation",
        env = "CODESPACE_DEVGUARD_GENERATION",
        required_if_eq("devguard", "status")
    )]
    pub generation: Option<String>,

    /// A private file (0600, one link) holding exactly the consumer's 64-character secret.
    #[arg(
        id = "devguard_credential_file",
        long = "devguard-credential-file",
        env = "CODESPACE_DEVGUARD_CREDENTIAL_FILE",
        required_if_eq("devguard", "status")
    )]
    pub credential_file: Option<PathBuf>,
}

impl DevGuardArgs {
    /// The adapter settings, unless participation is off.
    pub fn settings(&self) -> Option<Settings> {
        match self.mode {
            DevGuardMode::Off => None,
            DevGuardMode::Status => Some(Settings {
                socket: self.socket.clone()?,
                consumer: self.consumer.clone()?,
                generation: self.generation.clone()?,
                credential_file: self.credential_file.clone()?,
            }),
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
        let info = info(status);
        if last.as_ref().map(|probed| probed.info.state) != Some(info.state) {
            tracing::info!(
                state = ?info.state,
                error_code = info.error_code.as_deref(),
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

fn info(status: Status) -> ResourceAuthorityInfo {
    ResourceAuthorityInfo {
        provider: "devguard".into(),
        participation: ResourceParticipation::Status,
        governs_execution: false,
        state: match status.state {
            State::Available => ResourceAuthorityState::Available,
            State::Unavailable => ResourceAuthorityState::Unavailable,
            State::UntrustedAuthority => ResourceAuthorityState::UntrustedAuthority,
            State::Incompatible => ResourceAuthorityState::Incompatible,
            State::CredentialRefused => ResourceAuthorityState::CredentialRefused,
            State::CredentialUnavailable => ResourceAuthorityState::CredentialUnavailable,
        },
        error_code: status.error_code.map(str::to_owned),
        report: status.report.map(|report| ResourceAuthorityReport {
            protocol: report.protocol,
            capabilities: report.capabilities.into_iter().map(str::to_owned).collect(),
            role: report.role.to_owned(),
            storage_validated: report.storage_validated,
            registration_ready: report.registration_ready,
            execution_ready: report.execution_ready,
            reason: report.reason,
        }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::Cli;
    use clap::{CommandFactory, Parser};
    use codespace_devguard::Report;
    use codespace_domain::{ProcessId, Profile, WorkspaceId};
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
            let mapped = info(Status {
                state,
                error_code: Some("unauthorized"),
                report: None,
            });
            assert_eq!(mapped.state, expected);
            assert_eq!(mapped.error_code.as_deref(), Some("unauthorized"));
            // Status participation never governs execution, whatever the authority says.
            assert_eq!(mapped.participation, ResourceParticipation::Status);
            assert!(!mapped.governs_execution);
        }
        let available = info(Status {
            state: State::Available,
            error_code: None,
            report: Some(Report {
                protocol: 1,
                capabilities: vec!["durable_admission"],
                role: "workload",
                storage_validated: true,
                registration_ready: true,
                execution_ready: true,
                reason: "open".into(),
            }),
        });
        assert!(!available.governs_execution);
        assert_eq!(
            available.report,
            Some(ResourceAuthorityReport {
                protocol: 1,
                capabilities: vec!["durable_admission".into()],
                role: "workload".into(),
                storage_validated: true,
                registration_ready: true,
                execution_ready: true,
                reason: "open".into(),
            })
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn probes_run_one_at_a_time_and_report_from_after_the_call() {
        let dir = short_directory();
        let socket = dir.path().join("slow.sock");
        let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (opened, highest) = (open.clone(), most.clone());
        // Hold each session, answering nothing, and record how many are open at once.
        let endpoint = Endpoint::start(&socket, move |stream| {
            let now = opened.fetch_add(1, Ordering::SeqCst) + 1;
            highest.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            opened.fetch_sub(1, Ordering::SeqCst);
            drop(stream);
        });
        let authority = ResourceAuthority::new(settings(dir.path(), socket));
        let calls = (0..8).map(|_| {
            let authority = authority.clone();
            tokio::spawn(async move { authority.status().await })
        });
        for call in calls.collect::<Vec<_>>() {
            assert_eq!(
                call.await.unwrap().state,
                ResourceAuthorityState::Unavailable
            );
        }
        assert_eq!(most.load(Ordering::SeqCst), 1);
        // Calls that arrived together share a probe that started after they arrived.
        assert!(
            (1..=2).contains(&endpoint.accepted()),
            "{}",
            endpoint.accepted()
        );
        let before = endpoint.accepted();
        authority.status().await;
        assert_eq!(endpoint.accepted(), before + 1, "a later call probes again");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logs_name_the_state_and_never_the_secret() {
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
    struct ChildReport {
        descriptors: BTreeSet<RawFd>,
        sockets: BTreeSet<RawFd>,
        environment: String,
    }

    /// A script that lists the child's descriptors 3..128 and sockets among them, then its
    /// environment. The descriptor numbers the parent can hold stay below 128 here.
    fn report_script() -> String {
        let numbers: Vec<String> = (3..128).map(|n| n.to_string()).collect();
        format!(
            "fds=; sockets=; for n in {}; do if [ -e /dev/fd/$n ]; then fds=\"$fds $n\"; \
             if [ -S /dev/fd/$n ]; then sockets=\"$sockets $n\"; fi; fi; done; \
             echo \"fds:$fds\"; echo \"sockets:$sockets\"; env",
            numbers.join(" ")
        )
    }

    fn parse(output: &str) -> ChildReport {
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
        let (held, _peer) = inheritable_socket();
        let fd = held.as_raw_fd();
        let control = children.report(Spawner::Unguarded).await;
        assert!(control.sockets.contains(&fd), "{control:?}");
        for spawner in [Spawner::Pipe, Spawner::Pty, Spawner::Worker] {
            let report = children.report(spawner).await;
            assert!(!report.descriptors.contains(&fd), "{spawner:?}: {report:?}");
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
}
