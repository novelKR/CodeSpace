//! Child-side exclusion of unrelated descriptors (#79).
//!
//! A spawn passes on every descriptor that is not close-on-exec when it forks. macOS cannot
//! create a pipe, a socket pair or a pseudo-terminal close-on-exec atomically, so a child
//! spawned while another thread is between creating a descriptor and marking it inherits that
//! descriptor: another execution's pipe ends, or its PTY master and slave. The holder can then
//! delay that execution's completion, keep its terminal allocated, or read and write it.
//!
//! [`exclude_unrelated`] installs a `pre_exec` step that marks every descriptor above 2
//! close-on-exec in the child, before exec:
//! - Linux: `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`. Where the kernel refuses it (before 5.11,
//!   or under a seccomp policy), the child reads its descriptor-table size (`FDSize` in
//!   `/proc/self/status`) and marks every descriptor below it.
//! - macOS: the child asks the kernel for its descriptor-table size (`proc_pidinfo` with
//!   `PROC_PIDLISTFDS` and no buffer returns the table's slot count plus 20 entries) and marks
//!   every descriptor below it.
//!
//! Every open descriptor indexes that table, so the walk does not depend on `RLIMIT_NOFILE`,
//! which may have been lowered below a descriptor that is still open. The standard descriptors
//! are kept, and descriptors already close-on-exec, such as the standard library's exec-error
//! pipe, are left alone, so a failed exec is still reported as a spawn error. If the step cannot
//! establish this (the table size is unavailable, or an open descriptor cannot be inspected or
//! marked) it returns the error and the spawn fails, instead of starting a child that may hold
//! an unrelated descriptor. Other platforms always fail this way.
//!
//! [`exclude_unrelated_except`] does the same but keeps two named descriptors, which it makes
//! inheritable in the child only: the private descriptors of a DevGuard-managed launch, which
//! reach the launch helper and no other child (CSRG-U4).
//!
//! The step runs after fork and before exec. It does not allocate, take locks or panic. It
//! calls only `fcntl`, plus `getpid` and `proc_pidinfo` on macOS or `syscall` (`close_range`,
//! `openat`, `read`, `close`) on Linux, and it writes only its own stack and `errno`.
//!
//! Installing a `pre_exec` step makes std, and Tokio through it, create these children with
//! fork and exec instead of its `posix_spawn` fast path. Process ownership, the std and Tokio
//! child handles, reaping, output pumps, timeouts, the PTY path and lifecycle are unchanged.
//! Because the spawn then forks, installing the step first calls
//! [`crate::prepare_fork_spawns`], which keeps a macOS child from copying libnotify's
//! initialization in progress.

use std::io;

/// Mark every descriptor above 2 close-on-exec in the child, before exec. The spawn then forks,
/// so this first calls [`crate::prepare_fork_spawns`].
pub fn exclude_unrelated(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    crate::prepare_fork_spawns();
    // SAFETY: `mark_unrelated` runs in the forked child and is restricted as the module
    // documentation describes.
    unsafe { command.pre_exec(|| mark_unrelated(Forced::NONE)) }
}

/// [`exclude_unrelated`] for a standard-library command.
pub(crate) fn exclude_unrelated_std(
    command: &mut std::process::Command,
) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    crate::prepare_fork_spawns();
    // SAFETY: as for `exclude_unrelated`.
    unsafe { command.pre_exec(|| mark_unrelated(Forced::NONE)) }
}

/// [`exclude_unrelated`], except that the child keeps `keep` (CSRG-U4): it marks every other
/// descriptor above 2 close-on-exec and clears close-on-exec on each of `keep`, which therefore
/// may stay close-on-exec in this process. A managed launch passes DevGuard's permit carrier and
/// transcript writer this way to its launch helper and to nothing else. If one of `keep` is not
/// open in the child the step fails, and with it the spawn.
#[cfg(any(feature = "devguard", test))]
pub(crate) fn exclude_unrelated_except(
    command: &mut tokio::process::Command,
    keep: [libc::c_int; 2],
) -> &mut tokio::process::Command {
    crate::prepare_fork_spawns();
    // SAFETY: the step runs in the forked child and is restricted as the module documentation
    // describes; `keep` is copied into the closure, so it allocates nothing there.
    unsafe {
        command.pre_exec(move || {
            mark_unrelated_except(Forced::NONE, &keep)?;
            for fd in keep {
                keep_open(fd)?;
            }
            Ok(())
        })
    }
}

