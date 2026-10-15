//! Isolated runner worker. Binds the given `$dir/runner.sock` (or a
//! newly allocated unique dir when run standalone). Parent chmod is the
//! gateway's job: this process never calls
//! `prepare_private_socket_directory`. One `InProcessRunner` per
//! process. By default (`--lifecycle managed`) it is 1:1 — serve one
//! connection, then exit.
//!
//! Built with the `devguard` feature, the worker is CodeSpace's execution owner in DevGuard
//! (CSRG-U2). A gateway started with `--devguard register` passes, after the socket, the
//! authority's socket, the consumer and its generation, and the number of a descriptor carrying
//! the consumer secret, never the secret itself. The worker consumes and closes that descriptor
//! before anything else, registers itself in the background and answers the gateway's
//! `Registration` requests from then on. With `--devguard-launch-helper` too, it launches the
//! governed executions it prepares through that helper, as their helper's parent (CSRG-U4).
//!
//! With `--lifecycle independent` (P1R-C01) an operator or a harness starts the worker, not a
//! gateway: it is an independent Runner. It prepares its socket's directory itself, created
//! with mode 0700 or accepted only as a private directory of its user, and serves every
//! connection it accepts, of which one at a time, after its `Claim`, controls it (P1R-C03).
//! It logs each Claim with the process ID of its peer. Its processes, their output and their
//! timeouts outlive each connection until its controller's `Stop`, or the Runner's own SIGTERM
//! or SIGINT, ends them and the Runner. With DevGuard it reads the consumer secret from
//! `--devguard-credential-file`, as no gateway hands it one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(not(feature = "devguard"))]
use codespace_runner::serve_runner_connection;
#[cfg(feature = "devguard")]
use codespace_runner::serve_runner_connection_with_registration;
use codespace_runner::{
    allocate_private_runner_dir, host_worker, prepare_runner_socket_dir, reclaim_leftover_socket,
    runner_socket_path, ClaimNote, IndependentRunner, RunnerLifecycle, Served, STOP_DRAIN_BOUND,
};

fn main() {
    // First, while this is the only thread: no later fork can then copy libnotify's
    // initialization in progress (see `prepare_fork_spawns`).
    codespace_runner::prepare_fork_spawns();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed building the Runtime")
        .block_on(serve());
}

/// The worker's command line: `[SOCKET] [--lifecycle managed|independent] [--devguard-* VALUE]...`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Arguments {
    socket: Option<PathBuf>,
    lifecycle: RunnerLifecycle,
    /// The DevGuard settings, in order. A worker built without DevGuard ignores them, as it
    /// always has.
    devguard: Vec<OsString>,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut parsed = Arguments::default();
    let mut args = args.into_iter().peekable();
    if args
        .peek()
        .is_some_and(|first| !first.to_string_lossy().starts_with("--"))
    {
        parsed.socket = args.next().map(PathBuf::from);
    }
    while let Some(flag) = args.next() {
        let name = flag.to_str().ok_or("a non-UTF-8 argument")?.to_owned();
        let value = args.next().ok_or_else(|| format!("{name} needs a value"))?;
        match name.as_str() {
            "--lifecycle" => {
                parsed.lifecycle = match value.to_str() {
                    Some("managed") => RunnerLifecycle::Managed,
                    Some("independent") => RunnerLifecycle::Independent,
                    _ => return Err("--lifecycle must be managed or independent".into()),
                }
            }
            devguard if devguard.starts_with("--devguard-") => {
                parsed.devguard.push(flag);
                parsed.devguard.push(value);
            }
            _ => return Err(format!("unknown argument {name}")),
        }
    }
    Ok(parsed)
}

/// Report a problem with the worker's settings or socket and exit with `code`.
fn refuse(code: i32, problem: impl std::fmt::Display) -> ! {
    eprintln!("codespace-codex-runtime: {problem}");
    std::process::exit(code);
}

