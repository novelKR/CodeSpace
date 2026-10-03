//! Gateway-owned Unix worker. One child, one private dir, one socket.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use codespace_runner::{allocate_private_runner_dir, runner_socket_path, ShellRelease, UdsRunner};
use codespace_store::Store;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

pub struct RuntimeProcess {
    dir: PathBuf,
    socket: PathBuf,
    shutdown: Arc<Notify>,
    wait: Option<JoinHandle<()>>,
}

impl RuntimeProcess {
    pub async fn spawn(
        bin: &Path,
        runner_dir: Option<&Path>,
        on_process_exit: ShellRelease,
        store: Arc<Store>,
    ) -> Result<(Self, UdsRunner)> {
        let dir = allocate_private_runner_dir(runner_dir)
            .map_err(|err| anyhow!("private runner dir: {err}"))?;
        let socket = runner_socket_path(&dir);
        let mut child = worker_command(bin, &socket)
            .spawn()
            .with_context(|| format!("spawn {}", bin.display()))?;
        let shutdown = Arc::new(Notify::new());
        let wait_shutdown = shutdown.clone();
        let wait_store = store.clone();
        let wait = tokio::spawn(async move {
            tokio::select! {
                _ = child.wait() => {}
                _ = wait_shutdown.notified() => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            }
            wait_store.release_all_processes();
        });
        let stream = wait_for_socket(&socket, &wait).await?;
        let kill = shutdown.clone();
        let runner = UdsRunner::from_stream_with_disconnect(
            stream,
            on_process_exit,
            Arc::new(move || {
                kill.notify_one();
            }),
        );
        runner
            .handshake()
            .await
            .map_err(|err| anyhow!("runner hello: {err}"))?;
        Ok((
            Self {
                dir,
                socket,
                shutdown,
                wait: Some(wait),
            },
            runner,
        ))
    }