/// Failures the tests force. Production code passes `Forced::NONE`.
#[derive(Clone, Copy)]
struct Forced {
    /// Skip `close_range` (Linux), so the table walk runs.
    close_range: bool,
    /// Report the descriptor-table size as unavailable.
    table: bool,
}

impl Forced {
    const NONE: Self = Self {
        close_range: false,
        table: false,
    };
}

/// Runs in the child: mark every descriptor above 2 close-on-exec, or fail.
fn mark_unrelated(forced: Forced) -> io::Result<()> {
    mark_unrelated_except(forced, &[])
}

/// [`mark_unrelated`], leaving `keep` as it is. (`close_range` marks them too; the caller then
/// clears them.)
fn mark_unrelated_except(forced: Forced, keep: &[libc::c_int]) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        if !forced.close_range && close_range_cloexec() {
            return Ok(());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = forced.close_range;
    let end = if forced.table {
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    } else {
        descriptor_table_size()
    }?;
    for fd in 3..end {
        if !keep.contains(&fd) {
            mark_close_on_exec(fd)?;
        }
    }
    Ok(())
}

/// Runs in the child: clear close-on-exec on `fd`, which must be open.
#[cfg(any(feature = "devguard", test))]
fn keep_open(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl reads and sets only this process's descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// What `F_GETFD` reported for a descriptor number.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    NotOpen,
    CloseOnExec,
    Inheritable(libc::c_int),
    Failed(i32),
}

/// Only `EBADF` means "not open"; any other failure is an error.
fn classify(flags: libc::c_int, errno: i32) -> Probe {
    if flags < 0 {
        if errno == libc::EBADF {
            Probe::NotOpen
        } else {
            Probe::Failed(errno)
        }
    } else if flags & libc::FD_CLOEXEC != 0 {
        Probe::CloseOnExec
    } else {
        Probe::Inheritable(flags)
    }
}

