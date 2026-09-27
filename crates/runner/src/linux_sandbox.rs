//! Client for the Linux sandbox helper process. Codex translation stays
//! behind `codespace-linux-sandbox`; this module speaks prepare/run JSON
//! and locates the binary. `sandbox_exec_env` stays here (CodeSpace env
//! semantics, not Codex).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use codespace_domain::{ErrorBody, ErrorCode};
use codespace_linux_sandbox_protocol::{
    SandboxNetwork, SandboxPrepareRequest, SandboxPrepareResponse, SANDBOX_HELPER_PROTOCOL,
};

/// Env override for the helper, matching `CODESPACE_PATCH_BIN` /
/// `CODESPACE_RUNTIME_BIN`.
pub const HELPER_BIN_ENV: &str = "CODESPACE_LINUX_SANDBOX_BIN";
pub const HELPER_BIN_NAME: &str = "codespace-linux-sandbox";

/// PATH inside the sandbox. Host `HOME` / `~/.cargo/bin` are not mounted
/// for toolchain discovery.
pub const SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin";

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(5);
const PREPARE_IO_LIMIT: usize = 1024 * 1024;

/// Helper program + `run --plan` argv. Pipe and PTY spawn this, not the
/// user argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLaunch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub plan_path: PathBuf,
}

/// Locate the helper: `CODESPACE_LINUX_SANDBOX_BIN`, else a binary named
/// [`HELPER_BIN_NAME`] next to the current executable.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(HELPER_BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(HELPER_BIN_NAME);
    candidate.is_file().then_some(candidate)
}

/// Cached `helper probe`. Non-Linux is always `false`. A failed probe
/// does not wrap later spawns. A successful probe never falls back to
/// unsandboxed user argv.
pub fn probe() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return false;
        }
        let Some(helper) = helper_path() else {
            return false;
        };
        probe_helper(&helper)
    })
}

fn probe_helper(helper: &Path) -> bool {
    let mut child = Command::new(helper);
    child
        .arg("probe")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(child) = child.spawn() else {
        return false;
    };
    matches!(wait_with_timeout(child, PROBE_TIMEOUT), Some(status) if status.success())
}

/// Env applied after `env_clear` for a sandboxed spawn. `HOME` stays the
/// workspace root (same as unsandboxed runner defaults).
pub fn sandbox_exec_env(home: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), SANDBOX_PATH.into());
    env.insert("HOME".into(), home.display().to_string());
    env.insert("LANG".into(), "C".into());
    env.insert("TMPDIR".into(), "/tmp".into());
    env
}

/// Short-lived `helper prepare` then managed argv `helper run --plan`.
/// Failure is [`ErrorCode::ProcessSpawnFailed`] (no managed child).
pub fn prepare_run(
    workspace_root: &Path,
    command_cwd: &Path,
    writable_workspace: bool,
    network: SandboxNetwork,
    argv: &[String],
) -> Result<SandboxLaunch, ErrorBody> {
    let helper = helper_path().ok_or_else(|| {
        spawn_failed(format!(
            "{HELPER_BIN_ENV} is unset and {HELPER_BIN_NAME} was not found next to the executable"
        ))
    })?;
    prepare_run_from_helper(
        &helper,
        workspace_root,
        command_cwd,
        writable_workspace,
        network,
        argv,
    )
}

pub(crate) fn prepare_run_from_helper(
    helper: &Path,
    workspace_root: &Path,
    command_cwd: &Path,
    writable_workspace: bool,
    network: SandboxNetwork,
    argv: &[String],
) -> Result<SandboxLaunch, ErrorBody> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err(spawn_failed("command must be a non-empty argv (no shell)"));
    }
    let request = SandboxPrepareRequest {
        protocol: SANDBOX_HELPER_PROTOCOL,
        workspace_root: utf8_path(workspace_root)?,
        command_cwd: utf8_path(command_cwd)?,
        writable_workspace,
        network,
        argv: argv.to_vec(),
    };
    let payload = serde_json::to_vec(&request).map_err(|err| spawn_failed(err.to_string()))?;
    let spawned = Command::new(helper)
        .arg("prepare")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| spawn_failed(format!("failed to spawn linux sandbox prepare: {err}")))?;
    prepare_from_child(helper, spawned, &payload, PREPARE_TIMEOUT, PREPARE_IO_LIMIT)
}

