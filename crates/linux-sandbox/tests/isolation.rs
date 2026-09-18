//! Linux isolation checks against the helper binary. Non-Linux compiles
//! this file but skips the runtime assertions.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use codespace_linux_sandbox::{
    prepare_from_helper, probe_helper, sandbox_exec_env, SandboxExecSpec, SandboxLaunch,
    SandboxNetwork,
};

fn helper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codespace-linux-sandbox"))
}

fn spec(root: &Path) -> SandboxExecSpec {
    SandboxExecSpec {
        workspace_root: root.to_path_buf(),
        writable_workspace: true,
        network: SandboxNetwork::Restricted,
    }
}

fn launch(root: &Path, command: &[String]) -> SandboxLaunch {
    prepare_from_helper(&helper_bin(), &spec(root), command, root).expect("prepare")
}

fn run_ok(root: &Path, command: &[String]) -> String {
    let launch = launch(root, command);
    let output = Command::new(&launch.program)
        .args(&launch.args)
        .current_dir(root)
        .env_clear()
        .envs(sandbox_exec_env(root))
        .stdin(Stdio::null())
        .output()
        .expect("spawn helper");
    assert!(
        output.status.success(),
        "status={} stderr={} stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_status(root: &Path, command: &[String]) -> std::process::ExitStatus {
    let launch = launch(root, command);
    Command::new(&launch.program)
        .args(&launch.args)
        .current_dir(root)
        .env_clear()
        .envs(sandbox_exec_env(root))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn helper")
}

fn linux_ready() -> bool {
    cfg!(target_os = "linux") && probe_helper(&helper_bin())
}

fn python3() -> Option<&'static Path> {
    ["/usr/bin/python3", "/bin/python3"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
}

/// Spawn the helper on a PTY (same argv as pipe). Used for isatty + stdin.
fn run_pty(root: &Path, command: &[String], stdin: Option<&str>) -> String {
    let python = python3().expect("python3 for PTY checks");
    let launch = launch(root, command);
    let env_json = serde_json::to_string(&sandbox_exec_env(root)).expect("env json");
    let script = r#"
import json, os, pty, select, sys, time
env = json.loads(os.environ["CODESPACE_SANDBOX_ENV"])
stdin_data = os.environ.get("CODESPACE_PTY_STDIN") or ""
helper = sys.argv[1]
args = sys.argv[2:]
pid, fd = pty.fork()
if pid == 0:
    os.environ.clear()
    os.environ.update(env)
    os.execv(helper, [helper] + args)
if stdin_data:
    time.sleep(0.05)
    os.write(fd, stdin_data.encode())
data = b""
deadline = time.time() + 8
reaped = False
while time.time() < deadline:
    ready, _, _ = select.select([fd], [], [], 0.2)
    if fd in ready:
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        data += chunk
    wpid, _ = os.waitpid(pid, os.WNOHANG)
    if wpid != 0:
        reaped = True
        while True:
            ready, _, _ = select.select([fd], [], [], 0.05)
            if fd not in ready:
                break
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            data += chunk
        break
if not reaped:
    os.kill(pid, 9)
    os.waitpid(pid, 0)
sys.stdout.buffer.write(data)
"#;
    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .arg(&launch.program)
        .args(&launch.args)
        .current_dir(root)
        .env("CODESPACE_SANDBOX_ENV", env_json)
        .env("CODESPACE_PTY_STDIN", stdin.unwrap_or(""))
        .output()
        .expect("spawn python pty wrapper");
    assert!(
        output.status.success(),
        "pty wrapper status={} stderr={} stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn helper_probe_matches_platform() {
    if cfg!(target_os = "linux") {
        if !probe_helper(&helper_bin()) {
            eprintln!("skip: bubblewrap/userns/seccomp probe failed");
        }
    } else {
        assert!(!probe_helper(&helper_bin()));
    }
}

#[test]
fn workspace_write_and_outside_path_hidden() {
    if !linux_ready() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let ssh_dir = dir.path().join("home").join(".ssh");
    std::fs::create_dir_all(&ssh_dir).unwrap();
    let secret = ssh_dir.join("id_ed25519");
    std::fs::write(&secret, "leak").unwrap();

    run_ok(
        &ws,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "echo inside > visible.txt && cat visible.txt".into(),
        ],
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("visible.txt"))
            .unwrap()
            .trim(),
        "inside"
    );

    let status = run_status(
        &ws,
        &[
            "/bin/sh".into(),
            "-c".into(),
            format!("cat '{}'", secret.display()),
        ],
    );
    assert!(
        !status.success(),
        "host path outside workspace (~/.ssh) must not be readable"
    );
}

#[test]
fn private_tmp_is_writable_and_does_not_bind_host_tmp() {
    if !linux_ready() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let host_marker = std::env::temp_dir().join("codespace-linux-sandbox-host-tmp-marker");
    std::fs::write(&host_marker, "host").unwrap();

    let stdout = run_ok(
        dir.path(),
        &[
            "/bin/sh".into(),
            "-c".into(),
            "mkdir -p /tmp && echo sandboxed > /tmp/codespace-tmp && cat /tmp/codespace-tmp".into(),
        ],
    );
    assert!(stdout.contains("sandboxed"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(&host_marker).unwrap().trim(),
        "host"
    );
    let _ = std::fs::remove_file(&host_marker);
}

#[test]
fn inet_socket_denied_unix_socket_allowed() {
    if !linux_ready() {
        return;
    }
    let Some(python) = python3() else {
        eprintln!("skip: python3 not present for socket checks");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let inet = run_status(
        dir.path(),
        &[
            python.display().to_string(),
            "-c".into(),
            "import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)".into(),
        ],
    );
    assert!(!inet.success(), "AF_INET must be denied");

    let unix = run_status(
        dir.path(),
        &[
            python.display().to_string(),
            "-c".into(),
            "import socket; socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)".into(),
        ],
    );
    assert!(unix.success(), "AF_UNIX must be allowed");
}

#[test]
fn terminate_kills_sandbox_tree() {
    if !linux_ready() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let launch = launch(dir.path(), &["/bin/sleep".into(), "30".into()]);
    let mut child = Command::new(&launch.program)
        .args(&launch.args)
        .current_dir(dir.path())
        .env_clear()
        .envs(sandbox_exec_env(dir.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    std::thread::sleep(Duration::from_millis(100));
    child.kill().expect("kill helper");
    let start = Instant::now();
    loop {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "sandbox tree did not die with the helper"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn pty_isatty_and_stdin_roundtrip() {
    if !linux_ready() {
        return;
    }
    if python3().is_none() {
        eprintln!("skip: python3 not present for PTY checks");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let isatty = run_pty(
        dir.path(),
        &[
            "/bin/sh".into(),
            "-c".into(),
            "if [ -t 0 ]; then echo ISATTY; else echo NOTTY; fi".into(),
        ],
        None,
    );
    assert!(
        isatty.contains("ISATTY"),
        "helper argv on a PTY must see a TTY, got {isatty:?}"
    );
    assert!(!isatty.contains("NOTTY"), "chunk={isatty:?}");

    let echo = run_pty(
        dir.path(),
        &[
            "/bin/sh".into(),
            "-c".into(),
            "IFS= read -r line; printf 'got:%s\\n' \"$line\"".into(),
        ],
        Some("hello\n"),
    );
    assert!(
        echo.contains("hello"),
        "PTY stdin roundtrip through helper, got {echo:?}"
    );
}