    pub async fn connect_existing(
        socket: &Path,
        on_process_exit: ShellRelease,
        store: Arc<Store>,
    ) -> Result<UdsRunner> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect {}", socket.display()))?;
        let runner = UdsRunner::from_stream_with_disconnect(
            stream,
            on_process_exit,
            Arc::new(move || {
                store.release_all_processes();
            }),
        );
        runner
            .handshake()
            .await
            .map_err(|err| anyhow!("runner hello: {err}"))?;
        Ok(runner)
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub async fn wait_exit(&mut self) {
        self.shutdown.notify_one();
        if let Some(wait) = self.wait.take() {
            let _ = wait.await;
        }
    }
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        self.shutdown.notify_one();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The worker gets the socket path, a null stdin, the Gateway's stdout and stderr, and no other
/// descriptor of the Gateway's.
pub(crate) fn worker_command(bin: &Path, socket: &Path) -> Command {
    let mut command = Command::new(bin);
    command.arg(socket).stdin(Stdio::null()).kill_on_drop(true);
    codespace_runner::exclude_unrelated(&mut command);
    command
}

async fn wait_for_socket(socket: &Path, wait: &JoinHandle<()>) -> Result<UnixStream> {
    for _ in 0..100 {
        if wait.is_finished() {
            return Err(anyhow!("worker exited before the runner socket was ready"));
        }
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    Err(anyhow!("worker socket not ready at {}", socket.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_store::Store;
    use std::sync::Arc;

    fn runtime_bin() -> Option<PathBuf> {
        std::env::var_os("CODESPACE_RUNTIME_BIN").map(PathBuf::from)
    }

    #[tokio::test]
    async fn worker_keeps_only_its_standard_descriptors() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        // The server crate has no libc dependency; F_DUPFD is 0 on macOS and Linux.
        extern "C" {
            fn fcntl(fd: std::os::raw::c_int, cmd: std::os::raw::c_int, ...)
                -> std::os::raw::c_int;
        }
        // A descriptor that another thread's creation window could leave inheritable (#79).
        let null = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD returns a new descriptor without close-on-exec, owned below.
        let held = unsafe { fcntl(null.as_raw_fd(), 0, 400) };
        assert!(held >= 400, "F_DUPFD");
        // SAFETY: as above.
        let held = unsafe { OwnedFd::from_raw_fd(held) };
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report");
        // `/bin/sh` reads the script given where the socket path goes; executing a freshly
        // written file could meet ETXTBSY from a concurrent fork.
        let script = dir.path().join("worker.sh");
        std::fs::write(
            &script,
            format!(
                "for n in 1 {}; do if [ -e /dev/fd/$n ]; then echo held; else echo clear; fi; done > '{}'\n",
                held.as_raw_fd(),
                report.display()
            ),
        )
        .unwrap();
        let status = worker_command(Path::new("/bin/sh"), &script)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let seen = std::fs::read_to_string(&report).unwrap();
        assert_eq!(seen, "held\nclear\n");
    }

    /// What the scenario below needs from libc, which this crate does not depend on.
    mod sys {
        use std::os::raw::{c_int, c_void};

        pub const F_GETFD: c_int = 1;
        pub const F_SETFD: c_int = 2;
        pub const F_GETFL: c_int = 3;
        pub const F_SETFL: c_int = 4;
        pub const FD_CLOEXEC: c_int = 1;
        #[cfg(target_os = "macos")]
        pub const O_NONBLOCK: c_int = 0x4;
        #[cfg(target_os = "linux")]
        pub const O_NONBLOCK: c_int = 0o4000;
        #[cfg(target_os = "macos")]
        pub const RLIMIT_NOFILE: c_int = 8;
        #[cfg(target_os = "linux")]
        pub const RLIMIT_NOFILE: c_int = 7;

        #[repr(C)]
        pub struct Rlimit {
            pub cur: u64,
            pub max: u64,
        }

        extern "C" {
            pub fn pipe(fds: *mut c_int) -> c_int;
            pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
            pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
            pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
            pub fn getrlimit(resource: c_int, limit: *mut Rlimit) -> c_int;
            pub fn setrlimit(resource: c_int, limit: *const Rlimit) -> c_int;
            pub fn getdtablesize() -> c_int;
            pub fn pthread_atfork(
                prepare: Option<unsafe extern "C" fn()>,
                parent: Option<unsafe extern "C" fn()>,
                child: Option<unsafe extern "C" fn()>,
            ) -> c_int;
            #[cfg(target_os = "macos")]
            pub fn dlsym(handle: *mut c_void, symbol: *const std::os::raw::c_char) -> *mut c_void;
        }

        #[cfg(target_os = "macos")]
        pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    }

    const SCENARIO: &str = "CODESPACE_ISOLATED_SCENARIO";
    static FORK_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

    /// Address of libnotify's once word (`_os_alloc_once_table` slot 0) on macOS.
    static NOTIFY_ONCE_WORD: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    /// The once word when this process last forked.
    static ONCE_AT_FORK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Runs in the forking thread just before each fork of this process.
    unsafe extern "C" fn record_notify_at_fork() {
        let word = NOTIFY_ONCE_WORD.load(std::sync::atomic::Ordering::SeqCst) as *const usize;
        if !word.is_null() {
            // SAFETY: the address of slot 0's once word, valid for the life of the process.
            let once = unsafe { std::ptr::read_volatile(word) };
            ONCE_AT_FORK.store(once, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Runs in every child that this process forks: one byte per fork. posix_spawn runs no
    /// atfork handler.
    unsafe extern "C" fn note_fork_in_child() {
        let fd = FORK_PIPE.load(std::sync::atomic::Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write(2) is async-signal-safe; it writes one static byte to the pipe.
            unsafe { sys::write(fd, b"f".as_ptr().cast(), 1) };
        }
    }

    fn set_flag(
        fd: std::os::raw::c_int,
        get: std::os::raw::c_int,
        set: std::os::raw::c_int,
        flag: std::os::raw::c_int,
    ) {
        // SAFETY: fcntl reads and sets flags of a descriptor this test owns.
        unsafe {
            let flags = sys::fcntl(fd, get);
            assert!(flags >= 0, "fcntl");
            assert_eq!(sys::fcntl(fd, set, flags | flag), 0);
        }
    }

    /// The UDS worker's command, at the highest descriptor number this process can open: it
    /// forks, and on macOS it does so only once libnotify's initialization is complete
    /// (`prepare_fork_spawns`); the worker keeps only its standard descriptors. The scenario
    /// registers process-wide atfork handlers and raises the descriptor limit, so it runs alone
    /// in a fresh copy of this test binary.
    #[test]
    fn worker_command_forks_and_excludes_descriptors_up_to_the_table_end() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        if std::env::var_os(SCENARIO).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::tests::worker_command_forks_and_excludes_descriptors_up_to_the_table_end",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(SCENARIO, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "scenario failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        // The fork detector: a pipe whose write end every forked child writes to.
        let mut fds = [-1; 2];
        // SAFETY: pipe stores two new descriptors in `fds`.
        assert_eq!(unsafe { sys::pipe(fds.as_mut_ptr()) }, 0);
        for fd in fds {
            set_flag(fd, sys::F_GETFD, sys::F_SETFD, sys::FD_CLOEXEC);
        }
        set_flag(fds[0], sys::F_GETFL, sys::F_SETFL, sys::O_NONBLOCK);
        FORK_PIPE.store(fds[1], std::sync::atomic::Ordering::SeqCst);
        #[cfg(target_os = "macos")]
        {
            // SAFETY: dlsym looks up the table that libsystem_platform exports for its inline
            // `os_alloc_once`; slot 0 (libnotify's) starts with its once word.
            let table = unsafe { sys::dlsym(sys::RTLD_DEFAULT, c"_os_alloc_once_table".as_ptr()) };
            assert!(!table.is_null(), "_os_alloc_once_table");
            // SAFETY: as above.
            let once = unsafe { std::ptr::read_volatile(table.cast::<usize>()) };
            assert_eq!(once, 0, "libnotify was initialized before the spawn");
            NOTIFY_ONCE_WORD.store(table as usize, std::sync::atomic::Ordering::SeqCst);
        }
        // SAFETY: the handlers only load and store atomics, read one word and write to a pipe.
        assert_eq!(
            unsafe {
                sys::pthread_atfork(Some(record_notify_at_fork), None, Some(note_fork_in_child))
            },
            0
        );
        // SAFETY: pipe returned this descriptor, owned here from now on.
        let forks = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let drain = || {
            let mut total = 0;
            let mut buf = [0u8; 64];
            loop {
                // SAFETY: read into a stack buffer from the non-blocking read end.
                let n = unsafe { sys::read(forks.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    return total;
                }
                total += n as usize;
            }
        };

        // An inheritable descriptor at the highest number this process can open once its soft
        // limit is raised. macOS accepts a soft limit above kern.maxfilesperproc but caps the
        // table there; `getdtablesize` returns the capped size.
        let mut limit = sys::Rlimit { cur: 0, max: 0 };
        // SAFETY: getrlimit, setrlimit and getdtablesize read and write this process's limit.
        let table = unsafe {
            assert_eq!(sys::getrlimit(sys::RLIMIT_NOFILE, &mut limit), 0);
            let mut soft = limit.max.min(65_536);
            // Linux refuses any limit while the hard limit is above fs.nr_open.
            while soft > limit.cur
                && sys::setrlimit(
                    sys::RLIMIT_NOFILE,
                    &sys::Rlimit {
                        cur: soft,
                        max: limit.max,
                    },
                ) != 0
            {
                soft /= 2;
            }
            sys::getdtablesize()
        };
        let top = table - 1;
        assert!(top >= 1024, "the descriptor table stayed at {table}");
        let null = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD (0) returns a new descriptor without close-on-exec, owned below.
        let held = unsafe { sys::fcntl(null.as_raw_fd(), 0, top) };
        assert_eq!(held, top, "the highest descriptor number");
        // SAFETY: as above.
        let held = unsafe { OwnedFd::from_raw_fd(held) };

        let dir = tempfile::tempdir().unwrap();
        let script = |report: &Path| {
            let path = report.with_extension("sh");
            std::fs::write(
                &path,
                format!(
                    "for n in 1 {top}; do if [ -e /dev/fd/$n ]; then echo held; else echo clear; fi; done > '{}'\n",
                    report.display()
                ),
            )
            .unwrap();
            path
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // The control: a plain command spawns without forking and passes the descriptor on.
        let report = dir.path().join("plain");
        let plain = script(&report);
        drain();
        // tokio's `status` spawns when called, so call it inside the runtime.
        let status = runtime
            .block_on(async { Command::new("/bin/sh").arg(&plain).status().await })
            .unwrap();
        assert!(status.success());
        assert_eq!(drain(), 0, "a plain command does not fork");
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "held\nheld\n");

        // The worker's command forks, and the worker holds no descriptor above 2.
        let report = dir.path().join("worker");
        let worker = script(&report);
        drain();
        let status = runtime
            .block_on(async { worker_command(Path::new("/bin/sh"), &worker).status().await })
            .unwrap();
        assert!(status.success());
        assert!(drain() > 0, "the worker's command forks");
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "held\nclear\n");
        #[cfg(target_os = "macos")]
        {
            // Done (all bits set) or arm64's quiescing generation (low bits 01): the
            // initializer had returned when the worker's command forked.
            let once = ONCE_AT_FORK.load(std::sync::atomic::Ordering::SeqCst);
            assert!(
                once == usize::MAX || once & 3 == 1,
                "the worker's command forked during libnotify's initialization: {once:#x}"
            );
        }
        drop(held);
    }

    #[tokio::test]
    async fn spawn_hello_then_drop_kills_worker() {
        let Some(bin) = runtime_bin() else {
            return;
        };
        let store = Arc::new(Store::memory().unwrap());
        store.mark_shell_busy("demo", "proc-keep").unwrap();
        let (mut proc, _runner) =
            RuntimeProcess::spawn(&bin, None, Arc::new(|_| {}), store.clone())
                .await
                .expect("spawn runtime");
        let dir = proc.dir().to_path_buf();
        proc.wait_exit().await;
        drop(proc);
        let _lease = store
            .try_acquire_write("demo")
            .expect("process leases released after worker death");
        assert!(!dir.exists() || std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0) == 0);
    }
}
