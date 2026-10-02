//! Isolated runner worker. Binds the given `$dir/runner.sock` (or a
//! newly allocated unique dir when run standalone). Parent chmod is the
//! gateway's job: this process never calls
//! `prepare_private_socket_directory`. One `InProcessRunner` per
//! process. P0 is 1:1 — serve one connection, then exit.
//!
//! Built with the `devguard` feature, the worker is CodeSpace's execution owner in DevGuard
//! (CSRG-U2). A gateway started with `--devguard register` passes, after the socket, the
//! authority's socket, the consumer and its generation, and the number of a descriptor carrying
//! the consumer secret, never the secret itself. The worker consumes and closes that descriptor
//! before anything else, registers itself in the background and answers the gateway's
//! `Registration` requests from then on.

use std::path::PathBuf;

#[cfg(not(feature = "devguard"))]
use codespace_runner::serve_runner_connection;
#[cfg(feature = "devguard")]
use codespace_runner::serve_runner_connection_with_registration;
use codespace_runner::{
    allocate_private_runner_dir, host_worker, reclaim_leftover_socket, runner_socket_path,
};

#[tokio::main]
async fn main() {
    codex_process_hardening::pre_main_hardening();
    // First of all, so the handed descriptor is closed before this process can start a child.
    #[cfg(feature = "devguard")]
    let registration = devguard::start(std::env::args_os().skip(2));
    let socket = match std::env::args().nth(1) {
        Some(path) => PathBuf::from(path),
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
    let _ = serve_runner_connection_with_registration(stream, runner, events, registration).await;
    #[cfg(not(feature = "devguard"))]
    let _ = serve_runner_connection(stream, runner, events).await;
}

#[cfg(feature = "devguard")]
mod devguard {
    use std::ffi::OsString;
    use std::os::fd::RawFd;
    use std::path::PathBuf;
    use std::sync::Arc;

    use codespace_runner::registration::{
        handoff, Owner, OwnerCredential, OwnerSettings, ResourceOwner,
    };
    use codespace_runner::OwnerRegistration;

    /// The worker's DevGuard settings after its socket. None of them is a secret.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(crate) struct Arguments {
        pub(crate) socket: Option<PathBuf>,
        pub(crate) consumer: Option<String>,
        pub(crate) generation: Option<String>,
        pub(crate) credential_fd: Option<RawFd>,
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
                _ => return Err(format!("unknown argument {flag}")),
            }
        }
        Ok(parsed)
    }

    /// Consume the handed descriptor, then register in the background. `None` when the worker
    /// was started without DevGuard settings: it then answers that its mode cannot register.
    pub(crate) fn start(
        args: impl IntoIterator<Item = OsString>,
    ) -> Option<Arc<OwnerRegistration>> {
        let arguments = match parse(args) {
            Ok(arguments) => arguments,
            Err(problem) => {
                eprintln!("codespace-codex-runtime: {problem}");
                std::process::exit(2);
            }
        };
        let credential = match arguments.credential_fd {
            // SAFETY: the gateway started this process with this dedicated descriptor, which
            // nothing in this process has taken yet.
            Some(fd) => unsafe { handoff::receive(fd) }
                .map(OwnerCredential::Handed)
                .unwrap_or(OwnerCredential::Unavailable),
            None => OwnerCredential::Unavailable,
        };
        let (Some(socket), Some(consumer), Some(generation)) =
            (arguments.socket, arguments.consumer, arguments.generation)
        else {
            return None;
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
        Some(registration)
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
            ]))
            .unwrap();
            assert_eq!(
                parsed,
                Arguments {
                    socket: Some("/private/tmp/devguard-501/authority.sock".into()),
                    consumer: Some("codespace".into()),
                    generation: Some("g1".into()),
                    credential_fd: Some(3),
                }
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