fn prepare_from_child(
    helper: &Path,
    mut spawned: Child,
    payload: &[u8],
    timeout: Duration,
    io_limit: usize,
) -> Result<SandboxLaunch, ErrorBody> {
    let output = communicate_prepare(&mut spawned, payload, timeout, io_limit)?;
    let response: SandboxPrepareResponse = serde_json::from_slice(&output.stdout)
        .map_err(|err| spawn_failed(format!("invalid linux sandbox prepare response: {err}")))?;
    match response {
        SandboxPrepareResponse::Prepared { plan_path } if output.status.success() => {
            if plan_path.is_empty() {
                return Err(spawn_failed(
                    "linux sandbox prepare returned an empty plan path",
                ));
            }
            let plan_path = PathBuf::from(plan_path);
            Ok(SandboxLaunch {
                program: helper.to_path_buf(),
                args: vec![
                    OsString::from("run"),
                    OsString::from("--plan"),
                    plan_path.clone().into(),
                ],
                plan_path,
            })
        }
        SandboxPrepareResponse::Error { message } => Err(spawn_failed(format!(
            "linux sandbox setup failed: {message}"
        ))),
        SandboxPrepareResponse::Prepared { .. } => {
            Err(spawn_failed("linux sandbox prepare failed"))
        }
    }
}

fn communicate_prepare(
    child: &mut Child,
    payload: &[u8],
    timeout: Duration,
    io_limit: usize,
) -> Result<Output, ErrorBody> {
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    if let Some(ref stdin) = stdin {
        set_nonblocking(stdin).map_err(|err| {
            reap(child);
            spawn_failed(format!("failed to set prepare stdin nonblocking: {err}"))
        })?;
    }
    if let Some(ref stdout) = stdout {
        set_nonblocking(stdout).map_err(|err| {
            reap(child);
            spawn_failed(format!("failed to set prepare stdout nonblocking: {err}"))
        })?;
    }
    if let Some(ref stderr) = stderr {
        set_nonblocking(stderr).map_err(|err| {
            reap(child);
            spawn_failed(format!("failed to set prepare stderr nonblocking: {err}"))
        })?;
    }

    let start = Instant::now();
    let mut written = 0usize;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut stdout_eof = stdout.is_none();
    let mut stderr_eof = stderr.is_none();
    loop {
        if start.elapsed() > timeout {
            reap(child);
            return Err(spawn_failed("linux sandbox prepare timed out"));
        }
        let stdin_done = if let Some(ref mut pipe) = stdin {
            match write_nonblocking(pipe, payload, &mut written) {
                Ok(done) => done,
                Err(io_err) => {
                    reap(child);
                    return Err(spawn_failed(format!(
                        "failed to write linux sandbox prepare request: {io_err}"
                    )));
                }
            }
        } else {
            false
        };
        if stdin_done {
            stdin = None;
        }
        let stdout_done = if let Some(ref mut pipe) = stdout {
            match read_nonblocking(pipe, &mut out, io_limit) {
                Ok(done) => done,
                Err(message) => {
                    reap(child);
                    return Err(spawn_failed(message));
                }
            }
        } else {
            false
        };
        if stdout_done {
            stdout_eof = true;
            stdout = None;
        }
        let stderr_done = if let Some(ref mut pipe) = stderr {
            match read_nonblocking(pipe, &mut err, io_limit) {
                Ok(done) => done,
                Err(message) => {
                    reap(child);
                    return Err(spawn_failed(message));
                }
            }
        } else {
            false
        };
        if stderr_done {
            stderr_eof = true;
            stderr = None;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if stdout_eof && stderr_eof {
                    return Ok(Output {
                        status,
                        stdout: out,
                        stderr: err,
                    });
                }
            }
            Ok(None) => {}
            Err(wait_err) => {
                reap(child);
                return Err(spawn_failed(wait_err.to_string()));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn write_nonblocking(
    stdin: &mut ChildStdin,
    payload: &[u8],
    written: &mut usize,
) -> io::Result<bool> {
    if *written >= payload.len() {
        return Ok(true);
    }
    match stdin.write(&payload[*written..]) {
        Ok(0) => Err(io::Error::new(
            ErrorKind::WriteZero,
            "prepare stdin closed before request was written",
        )),
        Ok(n) => {
            *written += n;
            Ok(*written >= payload.len())
        }
        Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::Interrupted => {
            Ok(false)
        }
        Err(err) => Err(err),
    }
}

fn read_nonblocking(
    pipe: &mut impl Read,
    buf: &mut Vec<u8>,
    io_limit: usize,
) -> Result<bool, String> {
    let mut tmp = [0u8; 4096];
    match pipe.read(&mut tmp) {
        Ok(0) => Ok(true),
        Ok(n) => {
            if buf.len().saturating_add(n) > io_limit {
                return Err("linux sandbox prepare output too large".into());
            }
            buf.extend_from_slice(&tmp[..n]);
            Ok(false)
        }
        Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::Interrupted => {
            Ok(false)
        }
        Err(err) => Err(err.to_string()),
    }
}

fn set_nonblocking<T: AsRawFd>(io: &T) -> io::Result<()> {
    let fd = io.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn utf8_path(path: &Path) -> Result<String, ErrorBody> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        spawn_failed(format!(
            "sandbox path must be valid UTF-8: {}",
            path.display()
        ))
    })
}

fn spawn_failed(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::ProcessSpawnFailed, message.into())
}

