//! libSystem state that macOS fork handlers need in the child.
//!
//! On macOS, `fork` runs libSystem's atfork child handlers in the child before it returns.
//! CodeSpace's spawns fork: a `pre_exec` step, such as #79's descriptor exclusion or the PTY
//! setup in portable-pty, takes std off its `posix_spawn` path. libnotify's handler,
//! `_notify_fork_child`, reaches libnotify's globals through their one-time initialization
//! (`os_alloc_once`). If another thread of the parent was inside that initialization when the
//! parent forked, the child finds the initialization gate held by a thread it does not have,
//! and aborts in `_os_once_gate_corruption_abort` before `exec`. std reads its exec-error pipe
//! closing without data as a successful exec, so the spawn succeeds, and the child is reported
//! as having exited without an exit code.
//!
//! [`prepare_fork_spawns`] completes libnotify's initialization by calling
//! `notify_is_valid_token`, which initializes libnotify's globals and then only looks the token
//! up in this process's registration table. Once complete, the initialization never runs again,
//! so no later fork copies it in progress:
//! - The gateway and the worker call it first in `main`, before any thread exists.
//! - The spawns that fork call it before they fork: [`crate::exclude_unrelated`] and its std
//!   counterpart do for the pipe, patch-helper, sandbox-helper and worker spawns, and the PTY
//!   spawn calls it itself. In a process whose `main` does not call it, such as a test binary,
//!   such a spawn waits for an initialization that another thread has in progress, instead of
//!   forking during it.
//!
//! This closes that window only. Forks by code that does not call it, before it is called, may
//! still copy the initialization in progress, and other lazily initialized libSystem state that
//! an atfork child handler touches is not covered. It does nothing on other platforms.

use std::sync::Once;

/// Complete libnotify's one-time initialization before this process forks (macOS).
pub fn prepare_fork_spawns() {
    static PREPARED: Once = Once::new();
    PREPARED.call_once(complete_notify_initialization);
}

#[cfg(target_os = "macos")]
fn complete_notify_initialization() {
    extern "C" {
        // <notify.h>, macOS 10.10: whether `token` is a registration of this process.
        fn notify_is_valid_token(token: libc::c_int) -> bool;
    }
    // SAFETY: notify_is_valid_token takes no pointer. It initializes libnotify's globals if they
    // are not, takes libnotify's lock, looks `token` up and returns.
    unsafe {
        notify_is_valid_token(0);
    }
}

