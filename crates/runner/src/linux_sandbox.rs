//! Client for the Linux sandbox helper process. Codex translation stays
//! behind `codespace-linux-sandbox`; this module speaks prepare/run JSON
//! and locates the binary. `sandbox_exec_env` stays here (CodeSpace env
//! semantics, not Codex).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
    let mut spawned = Command::new(helper)
        .arg("prepare")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| spawn_failed(format!("failed to spawn linux sandbox prepare: {err}")))?;
    if let Some(mut stdin) = spawned.stdin.take() {
        if let Err(err) = stdin.write_all(&payload) {
            let _ = spawned.kill();
            let _ = spawned.wait();
            return Err(spawn_failed(format!(
                "failed to write linux sandbox prepare request: {err}"
            )));
        }
    }
    let output = wait_output_with_timeout(spawned, PREPARE_TIMEOUT)?;
    if output.stdout.len() > PREPARE_IO_LIMIT || output.stderr.len() > PREPARE_IO_LIMIT {
        return Err(spawn_failed("linux sandbox prepare output too large"));
    }
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

fn wait_output_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<Output, ErrorBody> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|err| spawn_failed(err.to_string()));
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(spawn_failed("linux sandbox prepare timed out"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                let _ = child.kill();
                return Err(spawn_failed(err.to_string()));
            }
        }
    }
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
            "#!/bin/sh\nprintf '%s' '{\"type\":\"error\",\"message\":\"nope\"}'\nexit 1\n",
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
        let helper = write_script(dir.path(), "#!/bin/sh\necho not-json\nexit 0\n");
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
    fn protocol_constant_is_one() {
        assert_eq!(SANDBOX_HELPER_PROTOCOL, 1);
    }
}