/// Drop a leftover opaque plan after a failed helper OS spawn. Unlinks
/// the file, then the empty `codespace-linux-sandbox-*` parent dir.
pub(crate) fn discard_plan(path: &Path) {
    let parent = path.parent().map(Path::to_path_buf);
    let _ = std::fs::remove_file(path);
    if let Some(parent) = parent {
        if parent.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with("codespace-linux-sandbox-")
        }) {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("codespace-linux-sandbox");
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn probe_is_false_off_linux() {
        if !cfg!(target_os = "linux") {
            assert!(!probe());
        }
    }

    #[test]
    fn sandbox_env_uses_fixed_path_and_workspace_home() {
        let env = sandbox_exec_env(Path::new("/workspace"));
        assert_eq!(env.get("PATH").unwrap(), SANDBOX_PATH);
        assert_eq!(env.get("HOME").unwrap(), "/workspace");
        assert_eq!(env.get("TMPDIR").unwrap(), "/tmp");
        assert!(!env.get("PATH").unwrap().contains(".cargo/bin"));
    }

    #[test]
    fn missing_helper_is_process_spawn_failed() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("missing");
        let err = prepare_run_from_helper(
            &helper,
            dir.path(),
            dir.path(),
            true,
            SandboxNetwork::Restricted,
            &["/bin/true".into()],
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
    }

    #[test]
    fn prepare_error_json_is_process_spawn_failed() {
        let dir = tempfile::tempdir().unwrap();
        let helper = write_script(
            dir.path(),
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"type\":\"error\",\"message\":\"nope\"}'\nexit 1\n",
        );
        let err = prepare_run_from_helper(
            &helper,
            dir.path(),
            dir.path(),
            true,
            SandboxNetwork::Restricted,
            &["/bin/true".into()],
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
        assert!(err.message.contains("nope"), "{}", err.message);
    }

    #[test]
    fn prepare_invalid_json_is_process_spawn_failed() {
        let dir = tempfile::tempdir().unwrap();
        let helper = write_script(
            dir.path(),
            "#!/bin/sh\ncat >/dev/null\necho not-json\nexit 0\n",
        );
        let err = prepare_run_from_helper(
            &helper,
            dir.path(),
            dir.path(),
            true,
            SandboxNetwork::Restricted,
            &["/bin/true".into()],
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
        assert!(
            err.message
                .contains("invalid linux sandbox prepare response"),
            "{}",
            err.message
        );
    }

    #[test]
    fn protocol_constant_is_one() {
        assert_eq!(SANDBOX_HELPER_PROTOCOL, 1);
    }

    #[test]
    fn prepare_unread_large_stdin_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let helper = write_script(dir.path(), "#!/bin/sh\nexec sleep 30\n");
        let spawned = Command::new(&helper)
            .arg("prepare")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = vec![b'x'; 256 * 1024];
        let start = Instant::now();
        let err = prepare_from_child(
            &helper,
            spawned,
            &payload,
            Duration::from_millis(300),
            PREPARE_IO_LIMIT,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
        assert!(err.message.contains("timed out"), "{}", err.message);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "prepare hung: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn prepare_stdout_flood_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let helper = write_script(dir.path(), "#!/bin/sh\nwhile :; do printf x; done\n");
        let spawned = Command::new(&helper)
            .arg("prepare")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let err = prepare_from_child(&helper, spawned, b"{}", Duration::from_secs(2), 64 * 1024)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
        assert!(err.message.contains("output too large"), "{}", err.message);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "prepare hung: {:?}",
            start.elapsed()
        );
    }

    // ---------------------------------------------------------------------
    // DIAGNOSTIC ONLY: branch codex/diag-etxtbsy-fixture, never merged. Each
    // test panics on purpose so that its outcome appears in the CI log.

    fn write_in_process<T>(dir: &Path, body: &str, meanwhile: impl FnOnce() -> T) -> (PathBuf, T) {
        // The current fixture's method: this process holds the file open for writing.
        let path = dir.join("codespace-linux-sandbox");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        let value = meanwhile();
        drop(file);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        (path, value)
    }

    fn write_by_child<T>(dir: &Path, body: &str, meanwhile: impl FnOnce() -> T) -> (PathBuf, T) {
        // The proposed method: only a short-lived /bin/sh child writes the file.
        let path = dir.join("codespace-linux-sandbox");
        let mut writer = Command::new("/bin/sh")
            .args(["-c", "printf '%s' \"$2\" > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(&path)
            .arg(body)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let value = meanwhile();
        assert!(writer.wait().unwrap().success());
        (path, value)
    }

    /// Fork a child that stays between fork and exec for two seconds, as a
    /// child spawned by a concurrent test does for a moment, and return once
    /// it exists.
    #[cfg(target_os = "linux")]
    fn fork_and_hold() -> std::thread::JoinHandle<std::process::ExitStatus> {
        use std::os::unix::process::CommandExt;
        let (ready, signal) = std::os::unix::net::UnixStream::pair().unwrap();
        let holder = std::thread::spawn(move || {
            let fd = signal.as_raw_fd();
            let mut command = Command::new("/bin/true");
            // SAFETY: the hook calls only async-signal-safe functions.
            unsafe {
                command.pre_exec(move || {
                    let byte = 1u8;
                    libc::write(fd, (&byte as *const u8).cast(), 1);
                    let pause = libc::timespec { tv_sec: 2, tv_nsec: 0 };
                    libc::nanosleep(&pause, std::ptr::null_mut());
                    Ok(())
                });
            }
            command.status().unwrap()
        });
        let mut byte = [0u8; 1];
        (&ready).read_exact(&mut byte).unwrap();
        holder
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn diagnostic_in_process_write_during_a_fork() {
        let dir = tempfile::tempdir().unwrap();
        let (helper, holder) = write_in_process(dir.path(), "#!/bin/sh\nexit 0\n", fork_and_hold);
        let started = Command::new(&helper).status();
        let held = holder.join().unwrap();
        panic!("DIAGNOSTIC in-process writer: exec in the fork window -> {started:?}; holder {held:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn diagnostic_child_write_during_a_fork() {
        let dir = tempfile::tempdir().unwrap();
        let (helper, holder) = write_by_child(dir.path(), "#!/bin/sh\nexit 0\n", fork_and_hold);
        let started = Command::new(&helper).status();
        let held = holder.join().unwrap();
        panic!("DIAGNOSTIC child writer: exec in the fork window -> {started:?}; holder {held:?}");
    }

    /// Natural rate under load: 8 threads write and start a script 200 times
    /// each while 8 threads keep spawning /bin/true.
    #[cfg(target_os = "linux")]
    #[test]
    fn diagnostic_busy_counts_under_concurrent_spawns() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        const WRITERS: usize = 8;
        const ITERATIONS: usize = 200;
        const SPAWNERS: usize = 8;
        fn run(in_process: bool) -> (usize, usize) {
            let stop = Arc::new(AtomicBool::new(false));
            let spawners: Vec<_> = (0..SPAWNERS)
                .map(|_| {
                    let stop = Arc::clone(&stop);
                    std::thread::spawn(move || {
                        let mut spawned: usize = 0;
                        while !stop.load(Ordering::Relaxed) {
                            let _ = Command::new("/bin/true").status();
                            spawned += 1;
                        }
                        spawned
                    })
                })
                .collect();
            let writers: Vec<_> = (0..WRITERS)
                .map(|_| {
                    std::thread::spawn(move || {
                        let dir = tempfile::tempdir().unwrap();
                        let mut busy: usize = 0;
                        for _ in 0..ITERATIONS {
                            let body = "#!/bin/sh\nexit 0\n";
                            let (path, ()) = if in_process {
                                write_in_process(dir.path(), body, || ())
                            } else {
                                write_by_child(dir.path(), body, || ())
                            };
                            match Command::new(&path).status() {
                                Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) => busy += 1,
                                Err(err) => panic!("unexpected start failure: {err}"),
                                Ok(status) => assert!(status.success()),
                            }
                            std::fs::remove_file(&path).unwrap();
                        }
                        busy
                    })
                })
                .collect();
            let busy: usize = writers.into_iter().map(|w| w.join().unwrap()).sum();
            stop.store(true, Ordering::Relaxed);
            let spawned: usize = spawners.into_iter().map(|s| s.join().unwrap()).sum();
            (busy, spawned)
        }
        let (old_busy, old_spawned) = run(true);
        let (new_busy, new_spawned) = run(false);
        let starts = WRITERS * ITERATIONS;
        panic!(
            "DIAGNOSTIC busy counts: in-process writer {old_busy} ETXTBSY of {starts} starts \
             ({old_spawned} concurrent spawns); child writer {new_busy} of {starts} ({new_spawned})"
        );
    }
}
