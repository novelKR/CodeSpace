//! Helper CLI handshake: invalid prepare is a failed helper process,
//! not a managed user command.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn helper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codespace-linux-sandbox"))
}

fn prepare(request: serde_json::Value) -> (std::process::ExitStatus, serde_json::Value, String) {
    let mut child = Command::new(helper_bin())
        .arg("prepare")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn prepare");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .expect("write request");
    let output = child.wait_with_output().expect("wait prepare");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let response = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (output.status, response, stdout)
}

#[test]
fn prepare_protocol_mismatch_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let (status, response, stdout) = prepare(serde_json::json!({
        "protocol": 99,
        "workspace_root": dir.path(),
        "command_cwd": dir.path(),
        "writable_workspace": true,
        "network": "restricted",
        "argv": ["/bin/true"],
    }));
    assert!(!status.success(), "{stdout}");
    assert_eq!(response["type"], "error", "{stdout}");
    assert!(
        response["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unsupported sandbox helper protocol"),
        "{stdout}"
    );
}

#[test]
fn prepare_empty_argv_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let (status, response, stdout) = prepare(serde_json::json!({
        "protocol": 1,
        "workspace_root": dir.path(),
        "command_cwd": dir.path(),
        "writable_workspace": true,
        "network": "restricted",
        "argv": [],
    }));
    assert!(!status.success(), "{stdout}");
    assert_eq!(response["type"], "error", "{stdout}");
    assert!(
        response["message"]
            .as_str()
            .unwrap_or_default()
            .contains("non-empty argv"),
        "{stdout}"
    );
}

#[test]
fn probe_is_false_off_linux() {
    if cfg!(target_os = "linux") {
        return;
    }
    let status = Command::new(helper_bin())
        .arg("probe")
        .status()
        .expect("probe");
    assert!(!status.success());
}

#[test]
fn run_missing_plan_exits_nonzero() {
    let status = Command::new(helper_bin())
        .args(["run", "--plan", "/no/such/codespace-sandbox.plan"])
        .status()
        .expect("run");
    assert!(!status.success());
}
