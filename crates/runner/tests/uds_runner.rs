//! UdsRunner talks length-prefixed CodeSpace JSON over a Unix stream pair.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use codespace_domain::{ErrorCode, ProcessId, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{
    host_worker, serve_runner_connection, Runner, RunnerError, RunnerExecRequest, UdsRunner,
};
use tempfile::tempdir;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

fn workspace(root: &std::path::Path) -> Workspace {
    Workspace::new(
        WorkspaceId("demo".into()),
        root.to_path_buf(),
        Profile::WorkspaceWrite,
    )
}

async fn drop_after_one_frame(stream: UnixStream) {
    let (mut read, _write) = stream.into_split();
    let mut len_buf = [0u8; 4];
    if read.read_exact(&mut len_buf).await.is_err() {
        return;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    let _ = read.read_exact(&mut payload).await;
}

#[tokio::test]
async fn hello_handshake_succeeds() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    runner.handshake().await.expect("hello");
}

#[tokio::test]
async fn uds_runner_read_and_exec_over_length_prefix() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let ws = workspace(dir.path());
    let read = runner.read(&ws, "a.txt").await.unwrap();
    assert_eq!(read.content, "hi");

    let process_id = ProcessId("proc-uds".into());
    runner
        .exec(
            &ws,
            RunnerExecRequest::for_host(
                vec!["/bin/echo".into(), "ok".into()],
                process_id.clone(),
                Profile::WorkspaceWrite,
            ),
        )
        .await
        .unwrap();
    let mut chunk = String::new();
    for _ in 0..50 {
        let result = runner
            .read_process(codespace_runner::RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        chunk = result.chunk;
        if result.eof {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(chunk.contains("ok"), "chunk={chunk:?}");

    std::env::set_var(
        "CODESPACE_PATCH_BIN",
        codespace_runner::ensure_helper_for_tests(),
    );
    let applied = runner
        .apply_patch(
            &ws,
            codespace_runner::RunnerApplyPatchRequest {
                patch: "*** Begin Patch\n*** Add File: b.txt\n+hello\n*** End Patch\n".into(),
                expected_versions: Default::default(),
                check_only: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(applied.status, codespace_domain::PatchStatus::Applied);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        "hello\n"
    );
}

#[tokio::test]
async fn uds_apply_patch_lost_response_is_ambiguous() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    tokio::spawn(async move {
        drop_after_one_frame(server).await;
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    let err = runner
        .apply_patch(
            &ws,
            codespace_runner::RunnerApplyPatchRequest {
                patch: "*** Begin Patch\n*** Add File: lost.txt\n+x\n*** End Patch\n".into(),
                expected_versions: Default::default(),
                check_only: false,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, RunnerError::TransportAmbiguous { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn uds_exec_lost_response_is_ambiguous() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    tokio::spawn(async move {
        drop_after_one_frame(server).await;
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    let err = runner
        .exec(
            &ws,
            RunnerExecRequest::for_host(
                vec!["/bin/echo".into(), "nope".into()],
                ProcessId("proc-lost".into()),
                Profile::WorkspaceWrite,
            ),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, RunnerError::TransportAmbiguous { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn uds_missing_executable_respects_sandbox_spawn_boundary() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    let process_id = ProcessId("proc-uds-missing".into());
    let result = runner
        .exec(
            &ws,
            RunnerExecRequest::for_host(
                vec!["/no/such/codespace-exec".into()],
                process_id.clone(),
                Profile::WorkspaceWrite,
            ),
        )
        .await;

    if codespace_runner::linux_sandbox_available() {
        let spawned = result.expect("sandbox helper should spawn successfully");
        assert_eq!(spawned.process_id, process_id);
        for _ in 0..50 {
            let read = runner
                .read_process(codespace_runner::RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            if read.eof {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    } else {
        let err = result.unwrap_err();
        match err {
            RunnerError::Execution(body) => {
                assert_eq!(body.code, ErrorCode::ProcessSpawnFailed, "{body:?}");
            }
            other => panic!("expected Execution(ProcessSpawnFailed), got {other:?}"),
        }
    }
}

#[tokio::test]
async fn process_exited_event_reaches_gateway_callback() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let hits = Arc::new(AtomicUsize::new(0));
    let flag = hits.clone();
    let runner = UdsRunner::from_stream(
        client,
        Arc::new(move |process_id: &str| {
            if process_id == "proc-exit" {
                flag.fetch_add(1, Ordering::SeqCst);
            }
        }),
    );
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    runner
        .exec(
            &ws,
            RunnerExecRequest::for_host(
                vec!["/bin/echo".into(), "done".into()],
                ProcessId("proc-exit".into()),
                Profile::WorkspaceWrite,
            ),
        )
        .await
        .unwrap();
    for _ in 0..50 {
        if hits.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(hits.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn closed_socket_before_call_is_before_dispatch() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    drop(server);
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    let err = runner
        .exec(
            &ws,
            RunnerExecRequest::for_host(
                vec!["/bin/echo".into()],
                ProcessId("proc-closed".into()),
                Profile::WorkspaceWrite,
            ),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, RunnerError::TransportBeforeDispatch { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn uds_tty_exec_sees_a_tty() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    let process_id = ProcessId("proc-uds-tty".into());
    let mut req = RunnerExecRequest::for_host(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "if [ -t 0 ]; then echo ISATTY; else echo NOTTY; fi".into(),
        ],
        process_id.clone(),
        Profile::WorkspaceWrite,
    );
    req.tty = true;
    runner.exec(&ws, req).await.unwrap();
    let mut chunk = String::new();
    for _ in 0..50 {
        let result = runner
            .read_process(codespace_runner::RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        chunk = result.chunk;
        if result.eof {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        chunk.contains("ISATTY"),
        "UDS worker should spawn a PTY when tty is true, got {chunk:?}"
    );
}

fn execution_code(err: &RunnerError) -> ErrorCode {
    match err {
        RunnerError::Execution(body) => body.code,
        other => panic!("expected Execution error, got {other:?}"),
    }
}

#[tokio::test]
async fn uds_read_filesystem_errors_keep_product_codes() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("foo"), "not-a-dir").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();
    let fifo = dir.path().join("pipe.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo should exist");
    let ws = workspace(dir.path());

    assert_eq!(
        execution_code(&runner.read(&ws, "missing.txt").await.unwrap_err()),
        ErrorCode::FileNotFound
    );
    assert_eq!(
        execution_code(&runner.read(&ws, "foo/bar.txt").await.unwrap_err()),
        ErrorCode::PathNotDirectory
    );
    assert_eq!(
        execution_code(&runner.read(&ws, "../outside").await.unwrap_err()),
        ErrorCode::PathEscape
    );
    assert_eq!(
        execution_code(&runner.read(&ws, "link").await.unwrap_err()),
        ErrorCode::SymlinkRejected
    );
    assert_eq!(
        execution_code(&runner.read(&ws, "pipe.fifo").await.unwrap_err()),
        ErrorCode::SpecialFileRejected
    );
}

#[tokio::test]
async fn uds_find_on_deleted_root_is_file_operation_failed() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let (worker, events) = host_worker();
    tokio::spawn(async move {
        serve_runner_connection(server, worker, events)
            .await
            .expect("serve runner");
    });
    let runner = UdsRunner::from_stream(client, Arc::new(|_| {}));
    let dir = tempdir().unwrap();
    let ws = workspace(dir.path());
    drop(dir);
    assert_eq!(
        execution_code(&runner.find(&ws, None).await.unwrap_err()),
        ErrorCode::FileOperationFailed
    );
}
