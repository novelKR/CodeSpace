//! Child-side exclusion of unrelated descriptors (#79).
//!
//! macOS cannot create a pipe, a socket pair or a pseudo-terminal close-on-exec atomically, and a
//! spawn passes on every descriptor that is not close-on-exec at that instant. A child spawned
//! while another thread is between creating a descriptor and marking it therefore inherits that
//! descriptor: another execution's pipe ends, or its PTY master and slave. The holder can then
//! delay that execution's completion, keep its terminal allocated, or read and write it.
//!
//! [`exclude_unrelated`] marks every descriptor above 2 close-on-exec in the child, before exec.
//! The child keeps the standard descriptors its spawn set up. Descriptors that are already
//! close-on-exec, such as the standard library's exec-error pipe, are left alone, so a failed exec
//! is still reported as a spawn error. Commands already get a cleared environment; with this they
//! also get no descriptor beyond their standard three.

use std::io;

/// Room in the listing for descriptors opened between [`Table::new`] and the fork.
#[cfg(target_os = "macos")]
const SPARE_ENTRIES: usize = 256;

/// The descriptor limit assumed when none is reported (Linux's default `nr_open`).
const FALLBACK_LIMIT: libc::rlim_t = 1 << 20;

/// Mark every descriptor above 2 close-on-exec in the child, before exec.
pub fn exclude_unrelated(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    let mut table = Table::new();
    // SAFETY: the closure runs in the forked child. It only makes system calls on memory
    // allocated before the fork, and it does not allocate, lock or panic (`Table::mark`).
    unsafe { command.pre_exec(move || table.mark()) }
}

/// [`exclude_unrelated`] for a standard-library command.
pub(crate) fn exclude_unrelated_std(
    command: &mut std::process::Command,
) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut table = Table::new();
    // SAFETY: as for `exclude_unrelated`.
    unsafe { command.pre_exec(move || table.mark()) }
}

/// What the child needs, allocated in the parent before the fork.
struct Table {
    /// One past the highest descriptor number this process can hold, for the fallback walk.
    limit: libc::c_int,
    /// Room for the child's listing of its own descriptors.
    #[cfg(target_os = "macos")]
    listing: Vec<libc::proc_fdinfo>,
}

#[cfg(target_os = "macos")]
const ENTRY: usize = std::mem::size_of::<libc::proc_fdinfo>();

impl Table {
    #[cfg(target_os = "macos")]
    fn new() -> Self {
        // SAFETY: without a buffer, proc_pidinfo only reports the size of this process's
        // descriptor table, which bounds the number of open descriptors.
        let table_bytes = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                std::ptr::null_mut(),
                0,
            )
        };
        let entries = usize::try_from(table_bytes).unwrap_or(0) / ENTRY + SPARE_ENTRIES;
        let empty = libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        Self {
            limit: descriptor_limit(),
            listing: vec![empty; entries],
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn new() -> Self {
        Self {
            limit: descriptor_limit(),
        }
    }

    /// Runs in the child: list the descriptors and mark them, or walk every number if the
    /// listing is unavailable.
    #[cfg(target_os = "macos")]
    fn mark(&mut self) -> io::Result<()> {
        let capacity =
            libc::c_int::try_from(self.listing.len() * ENTRY).unwrap_or(libc::c_int::MAX);
        // SAFETY: proc_pidinfo writes at most `capacity` bytes into the listing, which was
        // allocated before the fork.
        let bytes = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                self.listing.as_mut_ptr().cast(),
                capacity,
            )
        };
        if bytes > 0 && bytes < capacity {
            let listed = bytes as usize / ENTRY;
            for entry in self.listing.iter().take(listed) {
                mark_close_on_exec(entry.proc_fd)?;
            }
            return Ok(());
        }
        // The listing failed or may be truncated.
        walk(self.limit)
    }

    /// Runs in the child: one system call marks every descriptor above 2.
    #[cfg(target_os = "linux")]
    fn mark(&mut self) -> io::Result<()> {
        let first: libc::c_uint = 3;
        // SAFETY: close_range with CLOSE_RANGE_CLOEXEC only sets this process's descriptor flags.
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                first,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if marked == 0 {
            return Ok(());
        }
        // Kernels before 5.11 do not know the flag.
        walk(self.limit)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn mark(&mut self) -> io::Result<()> {
        walk(self.limit)
    }
}

/// Mark every descriptor number from 3 up to `limit`. Complete but slow; only a fallback.
fn walk(limit: libc::c_int) -> io::Result<()> {
    for fd in 3..limit {
        mark_close_on_exec(fd)?;
    }
    Ok(())
}

/// Mark `fd` close-on-exec, unless it is a standard descriptor, is not open, or is already marked.
fn mark_close_on_exec(fd: libc::c_int) -> io::Result<()> {
    if fd <= 2 {
        return Ok(());
    }
    // SAFETY: fcntl reads and sets only this process's descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || flags & libc::FD_CLOEXEC != 0 {
        return Ok(());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One past the highest descriptor number this process can hold.
fn descriptor_limit() -> libc::c_int {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one rlimit.
    let soft = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_cur
    } else {
        libc::RLIM_INFINITY
    };
    libc::c_int::try_from(soft.min(kernel_limit())).unwrap_or(libc::c_int::MAX)
}

#[cfg(target_os = "macos")]
fn kernel_limit() -> libc::rlim_t {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: sysctlbyname writes at most `size` bytes into `value`.
    let read = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    match libc::rlim_t::try_from(value) {
        Ok(limit) if read == 0 && limit > 0 => limit,
        _ => FALLBACK_LIMIT,
    }
}

#[cfg(not(target_os = "macos"))]
fn kernel_limit() -> libc::rlim_t {
    FALLBACK_LIMIT
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    /// A descriptor that is not close-on-exec, at a distinctive number: what another thread's
    /// creation window, or a careless library, leaves behind.
    pub(crate) fn inheritable_descriptor() -> OwnedFd {
        // SAFETY: open and fcntl create descriptors that this test owns.
        let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        assert!(null >= 0, "open /dev/null");
        // SAFETY: open returned a new descriptor.
        let null = unsafe { OwnedFd::from_raw_fd(null) };
        // SAFETY: F_DUPFD returns a new descriptor without close-on-exec.
        let high = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, 400) };
        assert!(high >= 400, "F_DUPFD");
        // SAFETY: as above.
        unsafe { OwnedFd::from_raw_fd(high) }
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
    fn fallback_walk_alone_excludes_the_descriptor() {
        let held = inheritable_descriptor();
        let limit = descriptor_limit();
        let report = child_report(held.as_raw_fd(), |command| {
            // SAFETY: the closure only calls fcntl.
            unsafe {
                command.pre_exec(move || walk(limit));
            }
        });
        assert_eq!(report, "held\nclear\n");
    }

    #[test]
    fn guarded_spawn_still_reports_a_failed_exec() {
        let mut command = Command::new("/no/such/codespace-exec");
        exclude_unrelated_std(&mut command);
        let error = command.spawn().expect_err("exec must fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn limit_covers_the_descriptors_in_use() {
        let held = inheritable_descriptor();
        assert!(descriptor_limit() > held.as_raw_fd());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn listing_has_room_for_the_descriptor_table() {
        let table = Table::new();
        assert!(table.listing.len() > SPARE_ENTRIES);
    }
}
