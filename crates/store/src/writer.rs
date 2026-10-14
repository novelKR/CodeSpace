//! The single writer of an operations database (P1R-C02). A gateway connected to an independent
//! Runner holds an exclusive lock on a file beside its database for as long as it runs, so that
//! two gateways never record into the same database. The lock is advisory: a gateway in another
//! mode, or a CodeSpace from before process records, does not take it.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a waiting gateway tries the lock again.
const RETRY: Duration = Duration::from_millis(100);

/// An exclusive lock on `<database>.gateway.lock`, held until it is dropped or its process
/// ends, however it ends.
#[derive(Debug)]
pub struct WriterLock {
    _file: File,
    path: PathBuf,
}

impl WriterLock {
    /// The lock file beside `database`.
    pub fn path_for(database: &Path) -> PathBuf {
        let mut name = database
            .file_name()
            .map(OsStr::to_os_string)
            .unwrap_or_default();
        name.push(".gateway.lock");
        database.with_file_name(name)
    }

    /// Take the lock beside `database`. The gateway that holds it may still be ending, as when
    /// a client starts a new stdio gateway while the old one detaches, so this waits up to
    /// `wait` for it, and calls `waiting` with the holder's process ID once it has to wait.
    /// Then it refuses.
    pub fn acquire(
        database: &Path,
        wait: Duration,
        waiting: impl FnOnce(Option<u32>),
    ) -> Result<Self, String> {
        let path = Self::path_for(database);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|err| format!("cannot open the lock file {}: {err}", path.display()))?;
        let deadline = Instant::now() + wait;
        let mut waiting = Some(waiting);
        loop {
            if try_lock(&file).map_err(|err| format!("cannot lock {}: {err}", path.display()))? {
                break;
            }
            let holder = holder(&mut file);
            if let Some(waiting) = waiting.take() {
                waiting(holder);
            }
            let now = Instant::now();
            if now >= deadline {
                let holder = holder
                    .map(|pid| format!("process {pid}"))
                    .unwrap_or_else(|| "another process".into());
                return Err(format!(
                    "the operations database {} has another writer: {holder} holds {} and \
                     did not let go within {wait:?}. One gateway records into a database at a \
                     time; end that gateway first",
                    database.display(),
                    path.display(),
                ));
            }
            std::thread::sleep(RETRY.min(deadline - now));
        }
        // Who holds it, for a gateway that has to wait.
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(format!("{}\n", std::process::id()).as_bytes()))
            .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
        Ok(Self { _file: file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Take the lock if no other open file holds it.
fn try_lock(file: &File) -> std::io::Result<bool> {
    loop {
        // SAFETY: flock locks the open file description behind a descriptor this file owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => continue,
            _ => return Err(err),
        }
    }
}

/// The process ID the holder wrote, when it has.
fn holder(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    fn not_waiting(holder: Option<u32>) {
        panic!("waited for {holder:?}");
    }

    #[test]
    fn the_lock_is_a_private_file_beside_the_database_naming_its_holder() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state").join("ops.sqlite");
        let lock = WriterLock::acquire(&database, Duration::ZERO, not_waiting).unwrap();
        assert_eq!(
            lock.path(),
            dir.path().join("state").join("ops.sqlite.gateway.lock")
        );
        let mode = std::fs::metadata(lock.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_to_string(lock.path()).unwrap(),
            format!("{}\n", std::process::id())
        );
        // Only the lock: the database is not created here.
        assert!(!database.exists());
    }

    #[test]
    fn a_second_writer_waits_up_to_its_bound_and_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let _first = WriterLock::acquire(&database, Duration::ZERO, not_waiting).unwrap();
        let started = Instant::now();
        let mut waited_for = None;
        let err = WriterLock::acquire(&database, Duration::from_millis(400), |holder| {
            waited_for = Some(holder)
        })
        .unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(400), "{err}");
        assert_eq!(waited_for, Some(Some(std::process::id())));
        assert!(err.contains("has another writer"), "{err}");
        assert!(
            err.contains(&format!("process {}", std::process::id())),
            "{err}"
        );
        assert!(err.contains("ops.sqlite.gateway.lock"), "{err}");
    }

    #[test]
    fn a_writer_that_lets_go_within_the_bound_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let first = WriterLock::acquire(&database, Duration::ZERO, not_waiting).unwrap();
        let ending = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(first);
        });
        let started = Instant::now();
        let mut waited = false;
        let second =
            WriterLock::acquire(&database, Duration::from_secs(10), |_| waited = true).unwrap();
        ending.join().unwrap();
        assert!(waited);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        assert_eq!(
            std::fs::read_to_string(second.path()).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    /// A gateway that is killed while it holds the lock lets go of it with its process: the
    /// next one never has to remove a stale lock file.
    #[test]
    fn a_writer_killed_holding_the_lock_lets_go_of_it() {
        const HOLDER: &str = "CODESPACE_STORE_LOCK_HOLDER";
        if let Some(database) = std::env::var_os(HOLDER) {
            // The holder, in a copy of this test binary: it holds the lock until it is killed.
            let _lock =
                WriterLock::acquire(Path::new(&database), Duration::from_secs(30), |_| {}).unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let mut holder = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "writer::tests::a_writer_killed_holding_the_lock_lets_go_of_it",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(HOLDER, &database)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let named = format!("process {}", holder.id());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            // A lock this test takes meanwhile is let go at once, before the next try.
            let held = matches!(
                WriterLock::acquire(&database, Duration::ZERO, |_| {}),
                Err(err) if err.contains(&named)
            );
            if held {
                break;
            }
            assert!(Instant::now() < deadline, "the holder never took the lock");
            std::thread::sleep(Duration::from_millis(50));
        }
        holder.kill().unwrap();
        holder.wait().unwrap();
        let lock = WriterLock::acquire(&database, Duration::from_secs(5), |_| {}).unwrap();
        assert_eq!(
            std::fs::read_to_string(lock.path()).unwrap(),
            format!("{}\n", std::process::id())
        );
    }
}
