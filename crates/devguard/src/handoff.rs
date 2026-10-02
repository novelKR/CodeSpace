//! The UDS worker's credential handoff (CSRG-U2).
//!
//! The gateway reads the consumer secret from its private file, as a status probe does, into
//! DevGuard's `CredentialHandoff`: a socket pair whose writing end is already closed and whose
//! reading end is close-on-exec in the gateway. [`PreparedHandoff::attach`] gives that end to
//! one child: only that child clears close-on-exec, after its other descriptors have been
//! excluded, and the gateway's copy closes when the command is dropped after the spawn. The
//! worker passes the descriptor's number, never the secret, as an argument, and [`receive`]
//! consumes and closes the descriptor at its startup, before it serves anything.
//!
//! The secret never enters an argument, the environment, a log or a status, and no other child
//! of either process inherits it.

use std::fmt;
use std::os::fd::RawFd;
use std::path::Path;

use devguard_client::credential::{take_inherited, CredentialHandoff};
use devguard_contract::Secret;

use crate::read_secret;

/// A consumer secret this process was handed at startup.
pub struct HandedCredential(Secret);

impl HandedCredential {
    pub(crate) fn secret(&self) -> &Secret {
        &self.0
    }
}

impl fmt::Debug for HandedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HandedCredential([REDACTED])")
    }
}

/// A carrier for one child: the reading end of a socket pair holding the consumer secret.
pub struct PreparedHandoff(CredentialHandoff);

impl fmt::Debug for PreparedHandoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PreparedHandoff(fd {})", self.0.descriptor())
    }
}

/// Read the consumer secret from its private file, under the same rules as a status probe,
/// into a carrier. `None` when the file is not a private file of this user holding a secret.
pub fn prepare(credential_file: &Path) -> Option<PreparedHandoff> {
    let secret = read_secret(credential_file).ok()?;
    CredentialHandoff::new(&secret).ok().map(PreparedHandoff)
}

impl PreparedHandoff {
    /// The descriptor number the child inherits.
    pub fn descriptor(&self) -> RawFd {
        self.0.descriptor()
    }

    /// Give the carrier to the child of `command`. The command owns it until dropped, and only
    /// its child clears close-on-exec. Register this after any step that excludes the child's
    /// unrelated descriptors: `pre_exec` steps run in the order they were added.
    pub fn attach(self, command: &mut std::process::Command) -> RawFd {
        self.0.attach(command)
    }
}