#[cfg(not(target_os = "macos"))]
fn complete_notify_initialization() {}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use codespace_domain::{ProcessId, ProcessState, Profile, WorkspaceId};
    use codespace_policy::Workspace;

    use crate::{InProcessRunner, Runner, RunnerExecRequest};

    /// Set by a parent test so that its re-run copy performs the scenario.
    const SCENARIO: &str = "CODESPACE_ISOLATED_SCENARIO";

    /// Run `test` (a test name under `crate::`) alone in a fresh copy of this test binary, for
    /// a scenario that needs a process of its own.
    pub(crate) fn run_isolated(test: &str) {
        run_isolated_with(test, &[]);
    }

    /// [`run_isolated`] with extra environment for the scenario.
    pub(crate) fn run_isolated_with(test: &str, env: &[(&str, &str)]) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--test-threads=1", "--nocapture"])
            .env(SCENARIO, "1")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "scenario failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub(crate) fn in_isolated_copy() -> bool {
        std::env::var_os(SCENARIO).is_some()
    }

    /// libnotify's slot in libplatform's `_os_alloc_once_table`:
    /// `OS_ALLOC_ONCE_KEY_LIBSYSTEM_NOTIFY`, key 0 in Libsystem's `alloc_once_private.h`.
    #[cfg(target_os = "macos")]
    fn notify_once() -> (usize, usize) {
        // SAFETY: dlsym looks up a data symbol that libsystem_platform exports for its inline
        // `os_alloc_once`: an array of (once, pointer) word pairs that lives as long as the
        // process.
        let table = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_os_alloc_once_table".as_ptr()) };
        assert!(
            !table.is_null(),
            "libplatform no longer exports _os_alloc_once_table; re-check prepare_fork_spawns"
        );
        let slot = table.cast::<usize>();
        // SAFETY: the first two words of the table are slot 0. No other thread of this process
        // runs CodeSpace or libnotify code here.
        unsafe {
            (
                std::ptr::read_volatile(slot),
                std::ptr::read_volatile(slot.add(1)),
            )
        }
    }

    /// The initializer has returned: `once` is done (all bits set) or, on arm64, a quiescing
    /// generation (low bits 01). A thread port name (low bit set, bit 1 set) means in progress.
    #[cfg(target_os = "macos")]
    fn completed(once: usize) -> bool {
        once == usize::MAX || once & 3 == 1
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn preparing_completes_the_libnotify_initialization() {
        if !in_isolated_copy() {
            run_isolated("fork_handlers::tests::preparing_completes_the_libnotify_initialization");
            return;
        }
        let (once, _) = notify_once();
        assert_eq!(
            once, 0,
            "libnotify was initialized before CodeSpace code ran, so this cannot show that \
             prepare_fork_spawns initializes it"
        );
        prepare_fork_spawns();
        let (once, globals) = notify_once();
        assert!(
            completed(once),
            "initialization not complete: once={once:#x}"
        );
        assert_ne!(globals, 0, "libnotify's globals were not allocated");
        // Later calls change nothing.
        prepare_fork_spawns();
        assert_eq!(notify_once().1, globals);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn preparing_does_nothing_elsewhere() {
        prepare_fork_spawns();
        prepare_fork_spawns();
    }

    /// Address of libnotify's once word, for the atfork prepare handler.
    #[cfg(target_os = "macos")]
    static NOTIFY_ONCE_WORD: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    /// The once word when this process last forked; `NOT_FORKED` before.
    #[cfg(target_os = "macos")]
    static ONCE_AT_FORK: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(NOT_FORKED);
    #[cfg(target_os = "macos")]
    const NOT_FORKED: usize = usize::MAX - 2;

    /// Runs in the forking thread just before each fork of this process.
    #[cfg(target_os = "macos")]
    unsafe extern "C" fn record_notify_at_fork() {
        let word = NOTIFY_ONCE_WORD.load(Ordering::SeqCst) as *const usize;
        // SAFETY: the address of slot 0's once word, valid for the life of the process.
        let once = unsafe { std::ptr::read_volatile(word) };
        ONCE_AT_FORK.store(once, Ordering::SeqCst);
    }

    /// Each forking spawn path completes libnotify's initialization before it forks. An atfork
    /// prepare handler, which runs in the forking thread just before the fork, records
    /// libnotify's once word; every path runs in its own fresh process, where nothing has
    /// initialized libnotify yet. The worker spawn is checked in the gateway crate.
    #[cfg(target_os = "macos")]
    #[test]
    fn spawns_find_libnotify_initialized_when_they_fork() {
        const PATH: &str = "CODESPACE_ISOLATED_SPAWN_PATH";
        if !in_isolated_copy() {
            for path in ["pipe", "pty", "patch-helper"] {
                run_isolated_with(
                    "fork_handlers::tests::spawns_find_libnotify_initialized_when_they_fork",
                    &[(PATH, path)],
                );
            }
            return;
        }
        let path = std::env::var(PATH).unwrap();
        assert_eq!(
            notify_once().0,
            0,
            "libnotify was initialized before the spawn"
        );
        // SAFETY: as in `notify_once`; slot 0's once word is the first word of the table.
        let table = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_os_alloc_once_table".as_ptr()) };
        NOTIFY_ONCE_WORD.store(table as usize, Ordering::SeqCst);
        // SAFETY: the handler only reads one word and stores it in an atomic.
        assert_eq!(
            unsafe { libc::pthread_atfork(Some(record_notify_at_fork), None, None) },
            0
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(
            WorkspaceId("demo".into()),
            dir.path().to_path_buf(),
            Profile::WorkspaceWrite,
        );
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        runtime.block_on(async {
            match path.as_str() {
                "pipe" => runner_exits(&runner, &ws, &["/usr/bin/true"], false).await,
                "pty" => runner_exits(&runner, &ws, &["/usr/bin/true"], true).await,
                "patch-helper" => {
                    let status = crate::patch_helper::helper_command(Path::new("/usr/bin/true"))
                        .status()
                        .await
                        .unwrap();
                    assert!(status.success());
                }
                other => panic!("unknown path {other}"),
            }
        });
        let at_fork = ONCE_AT_FORK.load(Ordering::SeqCst);
        assert_ne!(at_fork, NOT_FORKED, "{path} did not fork");
        assert!(
            completed(at_fork),
            "{path} forked while libnotify's initialization was not complete: {at_fork:#x}"
        );
    }

    /// Write end of the [`ForkDetector`] pipe, read by the atfork child handler.
    static FORK_PIPE: AtomicI32 = AtomicI32::new(-1);

    /// Runs in every child that this process forks, before `fork` returns there.
    unsafe extern "C" fn note_fork_in_child() {
        let fd = FORK_PIPE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write(2) is async-signal-safe; it writes one static byte to the pipe.
            unsafe { libc::write(fd, b"f".as_ptr().cast(), 1) };
        }
    }

    /// Tells whether a spawn forked: a `pthread_atfork` child handler writes one byte to a pipe
    /// in every forked child, and `posix_spawn` runs no atfork handler. The registration is
    /// process-wide and cannot be undone, so only an isolated copy of the test binary installs it.
    pub(crate) struct ForkDetector {
        read: OwnedFd,
    }

    fn set_fd_flag(fd: libc::c_int, get: libc::c_int, set: libc::c_int, flag: libc::c_int) {
        // SAFETY: fcntl reads and sets flags of a descriptor this test owns.
        unsafe {
            let flags = libc::fcntl(fd, get);
            assert!(flags >= 0, "fcntl: {}", std::io::Error::last_os_error());
            assert_eq!(libc::fcntl(fd, set, flags | flag), 0);
        }
    }

    impl ForkDetector {
        pub(crate) fn install() -> Self {
            assert!(in_isolated_copy(), "pthread_atfork outlives the test");
            let mut fds = [-1; 2];
            // SAFETY: pipe stores two new descriptors in `fds`.
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            for fd in fds {
                set_fd_flag(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            set_fd_flag(fds[0], libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK);
            // The write end stays open for the life of this process.
            FORK_PIPE.store(fds[1], Ordering::SeqCst);
            // SAFETY: the handler only loads an atomic and writes to a pipe.
            assert_eq!(
                unsafe { libc::pthread_atfork(None, None, Some(note_fork_in_child)) },
                0
            );
            Self {
                // SAFETY: pipe returned this descriptor, owned here from now on.
                read: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            }
        }

        /// Whether `spawn`, which returns once its child has run or exec'd, forked.
        pub(crate) fn forks(&self, spawn: impl FnOnce()) -> bool {
            self.drain();
            spawn();
            self.drain() > 0
        }

        /// [`Self::forks`] for a spawn that has to be awaited.
        pub(crate) async fn forks_async(
            &self,
            spawn: impl std::future::Future<Output = ()>,
        ) -> bool {
            self.drain();
            spawn.await;
            self.drain() > 0
        }

        fn drain(&self) -> usize {
            let mut total = 0;
            let mut buf = [0u8; 64];
            loop {
                // SAFETY: read into a stack buffer from the non-blocking read end.
                let n = unsafe {
                    libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n <= 0 {
                    return total;
                }
                total += n as usize;
            }
        }
    }

    fn run_status(mut command: Command) {
        let status = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .expect("spawn");
        assert!(status.success(), "{status}");
    }

    async fn runner_exits(runner: &InProcessRunner, ws: &Workspace, argv: &[&str], tty: bool) {
        let process_id = ProcessId(format!("fork-detect-{}-{tty}", argv[0]));
        let mut request = RunnerExecRequest::for_host(
            argv.iter().map(|arg| arg.to_string()).collect(),
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        request.tty = tty;
        runner.exec(ws, request).await.expect("exec");
        for _ in 0..500 {
            let status = runner.process_status(&process_id).await.unwrap();
            if status.state == ProcessState::Exited {
                assert_eq!(status.exit_code, Some(0), "{argv:?} tty={tty}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{argv:?} tty={tty} did not exit");
    }

    /// Which spawn paths fork on this platform, with their controls. The pipe, PTY and patch
    /// helper spawns fork, so they depend on `prepare_fork_spawns`; the worker spawn is checked
    /// in the gateway crate.
    #[test]
    fn spawn_paths_that_fork() {
        if !in_isolated_copy() {
            run_isolated("fork_handlers::tests::spawn_paths_that_fork");
            return;
        }
        let detector = ForkDetector::install();

        // Controls: std's posix_spawn path runs no atfork handler; a no-op pre_exec forks.
        assert!(!detector.forks(|| run_status(Command::new("/usr/bin/true"))));
        assert!(detector.forks(|| {
            let mut command = Command::new("/usr/bin/true");
            // SAFETY: the step does nothing.
            unsafe { command.pre_exec(|| Ok(())) };
            run_status(command);
        }));
        // std also forks for a bare program name once the child's PATH is set or the
        // environment is cleared, as CodeSpace's spawns do.
        assert!(detector.forks(|| {
            let mut command = Command::new("true");
            command.env("PATH", "/usr/bin:/bin");
            run_status(command);
        }));
        assert!(detector.forks(|| {
            let mut command = Command::new("true");
            command.env_clear().env("PATH", "/usr/bin:/bin");
            run_status(command);
        }));
        assert!(!detector.forks(|| run_status(Command::new("true"))));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(
            WorkspaceId("demo".into()),
            dir.path().to_path_buf(),
            Profile::WorkspaceWrite,
        );
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        runtime.block_on(async {
            for argv in [&["/usr/bin/true"][..], &["true"]] {
                let pipe = detector.forks_async(runner_exits(&runner, &ws, argv, false));
                assert!(pipe.await, "pipe {argv:?}");
                let pty = detector.forks_async(runner_exits(&runner, &ws, argv, true));
                assert!(pty.await, "pty {argv:?}");
            }
            let helper = detector.forks_async(async {
                let status = crate::patch_helper::helper_command(Path::new("/usr/bin/true"))
                    .status()
                    .await
                    .unwrap();
                assert!(status.success());
            });
            assert!(helper.await, "patch helper");
        });
    }
}