async fn serve() {
    codex_process_hardening::pre_main_hardening();
    let arguments = parse(std::env::args_os().skip(1)).unwrap_or_else(|problem| refuse(2, problem));
    // First of all, so the handed descriptor is closed before this process can start a child.
    #[cfg(feature = "devguard")]
    let (registration, launcher) = devguard::start(arguments.devguard, arguments.lifecycle);
    #[cfg(not(feature = "devguard"))]
    let _ = arguments.devguard;
    match arguments.lifecycle {
        RunnerLifecycle::Managed => {
            let socket = match arguments.socket {
                Some(path) => path,
                None => match std::env::var_os("CODESPACE_RUNNER_SOCKET") {
                    Some(path) => PathBuf::from(path),
                    None => {
                        let dir = allocate_private_runner_dir(None).expect("private runner dir");
                        let socket = runner_socket_path(&dir);
                        eprintln!("codespace-codex-runtime listening on {}", socket.display());
                        socket
                    }
                },
            };
            reclaim_leftover_socket(&socket).expect("runner socket");
            let mut listener = codex_uds::UnixListener::bind(&socket)
                .await
                .expect("bind runner socket");
            let (runner, events) = host_worker();
            let stream = listener.accept().await.expect("accept runner socket");
            // Keep the listener bound so a live connect-probe sees AddrInUse
            // instead of unlinking this rendezvous.
            let _listener = listener;
            #[cfg(feature = "devguard")]
            let _ = serve_runner_connection_with_registration(
                stream,
                runner,
                events,
                registration,
                launcher,
            )
            .await;
            #[cfg(not(feature = "devguard"))]
            let _ = serve_runner_connection(stream, runner, events).await;
        }
        RunnerLifecycle::Independent => {
            let socket = arguments.socket.unwrap_or_else(|| {
                refuse(2, "--lifecycle independent needs the socket's path first")
            });
            let (runner, events) = host_worker();
            let independent = IndependentRunner::new(runner, events)
                .with_claim_observer(std::sync::Arc::new(log_claim));
            #[cfg(feature = "devguard")]
            let independent = independent.with_registration(registration, launcher);
            std::process::exit(serve_independent(&socket, independent).await);
        }
    }
}

/// What an independent Runner logs of each Claim (P1R-C03, P1R-C04): any process of its user
/// that can reach its socket may claim it while no connection controls it.
fn log_claim(note: ClaimNote) {
    let peer = |pid: Option<i32>| pid.map_or_else(|| "unknown".into(), |pid| pid.to_string());
    match note {
        ClaimNote::Claimed {
            epoch,
            previous_controller,
            peer_pid,
        } => eprintln!(
            "codespace-codex-runtime: process {} claimed the Runner under epoch {epoch}{}",
            peer(peer_pid),
            if previous_controller {
                "; an earlier controller started work, so it starts nothing where that \
                 controller did until it reconciled it"
            } else {
                ""
            }
        ),
        ClaimNote::Refused {
            refusal,
            message,
            peer_pid,
        } => eprintln!(
            "codespace-codex-runtime: refused the Claim of process {} ({}): {message}",
            peer(peer_pid),
            refusal.as_str()
        ),
    }
}

/// An independent Runner's life (P1R-C01): its socket, the connections it serves, and its
/// stop, after which its socket, and the directory it created for it, are removed. The exit
/// code.
async fn serve_independent(socket: &Path, independent: IndependentRunner) -> i32 {
    let dir = match socket.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => refuse(2, "the socket's path needs a directory"),
    };
    let created = prepare_runner_socket_dir(dir).unwrap_or_else(|err| refuse(1, err));
    let rendezvous = Rendezvous {
        socket: socket.to_path_buf(),
        dir: created.then(|| dir.to_path_buf()),
    };
    if let Err(err) = reclaim_leftover_socket(socket) {
        rendezvous.remove_dir();
        refuse(1, err);
    }
    // Tokio's listener, as codex-uds binds on Unix: a Claim needs its peer's credentials from
    // the accepted stream (P1R-C03).
    let listener = match tokio::net::UnixListener::bind(socket) {
        Ok(listener) => listener,
        Err(err) => {
            rendezvous.remove_dir();
            refuse(1, format!("bind {}: {err}", socket.display()));
        }
    };
    let stop = independent.stop_handle();
    let mut terminate = signal(tokio::signal::unix::SignalKind::terminate());
    let mut interrupt = signal(tokio::signal::unix::SignalKind::interrupt());
    let watchdog = Rendezvous {
        socket: rendezvous.socket.clone(),
        dir: rendezvous.dir.clone(),
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        // Its own signal stops the Runner as a gateway's `Stop` does.
        stop.request();
        // A connection whose gateway no longer reads cannot keep the Runner from stopping:
        // its processes are already terminated.
        tokio::time::sleep(STOP_DRAIN_BOUND * 2).await;
        eprintln!("codespace-codex-runtime: the connection did not end; stopping without it");
        watchdog.remove();
        std::process::exit(1);
    });
    eprintln!(
        "codespace-codex-runtime: independent Runner {} listening on {}",
        std::process::id(),
        socket.display()
    );
    let listener = std::sync::Arc::new(listener);
    let terminated = independent
        .run(
            move || {
                let listener = listener.clone();
                async move { listener.accept().await.map(|(stream, _)| stream) }
            },
            |ending| match ending {
                Ok(Served::Detached) => eprintln!(
                    "codespace-codex-runtime: the controlling gateway detached; its processes go on"
                ),
                Ok(Served::Lost) => eprintln!(
                    "codespace-codex-runtime: the controlling gateway's connection ended without a \
                     detach or a stop; its processes go on"
                ),
                Ok(Served::Stopped { .. }) => {}
                Err(err) => eprintln!("codespace-codex-runtime: accept: {err}"),
            },
        )
        .await;
    eprintln!("codespace-codex-runtime: stopped; {terminated} processes were terminated");
    rendezvous.remove();
    0
}