/// Consume and close the descriptor this process was handed at startup. `None` if it does not
/// carry a secret within DevGuard's 250 ms read deadline; the descriptor is closed either way.
///
/// # Safety
/// `fd` must be the dedicated descriptor this process was started with, with no other owner.
pub unsafe fn receive(fd: RawFd) -> Option<HandedCredential> {
    // SAFETY: the caller's precondition is `take_inherited`'s.
    unsafe { take_inherited(fd) }.ok().map(HandedCredential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{private_file, short_directory};
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    const MODE: &str = "CODESPACE_DEVGUARD_HANDOFF_MODE";
    const FD: &str = "CODESPACE_DEVGUARD_HANDOFF_FD";
    const DIGEST: &str = "CODESPACE_DEVGUARD_HANDOFF_DIGEST";
    const SECRET: &str = "4a4d0ff04a4d0ff04a4d0ff04a4d0ff04a4d0ff04a4d0ff04a4d0ff04a4d0ff0";

    /// This test binary again, running only the ignored test `test`.
    fn child(test: &str, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                test,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MODE, mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn closed(fd: RawFd) -> bool {
        // SAFETY: F_GETFD only inspects the descriptor number.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        flags == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
    }

    fn mentions_secret(text: &str) -> bool {
        text.contains(SECRET)
    }

    #[test]
    fn an_unusable_credential_file_prepares_no_handoff() {
        let dir = short_directory();
        let shared = private_file(dir.path(), "shared.secret", SECRET.as_bytes());
        std::fs::set_permissions(&shared, std::os::unix::fs::PermissionsExt::from_mode(0o640))
            .unwrap();
        let real = private_file(dir.path(), "real.secret", SECRET.as_bytes());
        let linked = dir.path().join("linked.secret");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let short = private_file(dir.path(), "short.secret", &SECRET.as_bytes()[..63]);
        for file in [dir.path().join("missing.secret"), shared, linked, short] {
            assert!(prepare(&file).is_none(), "{file:?}");
        }
        assert!(prepare(&real).is_some());
    }

    #[test]
    fn the_child_alone_receives_the_secret_and_closes_its_descriptor() {
        let dir = short_directory();
        let file = private_file(dir.path(), "consumer.secret", SECRET.as_bytes());
        let handoff = prepare(&file).unwrap();
        assert!(!mentions_secret(&format!("{handoff:?}")));
        let mut command = child("handoff::tests::handoff_receiver", "receiver");
        let raw = handoff.attach(&mut command);
        assert!(raw >= 3);
        // SAFETY: F_GETFD reads the flags of the descriptor the command owns.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "the parent's copy"
        );
        command
            .env(FD, raw.to_string())
            .env(DIGEST, devguard_contract::digest_bytes(SECRET.as_bytes()));
        assert!(!mentions_secret(&format!("{command:?}")));
        let output = command.output().unwrap();
        drop(command);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(output.status.success(), "{stdout}{stderr}");
        assert!(
            stdout.contains("payload has no credential descriptor"),
            "{stdout}"
        );
        assert!(!mentions_secret(&stdout) && !mentions_secret(&stderr));
    }

    #[test]
    #[ignore = "run with a handed descriptor by its parent test"]
    fn handoff_receiver() {
        assert_eq!(std::env::var(MODE).unwrap(), "receiver");
        let fd: RawFd = std::env::var(FD).unwrap().parse().unwrap();
        // SAFETY: this process was started with this dedicated descriptor and nothing owns it.
        let handed = unsafe { receive(fd) }.unwrap();
        assert!(closed(fd), "the received descriptor is closed");
        let secret = handed.secret().expose();
        assert_eq!(
            devguard_contract::digest_bytes(secret.as_bytes()),
            std::env::var(DIGEST).unwrap()
        );
        assert!(!format!("{handed:?}").contains(secret));
        assert!(!std::env::args_os().any(|arg| arg.to_string_lossy().contains(secret)));
        assert!(!std::env::vars_os().any(|(key, value)| {
            key.to_string_lossy().contains(secret) || value.to_string_lossy().contains(secret)
        }));
        // A child started after the handoff inherits no credential descriptor.
        let error = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handoff::tests::handoff_payload",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MODE, "payload")
            .exec();
        panic!("could not exec the payload check: {error}");
    }

    #[test]
    #[ignore = "exec target of the handoff test"]
    fn handoff_payload() {
        assert_eq!(std::env::var(MODE).unwrap(), "payload");
        let fd: RawFd = std::env::var(FD).unwrap().parse().unwrap();
        assert!(closed(fd));
        println!("payload has no credential descriptor");
    }

    #[test]
    fn a_descriptor_without_a_secret_is_refused_and_closed() {
        let output = child("handoff::tests::handoff_refuser", "refuser")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("refused and closed: 3"));
    }

    #[test]
    #[ignore = "run in a single-threaded process by its parent test"]
    fn handoff_refuser() {
        assert_eq!(std::env::var(MODE).unwrap(), "refuser");
        let mut refused = 0;
        // Too short, too long and not a secret at all.
        for contents in [&SECRET.as_bytes()[..63], &[b'a'; 65][..], &[b'Z'; 64][..]] {
            let (reader, mut writer) = UnixStream::pair().unwrap();
            std::io::Write::write_all(&mut writer, contents).unwrap();
            drop(writer);
            let fd = reader.into_raw_fd();
            // SAFETY: this test owns `fd`, which no Rust object owns any more.
            assert!(unsafe { receive(fd) }.is_none());
            assert!(closed(fd));
            refused += 1;
        }
        // A descriptor that is not a carrier at all.
        let null = std::fs::File::open("/dev/null").unwrap();
        let fd = null.as_raw_fd();
        std::mem::forget(null);
        // SAFETY: as above; `null` was forgotten, so only `receive` owns `fd`.
        assert!(unsafe { receive(fd) }.is_none());
        assert!(closed(fd));
        println!("refused and closed: {refused}");
    }
}