/// The current `errno`, read without allocating.
fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Mark `fd` close-on-exec unless it is not open or already marked.
fn mark_close_on_exec(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl reads and sets only this process's descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    let probe = classify(flags, if flags < 0 { errno() } else { 0 });
    match probe {
        Probe::NotOpen | Probe::CloseOnExec => Ok(()),
        Probe::Failed(code) => Err(io::Error::from_raw_os_error(code)),
        Probe::Inheritable(flags) => {
            // SAFETY: as above.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

/// An error for a call that failed, or `fallback` if `errno` was not set.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn os_error_or(fallback: i32) -> io::Error {
    match errno() {
        0 => io::Error::from_raw_os_error(fallback),
        code => io::Error::from_raw_os_error(code),
    }
}

/// Linux: one call marks every descriptor from 3 up.
#[cfg(target_os = "linux")]
fn close_range_cloexec() -> bool {
    let first: libc::c_uint = 3;
    // SAFETY: close_range with CLOSE_RANGE_CLOEXEC only sets this process's descriptor flags.
    unsafe {
        libc::syscall(
            libc::SYS_close_range,
            first,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        ) == 0
    }
}

/// macOS: the descriptor-table size. Without a buffer, `proc_pidinfo(PROC_PIDLISTFDS)` returns
/// `(fd_nfiles + 20) * sizeof(struct proc_fdinfo)`, and every open descriptor is below
/// `fd_nfiles`, the table's slot count.
#[cfg(target_os = "macos")]
fn descriptor_table_size() -> io::Result<libc::c_int> {
    // SAFETY: getpid and a size-only proc_pidinfo read this process's state into no buffer.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    let entry = std::mem::size_of::<libc::proc_fdinfo>() as libc::c_int;
    if bytes <= 0 {
        return Err(os_error_or(libc::EIO));
    }
    Ok(bytes / entry)
}

/// Linux: the descriptor-table size, `FDSize` in `/proc/self/status`. Every open descriptor is
/// below it.
#[cfg(target_os = "linux")]
fn descriptor_table_size() -> io::Result<libc::c_int> {
    let mut status = [0u8; 4096];
    // SAFETY: openat of a constant path; the descriptor is closed below.
    let file = unsafe {
        libc::syscall(
            libc::SYS_openat,
            libc::c_long::from(libc::AT_FDCWD),
            c"/proc/self/status".as_ptr(),
            libc::c_long::from(libc::O_RDONLY | libc::O_CLOEXEC),
        )
    };
    if file < 0 {
        return Err(os_error_or(libc::ENOENT));
    }
    let mut filled = 0;
    let read = loop {
        let rest = status.get_mut(filled..).unwrap_or_default();
        if rest.is_empty() {
            break Ok(());
        }
        // SAFETY: read writes at most `rest.len()` bytes into the stack buffer.
        let count = unsafe { libc::syscall(libc::SYS_read, file, rest.as_mut_ptr(), rest.len()) };
        if count == 0 {
            break Ok(());
        }
        if count < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            break Err(os_error_or(libc::EIO));
        }
        filled += count as usize;
    };
    // SAFETY: closes the descriptor opened above.
    unsafe { libc::syscall(libc::SYS_close, file) };
    read?;
    parse_fd_size(status.get(..filled).unwrap_or_default())
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ENODATA))
}

