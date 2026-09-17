//! ContainerRunner talks CodeSpace JSON over a Unix stream pair.

use std::sync::Arc;

use codespace_domain::{ProcessId, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{
    serve_runner_connection, ContainerRunner, InProcessRunner, Runner, RunnerExecRequest,
};
use tempfile::tempdir;
use tokio::net::UnixStream;

#[tokio::test]
async fn container_runner_read_and_exec_over_uds() {
    let (client, server) = UnixStream::pair().expect("unix pair");
    let worker = InProcessRunner::new(Arc::new(|_| {}));
    tokio::spawn(async move {
        serve_runner_connection(server, worker)
            .await
            .expect("serve runner");
    });
    let runner = ContainerRunner::from_stream(client);
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let ws = Workspace::new(
        WorkspaceId("demo".into()),
        dir.path().to_path_buf(),
        Profile::WorkspaceWrite,
    );
    let read = runner.read(&ws, "a.txt").await.unwrap();
    assert_eq!(read.content, "hi");

    let process_id = ProcessId("proc-uds".into());
    runner
        .exec(
            &ws,
            RunnerExecRequest::for_host(
                vec!["/bin/echo".into(), "ok".into()],
                process_id.clone(),
                dir.path().to_path_buf(),
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