fn signal(kind: tokio::signal::unix::SignalKind) -> tokio::signal::unix::Signal {
    tokio::signal::unix::signal(kind).unwrap_or_else(|err| refuse(1, format!("signals: {err}")))
}

/// An independent Runner's socket, and its directory when the Runner created it.
struct Rendezvous {
    socket: PathBuf,
    dir: Option<PathBuf>,
}

impl Rendezvous {
    fn remove(&self) {
        let _ = std::fs::remove_file(&self.socket);
        self.remove_dir();
    }

    /// Only a directory the Runner created, and only while it is empty.
    fn remove_dir(&self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_socket_comes_first_and_the_lifecycle_defaults_to_managed() {
        assert_eq!(parse(args(&[])).unwrap(), Arguments::default());
        assert_eq!(
            parse(args(&["/run/cs/runner.sock", "--lifecycle", "independent"])).unwrap(),
            Arguments {
                socket: Some("/run/cs/runner.sock".into()),
                lifecycle: RunnerLifecycle::Independent,
                devguard: Vec::new(),
            }
        );
        assert_eq!(
            parse(args(&[
                "/run/cs/runner.sock",
                "--devguard-consumer",
                "codespace",
                "--lifecycle",
                "managed",
            ]))
            .unwrap(),
            Arguments {
                socket: Some("/run/cs/runner.sock".into()),
                lifecycle: RunnerLifecycle::Managed,
                devguard: args(&["--devguard-consumer", "codespace"]),
            }
        );
        for wrong in [
            &["--lifecycle", "detached"][..],
            &["/run/cs/runner.sock", "--lifecycle"],
            &["/run/cs/runner.sock", "--stop", "now"],
            &["/run/cs/runner.sock", "/run/cs/other.sock"],
        ] {
            assert!(parse(args(wrong)).is_err(), "{wrong:?}");
        }
    }
}

#[cfg(feature = "devguard")]
mod devguard {
    use std::ffi::OsString;
    use std::os::fd::RawFd;
    use std::path::PathBuf;
    use std::sync::Arc;

    use codespace_runner::launch::Launcher;
    use codespace_runner::registration::{
        handoff, Owner, OwnerCredential, OwnerSettings, ResourceOwner,
    };
    use codespace_runner::{OwnerRegistration, RunnerLifecycle};

    /// The worker's DevGuard settings after its socket. None of them is a secret.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(crate) struct Arguments {
        pub(crate) socket: Option<PathBuf>,
        pub(crate) consumer: Option<String>,
        pub(crate) generation: Option<String>,
        pub(crate) credential_fd: Option<RawFd>,
        /// An independent Runner's consumer secret, which it reads from this private file
        /// itself (P1R-C01).
        pub(crate) credential_file: Option<PathBuf>,
        pub(crate) launch_helper: Option<PathBuf>,
    }

    pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
        let mut parsed = Arguments::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| "a non-UTF-8 argument")?;
            let value = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--devguard-socket" => parsed.socket = Some(value.into()),
                "--devguard-consumer" => parsed.consumer = Some(value),
                "--devguard-generation" => parsed.generation = Some(value),
                "--devguard-credential-fd" => {
                    parsed.credential_fd = Some(
                        value
                            .parse()
                            .map_err(|_| format!("{flag} needs a number"))?,
                    )
                }
                "--devguard-credential-file" => parsed.credential_file = Some(value.into()),
                "--devguard-launch-helper" => parsed.launch_helper = Some(value.into()),
                _ => return Err(format!("unknown argument {flag}")),
            }
        }
        Ok(parsed)
    }

    /// Consume the handed descriptor, then register in the background. No registration when
    /// the worker was started without DevGuard settings: it then answers that its mode cannot
    /// register. A launch helper only with a registration. A managed worker gets its secret
    /// through the descriptor from the gateway that starts it; an independent Runner reads it
    /// from its file (P1R-C01).
    pub(crate) fn start(
        args: impl IntoIterator<Item = OsString>,
        lifecycle: RunnerLifecycle,
    ) -> (Option<Arc<OwnerRegistration>>, Option<Launcher>) {
        let arguments = match parse(args) {
            Ok(arguments) => arguments,
            Err(problem) => {
                eprintln!("codespace-codex-runtime: {problem}");
                std::process::exit(2);
            }
        };
        let credential = match (
            lifecycle,
            arguments.credential_fd,
            arguments.credential_file,
        ) {
            // SAFETY: the gateway started this process with this dedicated descriptor, which
            // nothing in this process has taken yet.
            (RunnerLifecycle::Managed, Some(fd), None) => unsafe { handoff::receive(fd) }
                .map(OwnerCredential::Handed)
                .unwrap_or(OwnerCredential::Unavailable),
            (RunnerLifecycle::Independent, None, Some(file)) => OwnerCredential::File(file),
            (_, None, None) => OwnerCredential::Unavailable,
            (RunnerLifecycle::Managed, ..) => {
                eprintln!(
                    "codespace-codex-runtime: a worker its gateway starts gets the consumer \
                     secret through --devguard-credential-fd only"
                );
                std::process::exit(2);
            }
            (RunnerLifecycle::Independent, ..) => {
                eprintln!(
                    "codespace-codex-runtime: an independent Runner reads the consumer secret \
                     from --devguard-credential-file only"
                );
                std::process::exit(2);
            }
        };
        let launcher = match arguments.launch_helper.map(Launcher::new) {
            Some(Err(problem)) => {
                eprintln!("codespace-codex-runtime: --devguard-launch-helper: {problem}");
                std::process::exit(2);
            }
            Some(Ok(launcher)) => Some(launcher),
            None => None,
        };
        let (Some(socket), Some(consumer), Some(generation)) =
            (arguments.socket, arguments.consumer, arguments.generation)
        else {
            return (None, None);
        };
        let registration = OwnerRegistration::new(
            Owner::new(
                OwnerSettings {
                    socket,
                    consumer,
                    generation,
                },
                credential,
            ),
            ResourceOwner::Worker,
        );
        let eager = registration.clone();
        tokio::spawn(async move { eager.report().await });
        (Some(registration), launcher)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(list: &[&str]) -> Vec<OsString> {
            list.iter().map(OsString::from).collect()
        }

        #[test]
        fn settings_follow_the_socket_and_carry_no_secret() {
            let parsed = parse(args(&[
                "--devguard-socket",
                "/private/tmp/devguard-501/authority.sock",
                "--devguard-consumer",
                "codespace",
                "--devguard-generation",
                "g1",
                "--devguard-credential-fd",
                "3",
                "--devguard-launch-helper",
                "/opt/devguard/bin/devguard-launch",
            ]))
            .unwrap();
            assert_eq!(
                parsed,
                Arguments {
                    socket: Some("/private/tmp/devguard-501/authority.sock".into()),
                    consumer: Some("codespace".into()),
                    generation: Some("g1".into()),
                    credential_fd: Some(3),
                    credential_file: None,
                    launch_helper: Some("/opt/devguard/bin/devguard-launch".into()),
                }
            );
            assert_eq!(
                parse(args(&[
                    "--devguard-credential-file",
                    "/Users/op/.config/codespace/consumer.secret"
                ]))
                .unwrap()
                .credential_file,
                Some("/Users/op/.config/codespace/consumer.secret".into())
            );
            assert_eq!(parse(args(&[])).unwrap(), Arguments::default());
            for wrong in [
                &["--devguard-secret", "x"][..],
                &["--devguard-credential-fd", "three"],
                &["--devguard-socket"],
            ] {
                assert!(parse(args(wrong)).is_err(), "{wrong:?}");
            }
        }
    }
}