/// The `FDSize` value of a `/proc/<pid>/status` text.
#[cfg(any(target_os = "linux", test))]
fn parse_fd_size(status: &[u8]) -> Option<libc::c_int> {
    const KEY: &[u8] = b"\nFDSize:";
    let start = status.windows(KEY.len()).position(|window| window == KEY)? + KEY.len();
    let mut value: libc::c_int = 0;
    let mut digits = 0;
    for &byte in status.get(start..)? {
        match byte {
            b' ' | b'\t' if digits == 0 => {}
            b'0'..=b'9' => {
                value = value
                    .checked_mul(10)?
                    .checked_add(libc::c_int::from(byte - b'0'))?;
                digits += 1;
            }
            _ => break,
        }
    }
    (digits > 0).then_some(value)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn descriptor_table_size() -> io::Result<libc::c_int> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    /// A descriptor that is not close-on-exec at or above `at`: what another thread's creation
    /// window, or a careless library, leaves behind.
    fn inheritable_at(at: libc::c_int) -> OwnedFd {
        // SAFETY: open and fcntl create descriptors that this test owns.
        let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        assert!(null >= 0, "open /dev/null");
        // SAFETY: open returned a new descriptor.
        let null = unsafe { OwnedFd::from_raw_fd(null) };
        // SAFETY: F_DUPFD returns a new descriptor without close-on-exec.
        let high = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, at) };
        assert!(high >= at, "F_DUPFD {at}: {}", io::Error::last_os_error());
        // SAFETY: as above.
        unsafe { OwnedFd::from_raw_fd(high) }
    }

    pub(crate) fn inheritable_descriptor() -> OwnedFd {
        inheritable_at(200)
    }

    /// A shell command that reports whether it holds its stdout (a control) and then `fd`.
    pub(crate) fn report_script(fd: libc::c_int) -> String {
        format!(
            "for n in 1 {fd}; do if [ -e /dev/fd/$n ]; then echo held; else echo clear; fi; done"
        )
    }

    fn child_report(fd: libc::c_int, prepare: impl FnOnce(&mut Command)) -> String {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(report_script(fd))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        prepare(&mut command);
        let output = command.output().expect("spawn /bin/sh");
        String::from_utf8(output.stdout).expect("utf-8 report")
    }

    fn forced(command: &mut Command, forced: Forced) {
        crate::prepare_fork_spawns();
        // SAFETY: as for `exclude_unrelated`.
        unsafe {
            command.pre_exec(move || mark_unrelated(forced));
        }
    }

    const WALK: Forced = Forced {
        close_range: true,
        table: false,
    };

    #[test]
    fn unguarded_child_inherits_the_descriptor() {
        let held = inheritable_descriptor();
        assert_eq!(child_report(held.as_raw_fd(), |_| {}), "held\nheld\n");
    }

    #[test]
    fn guarded_child_keeps_only_its_standard_descriptors() {
        let held = inheritable_descriptor();
        let report = child_report(held.as_raw_fd(), |command| {
            exclude_unrelated_std(command);
        });
        assert_eq!(report, "held\nclear\n");
    }

    #[test]
    fn table_walk_excludes_the_descriptor() {
        let held = inheritable_descriptor();
        let report = child_report(held.as_raw_fd(), |command| forced(command, WALK));
        assert_eq!(report, "held\nclear\n");
    }

    /// A close-on-exec descriptor at or above `at`, as DevGuard creates a launch's carriers.
    fn close_on_exec_at(at: libc::c_int) -> OwnedFd {
        let held = inheritable_at(at);
        // SAFETY: sets the flags of a descriptor this test owns.
        assert_eq!(
            unsafe { libc::fcntl(held.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
        held
    }

    /// A shell command that reports, for each of `fds`, whether it holds it.
    fn report_each(fds: &[libc::c_int]) -> String {
        let numbers: Vec<String> = fds.iter().map(|fd| fd.to_string()).collect();
        format!(
            "for n in {}; do if [ -e /dev/fd/$n ]; then echo held; else echo clear; fi; done",
            numbers.join(" ")
        )
    }

    async fn tokio_report(
        script: String,
        prepare: impl FnOnce(&mut tokio::process::Command),
    ) -> String {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        prepare(&mut command);
        let output = command.output().await.expect("spawn /bin/sh");
        String::from_utf8(output.stdout).expect("utf-8 report")
    }

    #[tokio::test]
    async fn only_the_kept_descriptors_reach_the_child() {
        // Close-on-exec here, as a launch's carriers are; and one inheritable, as another
        // thread's creation window would leave.
        let permit = close_on_exec_at(210);
        let report = close_on_exec_at(220);
        let unrelated = inheritable_descriptor();
        let fds = [
            permit.as_raw_fd(),
            report.as_raw_fd(),
            unrelated.as_raw_fd(),
        ];
        // The control: a plain child holds only the inheritable one.
        assert_eq!(
            tokio_report(report_each(&fds), |_| {}).await,
            "clear\nclear\nheld\n"
        );
        let kept = tokio_report(report_each(&fds), |command| {
            exclude_unrelated_except(command, [fds[0], fds[1]]);
        })
        .await;
        assert_eq!(kept, "held\nheld\nclear\n");
        // This process's copies stay close-on-exec.
        for fd in [fds[0], fds[1]] {
            // SAFETY: reads the flags of a descriptor this test owns.
            assert_ne!(
                unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
    }

    #[tokio::test]
    async fn a_kept_descriptor_that_is_not_open_fails_the_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let gone = close_on_exec_at(230);
        let number = gone.as_raw_fd();
        drop(gone);
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!(": > '{}'", ran.display()))
            .stdin(Stdio::null());
        exclude_unrelated_except(&mut command, [number, number]);
        let error = command.spawn().expect_err("the spawn must fail");
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
        assert!(!ran.exists(), "the child must not run");
    }

    #[test]
    fn the_table_walk_keeps_only_the_kept_descriptors() {
        let kept = close_on_exec_at(240);
        let unrelated = inheritable_descriptor();
        let fds = [kept.as_raw_fd(), unrelated.as_raw_fd()];
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(report_each(&fds))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::prepare_fork_spawns();
        let keep = [fds[0], fds[0]];
        // SAFETY: as for `exclude_unrelated_except`.
        unsafe {
            command.pre_exec(move || {
                mark_unrelated_except(WALK, &keep)?;
                keep_open(keep[0])
            });
        }
        let output = command.output().unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "held\nclear\n");
    }

    #[test]
    fn unavailable_table_size_fails_the_spawn() {
        let held = inheritable_descriptor();
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!(": > '{}'", ran.display()))
            .stdin(Stdio::null());
        forced(
            &mut command,
            Forced {
                close_range: true,
                table: true,
            },
        );
        let error = command.spawn().expect_err("the spawn must fail");
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSUP));
        assert!(!ran.exists(), "the child must not run");
        drop(held);
    }

    #[test]
    fn guarded_spawn_still_reports_a_failed_exec() {
        let mut command = Command::new("/no/such/codespace-exec");
        exclude_unrelated_std(&mut command);
        let error = command.spawn().expect_err("exec must fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn only_ebadf_means_not_open() {
        assert_eq!(classify(-1, libc::EBADF), Probe::NotOpen);
        assert_eq!(classify(-1, libc::EINVAL), Probe::Failed(libc::EINVAL));
        assert_eq!(classify(-1, libc::EIO), Probe::Failed(libc::EIO));
        assert_eq!(classify(libc::FD_CLOEXEC, 0), Probe::CloseOnExec);
        assert_eq!(classify(0, 0), Probe::Inheritable(0));
    }

    #[test]
    fn marking_sets_close_on_exec_and_skips_numbers_not_open() {
        let held = inheritable_descriptor();
        let fd = held.as_raw_fd();
        // SAFETY: reads this test's own descriptor flags.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        mark_close_on_exec(fd).unwrap();
        // SAFETY: as above.
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        // A number no test opens.
        mark_close_on_exec(1 << 30).unwrap();
    }

    #[test]
    fn table_size_covers_an_open_descriptor() {
        let held = inheritable_descriptor();
        assert!(descriptor_table_size().unwrap() > held.as_raw_fd());
    }

    #[test]
    fn fd_size_is_parsed_from_proc_status() {
        assert_eq!(
            parse_fd_size(b"Name:\tx\nPid:\t1\nFDSize:\t256\nGroups:\t\n"),
            Some(256)
        );
        assert_eq!(parse_fd_size(b"Name:\tx\nFDSize:\n"), None);
        assert_eq!(parse_fd_size(b"Name:\tx\nFDSize:\t99999999999\n"), None);
        assert_eq!(parse_fd_size(b"FDSize:\t64\n"), None);
    }

    /// Set by the parent test so that its re-run copy performs the scenario.
    const SCENARIO: &str = "CODESPACE_DESCRIPTOR_LIMIT_SCENARIO";

    #[test]
    fn descriptor_above_a_lowered_limit_is_excluded() {
        if std::env::var_os(SCENARIO).is_some() {
            lowered_limit_scenario();
            return;
        }
        // The scenario changes this process's descriptor limit, so it runs in a fresh copy of
        // this test binary.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "descriptors::tests::descriptor_above_a_lowered_limit_is_excluded",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(SCENARIO, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "scenario failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn set_soft_limit(soft: libc::rlim_t) {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit and setrlimit read and write one rlimit of this process.
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
            limit.rlim_cur = soft;
            assert_eq!(
                libc::setrlimit(libc::RLIMIT_NOFILE, &limit),
                0,
                "setrlimit {soft}: {}",
                io::Error::last_os_error()
            );
        }
    }

    /// A descriptor stays open above a soft `RLIMIT_NOFILE` lowered after it was opened. The
    /// table walk must still exclude it.
    fn lowered_limit_scenario() {
        const HIGH: libc::c_int = 1500;
        const LOWERED: libc::rlim_t = 1024;
        set_soft_limit(2048);
        let held = inheritable_at(HIGH);
        set_soft_limit(LOWERED);
        assert!(LOWERED <= held.as_raw_fd() as libc::rlim_t);
        assert_eq!(child_report(held.as_raw_fd(), |_| {}), "held\nheld\n");
        let walked = child_report(held.as_raw_fd(), |command| forced(command, WALK));
        assert_eq!(walked, "held\nclear\n");
        let guarded = child_report(held.as_raw_fd(), |command| {
            exclude_unrelated_std(command);
        });
        assert_eq!(guarded, "held\nclear\n");
    }

    /// Raise the soft `RLIMIT_NOFILE` as far as the system allows, up to `wanted`, and return the
    /// size of this process's descriptor table. macOS accepts a soft limit above
    /// kern.maxfilesperproc but caps the table there; `getdtablesize` returns the capped size.
    fn raise_descriptor_table(wanted: libc::rlim_t) -> libc::c_int {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit, setrlimit and getdtablesize read and write this process's limit.
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
            let mut soft = wanted.min(limit.rlim_max);
            // Linux refuses any limit while the hard limit is above fs.nr_open.
            while soft > limit.rlim_cur {
                let raised = libc::rlimit {
                    rlim_cur: soft,
                    rlim_max: limit.rlim_max,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                    break;
                }
                soft /= 2;
            }
            libc::getdtablesize()
        }
    }

    /// The pipe and patch-helper spawns exclude an inheritable descriptor at the highest number
    /// this process can open. The scenario raises the descriptor limit, so it runs in a fresh
    /// copy of this test binary; the worker spawn is checked in the gateway crate.
    #[test]
    fn production_children_exclude_a_descriptor_at_the_table_end() {
        use crate::fork_handlers::tests::{in_isolated_copy, run_isolated};
        if !in_isolated_copy() {
            run_isolated(
                "descriptors::tests::production_children_exclude_a_descriptor_at_the_table_end",
            );
            return;
        }
        let table = raise_descriptor_table(65_536);
        let top = table - 1;
        assert!(top >= 1024, "the descriptor table stayed at {table}");
        let held = inheritable_at(top);
        assert_eq!(held.as_raw_fd(), top, "the highest descriptor number");
        // The control: a plain child inherits it.
        assert_eq!(child_report(top, |_| {}), "held\nheld\n");

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            use crate::Runner;
            let dir = tempfile::tempdir().unwrap();
            let ws = codespace_policy::Workspace::new(
                codespace_domain::WorkspaceId("demo".into()),
                dir.path().to_path_buf(),
                codespace_domain::Profile::WorkspaceWrite,
            );
            let runner = crate::InProcessRunner::new(std::sync::Arc::new(|_| {}));
            let process_id = codespace_domain::ProcessId("proc-table-end".into());
            let request = crate::RunnerExecRequest::for_host(
                vec!["/bin/sh".into(), "-c".into(), report_script(top)],
                process_id.clone(),
                codespace_domain::Profile::WorkspaceWrite,
            );
            runner.exec(&ws, request).await.unwrap();
            let mut output = String::new();
            for _ in 0..500 {
                let read = runner
                    .read_process(crate::RunnerReadProcess {
                        process_id: process_id.clone(),
                        cursor: 0,
                    })
                    .await
                    .unwrap();
                output = read.chunk;
                if read.eof {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(output, "held\nclear\n", "pipe child");

            // The patch helper's command, with a shell standing in for the helper: it reads the
            // report script from its piped stdin.
            let mut helper = crate::patch_helper::helper_command(std::path::Path::new("/bin/sh"))
                .spawn()
                .unwrap();
            let mut stdin = helper.stdin.take().unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stdin, report_script(top).as_bytes())
                .await
                .unwrap();
            drop(stdin);
            let output = helper.wait_with_output().await.unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "held\nclear\n",
                "patch helper child"
            );
        });
        drop(held);
    }
}
