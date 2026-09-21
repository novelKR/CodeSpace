//! Spawn the worker binary against a real socket file.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{ProcessId, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{
    read_frame, write_frame, Runner, RunnerExecRequest, RunnerOp, RunnerOpResult, UdsRunner,
    WireEnvelope, WIRE_PROTOCOL,
};
use tokio::net::UnixStream;

const BIN: &str = env!("CARGO_BIN_EXE_codespace-codex-runtime");

fn runner_socket(dir: &Path) -> PathBuf {
    dir.join("runner.sock")
}

async fn wait_connect(socket: &Path) -> UnixStream {
    for _ in 0..100 {
        if let Ok(stream) = UnixStream::connect(socket).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("worker socket not ready at {}", socket.display());
}

async fn spawn_worker(socket: &Path) -> tokio::process::Child {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    tokio::process::Command::new(BIN)
        .arg(socket)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn codespace-codex-runtime")
}

#[tokio::test]
async fn dedicated_leaf_stays_0700_and_parent_mode_is_unchanged() {
    let parent = tempfile::tempdir().unwrap();
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let leaf = parent.path().join("leaf");
    std::fs::create_dir(&leaf).unwrap();
    std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runner_socket(&leaf);
    let _worker = spawn_worker(&socket).await;
    let stream = wait_connect(&socket).await;
    let parent_mode = std::fs::metadata(parent.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let leaf_mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
    assert_eq!(parent_mode, 0o755);
    assert_eq!(leaf_mode, 0o700);
    drop(stream);
}

#[tokio::test]
async fn parent_0755_is_not_chmodded_when_socket_lives_there() {
    let parent = tempfile::tempdir().unwrap();
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = runner_socket(parent.path());
    let _worker = spawn_worker(&socket).await;
    let stream = wait_connect(&socket).await;
    let parent_mode = std::fs::metadata(parent.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(parent_mode, 0o755);
    drop(stream);
}

#[tokio::test]
async fn second_instance_does_not_unlink_live_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = runner_socket(dir.path());
    let _worker = spawn_worker(&socket).await;
    let stream = wait_connect(&socket).await;
    assert!(socket.exists());
    let second = Command::new(BIN)
        .arg(&socket)
        .output()
        .expect("second worker");
    assert!(
        !second.status.success(),
        "second worker stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(socket.exists(), "live socket must not be unlinked");
    drop(stream);
}

#[tokio::test]
async fn protocol_mismatch_responds_then_exits() {
    let dir = tempfile::tempdir().unwrap();
    let socket = runner_socket(dir.path());
    let mut worker = spawn_worker(&socket).await;
    let mut stream = wait_connect(&socket).await;
    let mut envelope = WireEnvelope::request("rrpc-bad".into(), RunnerOp::Hello);
    envelope.protocol = 99;
    write_frame(&mut stream, &envelope).await.unwrap();
    let reply = read_frame(&mut stream).await.unwrap().unwrap();
    let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
    assert_eq!(parsed.ok, Some(false));
    assert!(parsed.error.is_some());
    let eof = read_frame(&mut stream).await.unwrap();
    assert!(eof.is_none());
    let status = tokio::time::timeout(Duration::from_secs(2), worker.wait())
        .await
        .expect("worker should exit after mismatch")
        .expect("wait worker");
    let _ = status;
}

#[tokio::test]
async fn hello_on_live_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = runner_socket(dir.path());
    let _worker = spawn_worker(&socket).await;
    let mut stream = wait_connect(&socket).await;
    write_frame(
        &mut stream,
        &WireEnvelope::request("rrpc-hello".into(), RunnerOp::Hello),
    )
    .await
    .unwrap();
    let reply = read_frame(&mut stream).await.unwrap().unwrap();
    let parsed: WireEnvelope = serde_json::from_slice(&reply).unwrap();
    match parsed.result {
        Some(RunnerOpResult::Hello { protocol }) => {
            assert_eq!(protocol, WIRE_PROTOCOL);
            assert_eq!(protocol, 6);
        }
        other => panic!("unexpected hello {other:?}"),
    }
}

#[tokio::test]
async fn worker_socket_drop_terminates_owned_child() {
    let dir = tempfile::tempdir().unwrap();
    let socket = runner_socket(dir.path());
    let mut worker = spawn_worker(&socket).await;
    let stream = wait_connect(&socket).await;
    let runner = UdsRunner::from_stream(stream, Arc::new(|_| {}));
    runner.handshake().await.expect("hello");

    let ws_dir = tempfile::tempdir().unwrap();
    let beat = ws_dir.path().join("heartbeat");
    let ws = Workspace::new(
        WorkspaceId("demo".into()),
        ws_dir.path().to_path_buf(),
        Profile::WorkspaceWrite,
    );
    let req = RunnerExecRequest::for_host(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "while :; do date +%s > '{}'; sleep 0.1; done",
                beat.display()
            ),
        ],
        ProcessId("proc-uds-disconnect".into()),
        Profile::WorkspaceWrite,
    );
    runner.exec(&ws, req).await.expect("exec heartbeat");

    let mut last = String::new();
    for _ in 0..50 {
        if let Ok(raw) = std::fs::read_to_string(&beat) {
            if !raw.trim().is_empty() {
                last = raw;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        !last.is_empty(),
        "child should write a heartbeat before disconnect"
    );

    runner.close().await;
    drop(runner);
    let _ = tokio::time::timeout(Duration::from_secs(5), worker.wait())
        .await
        .expect("worker should exit after socket close")
        .expect("wait worker");

    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = std::fs::read_to_string(&beat).unwrap_or_default();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let later = std::fs::read_to_string(&beat).unwrap_or_default();
    assert_eq!(
        after, later,
        "owned child must stop updating heartbeat after worker disconnect (after={after:?} later={later:?})"
    );
}
