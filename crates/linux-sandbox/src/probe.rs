//! Cached-at-the-caller `helper probe`: Minimal+workspace+Restricted
//! `/usr/bin/true` (or `/bin/true`) through the same prepare/run path.
//! Exit 0 only when that inner command succeeds.

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use codespace_linux_sandbox_protocol::{
    SandboxNetwork, SandboxPrepareRequest, SANDBOX_HELPER_PROTOCOL,
};

#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const REQUIRE_ENV: &str = "CODESPACE_REQUIRE_LINUX_SANDBOX";

pub fn main() {
    std::process::exit(if probe_ok() { 0 } else { 1 });
}

#[cfg(not(target_os = "linux"))]
fn probe_ok() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn require_linux_sandbox() -> bool {
    std::env::var_os(REQUIRE_ENV).is_some_and(|value| value == "1")
}

#[cfg(target_os = "linux")]
fn probe_ok() -> bool {
    let require = require_linux_sandbox();
    let Ok(exe) = std::env::current_exe() else {
        if require {
            eprintln!("linux sandbox probe failed: current_exe missing");
        }
        return false;
    };
    let workspace = probe_workspace();
    let request = SandboxPrepareRequest {
        protocol: SANDBOX_HELPER_PROTOCOL,
        workspace_root: workspace.to_string_lossy().into_owned(),
        command_cwd: workspace.to_string_lossy().into_owned(),
        writable_workspace: true,
        network: SandboxNetwork::Restricted,
        argv: vec![true_command()],
    };
    let plan = match crate::prepare::linux_sandbox_args(&request, &exe)
        .and_then(crate::plan::write_argv)
    {
        Ok(plan) => plan,
        Err(err) => {
            if require {
                eprintln!("linux sandbox probe prepare failed: {err}");
            }
            return false;
        }
    };
    let mut child = Command::new(&exe);
    child
        .arg("run")
        .arg("--plan")
        .arg(&plan)
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if require {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
    let child = match child.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = std::fs::remove_file(&plan);
            if require {
                eprintln!("linux sandbox probe spawn failed: {err}");
            }
            return false;
        }
    };
    match wait_with_timeout(child, PROBE_TIMEOUT) {
        Some(status) if status.success() => true,
        Some(status) => {
            if require {
                eprintln!("linux sandbox probe exited with {status}");
            }
            false
        }
        None => {
            if require {
                eprintln!("linux sandbox probe timed out after {PROBE_TIMEOUT:?}");
            }
            false
        }
    }
}

#[cfg(target_os = "linux")]
fn true_command() -> String {
    for candidate in ["/usr/bin/true", "/bin/true"] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    "true".to_string()
}

#[cfg(target_os = "linux")]
fn probe_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join("codespace-linux-sandbox-probe");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(target_os = "linux")]
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
