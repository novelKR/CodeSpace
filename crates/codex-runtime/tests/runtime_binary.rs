//! Spawn the worker binary against a real socket file.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{ErrorCode, ProcessId, ProcessState, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{
    read_frame, write_frame, Runner, RunnerError, RunnerExecRequest, RunnerLifecycle, RunnerOp,
    RunnerOpResult, UdsRunner, WireEnvelope, WIRE_PROTOCOL,
};
use tokio::net::UnixStream;

const BIN: &str = env!("CARGO_BIN_EXE_codespace-codex-runtime");

fn runner_socket(dir: &Path) -> PathBuf {
    dir.join("runner.sock")
}

/// The worker's socket, once it accepts. A worker binary never run before can take macOS a few
/// seconds to start: it checks a new executable file on its first exec, and cargo places the
/// binary anew for each test run.
async fn wait_connect(socket: &Path) -> UnixStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(stream) = UnixStream::connect(socket).await {
            return stream;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("worker socket not ready at {}", socket.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
        Some(RunnerOpResult::Hello { protocol, .. }) => {
            assert_eq!(protocol, WIRE_PROTOCOL);
            assert_eq!(protocol, 9);
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

/// An independent Runner (P1R-C01) at `socket`, whose directory it prepares itself.
fn spawn_independent(socket: &Path) -> tokio::process::Child {
    tokio::process::Command::new(BIN)
        .arg(socket)
        .args(["--lifecycle", "independent"])
        .kill_on_drop(true)
        .spawn()
        .expect("spawn an independent Runner")
}

/// A gateway's connection to the Runner at `socket`, after its handshake.
async fn gateway(socket: &Path) -> UdsRunner {
    let runner = UdsRunner::from_stream(wait_connect(socket).await, Arc::new(|_| {}));
    runner.handshake().await.expect("hello");
    runner
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

/// A command that counts into `beat` ten times a second.
fn beating(beat: &Path, process_id: &str, tty: bool) -> RunnerExecRequest {
    let mut request = RunnerExecRequest::for_host(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "i=0; while :; do i=$((i+1)); echo $i > '{}'; sleep 0.1; done",
                beat.display()
            ),
        ],
        ProcessId(process_id.into()),
        Profile::WorkspaceWrite,
    );
    request.timeout_ms = 60_000;
    request.tty = tty;
    request
}

/// Whether the count in `beat` still goes up.
async fn still_beating(beat: &Path) -> bool {
    let read = || std::fs::read_to_string(beat).unwrap_or_default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while read().trim().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let before = read();
    tokio::time::sleep(Duration::from_millis(600)).await;
    before != read()
}

async fn exited(runner: &mut tokio::process::Child) -> std::process::ExitStatus {
    tokio::time::timeout(Duration::from_secs(15), runner.wait())
        .await
        .expect("the Runner exits")
        .expect("wait for the Runner")
}

fn demo_workspace(root: &Path) -> Workspace {
    Workspace::new(
        WorkspaceId("demo".into()),
        root.to_path_buf(),
        Profile::WorkspaceWrite,
    )
}

/// An independent Runner keeps its pipe and tty processes after its gateway detaches, the next
/// gateway inherits them but starts nothing, and a stop ends them and the Runner, which removes
/// its socket and the directory it created (P1R-C01).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_independent_runner_outlives_its_gateways_until_one_stops_it() {
    let base = tempfile::tempdir().unwrap();
    std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = base.path().join("runner");
    let socket = runner_socket(&dir);
    let mut runner = spawn_independent(&socket);
    let first = gateway(&socket).await;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(base.path()), 0o755);
    let hello = first.hello();
    assert_eq!(hello.lifecycle, RunnerLifecycle::Independent);
    assert_eq!(
        hello.identity.as_ref().map(|identity| identity.pid),
        runner.id()
    );
    assert!(!hello.previous_controller);
    // A second Runner neither takes nor removes the live socket.
    let second = Command::new(BIN)
        .arg(&socket)
        .args(["--lifecycle", "independent"])
        .output()
        .expect("second Runner");
    assert!(!second.status.success(), "{second:?}");
    assert!(socket.exists());

    let ws_dir = tempfile::tempdir().unwrap();
    let ws = demo_workspace(ws_dir.path());
    let pipe_beat = ws_dir.path().join("pipe-beat");
    let tty_beat = ws_dir.path().join("tty-beat");
    first
        .exec(&ws, beating(&pipe_beat, "proc-pipe", false))
        .await
        .unwrap();
    first
        .exec(&ws, beating(&tty_beat, "proc-tty", true))
        .await
        .unwrap();
    assert!(still_beating(&pipe_beat).await && still_beating(&tty_beat).await);
    first.detach().await.unwrap();
    // No gateway is connected, and both keep running.
    assert!(still_beating(&pipe_beat).await && still_beating(&tty_beat).await);

    let next = gateway(&socket).await;
    let inherited = next.hello();
    assert_eq!(inherited.identity, hello.identity);
    assert!(inherited.previous_controller);
    assert_eq!(inherited.inherited_processes, 2);
    assert_eq!(
        next.process_status(&ProcessId("proc-pipe".into()))
            .await
            .unwrap()
            .state,
        ProcessState::Running
    );
    match next
        .exec(
            &ws,
            beating(&ws_dir.path().join("new-beat"), "proc-new", false),
        )
        .await
    {
        Err(RunnerError::Execution(body)) => assert_eq!(body.code, ErrorCode::RecoveryPending),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(next.stop_runner().await.unwrap(), 2);
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
    assert!(!still_beating(&pipe_beat).await);
    assert!(!still_beating(&tty_beat).await);
    assert!(!socket.exists());
    assert!(!dir.exists());
}

/// The Runner's own SIGTERM stops it as a gateway's `Stop` does, and the gateway connected to it
/// refuses its later requests (P1R-C01).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_stops_an_independent_runner_and_its_processes() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("runner");
    let socket = runner_socket(&dir);
    let mut runner = spawn_independent(&socket);
    let connected = gateway(&socket).await;
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = demo_workspace(ws_dir.path());
    let beat = ws_dir.path().join("beat");
    let process_id = ProcessId("proc-pipe".into());
    connected
        .exec(&ws, beating(&beat, &process_id.0, false))
        .await
        .unwrap();
    assert!(still_beating(&beat).await);
    let pid = runner.id().unwrap().to_string();
    let sent = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(sent.success());
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
    assert!(!still_beating(&beat).await);
    assert!(!socket.exists());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match connected.process_status(&process_id).await {
            Err(RunnerError::Execution(body)) if body.code == ErrorCode::RunnerDisconnected => {
                break
            }
            other => assert!(tokio::time::Instant::now() < deadline, "{other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// An independent Runner does not start in a socket directory that is not private to its user,
/// nor without a socket or with an unknown lifecycle, and changes nothing (P1R-C01).
#[tokio::test]
async fn an_independent_runner_refuses_a_shared_directory_and_unknown_settings() {
    let base = tempfile::tempdir().unwrap();
    let shared = base.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = runner_socket(&shared);
    let refused = Command::new(BIN)
        .arg(&socket)
        .args(["--lifecycle", "independent"])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("0700"),
        "{refused:?}"
    );
    assert_eq!(mode(&shared), 0o755);
    assert!(!socket.exists());
    let socket = socket.display().to_string();
    for args in [
        &["--lifecycle", "independent"][..],
        &[&socket, "--lifecycle", "later"],
        &[&socket, "--runner-exit-intent", "stop"],
    ] {
        let refused = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(refused.status.code(), Some(2), "{args:?}: {refused:?}");
    }
}
