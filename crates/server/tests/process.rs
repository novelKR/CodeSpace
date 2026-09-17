use codespace_domain::{
    TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND, TOOL_READ_PROCESS, TOOL_TERMINATE_PROCESS,
    TOOL_WRITE_STDIN,
};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;
use tokio::time::{sleep, Duration};

fn payload(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    result.structured_content.clone().unwrap_or_else(|| {
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
    })
}

fn err_text(result: &Result<rmcp::model::CallToolResult, rmcp::service::ServiceError>) -> String {
    match result {
        Ok(r) => r
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join(""),
        Err(err) => err.to_string(),
    }
}

async fn spawn_client(
    cfg: &std::path::Path,
    db: &std::path::Path,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let patch_bin = codespace_server::patch_helper::ensure_helper_for_tests();
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("CODESPACE_CONFIG", cfg)
                .env("CODESPACE_OPERATIONS_DB", db)
                .env("CODESPACE_PATCH_BIN", patch_bin);
        }))
        .expect("spawn"),
    )
    .await
    .expect("init")
}

fn write_cfg(root: &std::path::Path, profile: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let ws = root.join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("keep.txt"), "keep\n").unwrap();
    let cfg = root.join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": {
                "demo": { "root": ws, "profile": profile }
            }
        })
        .to_string(),
    )
    .unwrap();
    (cfg, root.join("ops.sqlite"))
}

#[tokio::test]
async fn printf_output_is_read_from_cursor() {
    let root = tempfile::tempdir().unwrap();
    let (cfg, db) = write_cfg(root.path(), "workspace-write");
    let client = spawn_client(&cfg, &db).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sh", "-c", "printf hello-from-proc"],
                "timeout_ms": 4000
            })),
        )
        .await
        .expect("exec");
    let body = payload(&started);
    let process_id = body["process_id"].as_str().unwrap().to_string();
    assert!(process_id.starts_with("proc-"));
    assert_ne!(process_id, "1");

    let mut cursor = 0u64;
    let mut out = String::new();
    for _ in 0..50 {
        let chunk = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS).with_arguments(object!({
                    "process_id": process_id,
                    "cursor": cursor
                })),
            )
            .await
            .expect("read_process");
        let body = payload(&chunk);
        out.push_str(body["chunk"].as_str().unwrap());
        cursor = body["cursor"].as_u64().unwrap();
        if body["eof"].as_bool() == Some(true) {
            break;
        }
        sleep(Duration::from_millis(30)).await;
    }
    assert!(
        out.contains("hello-from-proc"),
        "missing process output: {out:?}"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn invented_process_id_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let (cfg, db) = write_cfg(root.path(), "workspace-write");
    let client = spawn_client(&cfg, &db).await;

    let result = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ_PROCESS).with_arguments(object!({
                "process_id": "proc-invented",
                "cursor": 0
            })),
        )
        .await;
    let text = err_text(&result);
    assert!(
        text.contains("PROCESS_NOT_FOUND") || text.contains("unknown process_id"),
        "unexpected invented handle result: {result:?}"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn busy_shell_blocks_apply_patch_until_terminated() {
    let root = tempfile::tempdir().unwrap();
    let (cfg, db) = write_cfg(root.path(), "workspace-write");
    let client = spawn_client(&cfg, &db).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sleep", "30"],
                "timeout_ms": 60000
            })),
        )
        .await
        .expect("exec sleep");
    let process_id = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    let busy = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: new.txt\n+hello\n*** End Patch\n",
                "operation_key": "during-shell"
            })),
        )
        .await;
    let busy_text = err_text(&busy);
    assert!(
        busy_text.contains("WORKSPACE_BUSY") || busy_text.contains("busy shell"),
        "expected workspace busy, got {busy:?}"
    );

    let stopped = client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": process_id })),
        )
        .await
        .expect("terminate");
    assert_eq!(payload(&stopped)["terminated"], true);

    let applied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: new.txt\n+hello\n*** End Patch\n",
                "operation_key": "after-shell"
            })),
        )
        .await
        .expect("apply after terminate");
    assert_eq!(payload(&applied)["status"], "applied");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn write_stdin_reaches_child_and_read_only_rejects_exec() {
    let root = tempfile::tempdir().unwrap();
    let (cfg, db) = write_cfg(root.path(), "workspace-write");
    let client = spawn_client(&cfg, &db).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sh", "-c", "IFS= read -r line; printf %s \"$line\""],
                "timeout_ms": 4000
            })),
        )
        .await
        .expect("exec read");
    let process_id = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_WRITE_STDIN).with_arguments(object!({
                "process_id": process_id,
                "data": "ping-stdin\n"
            })),
        )
        .await
        .expect("write_stdin");

    let mut cursor = 0u64;
    let mut out = String::new();
    for _ in 0..50 {
        let chunk = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS).with_arguments(object!({
                    "process_id": process_id,
                    "cursor": cursor
                })),
            )
            .await
            .expect("read_process");
        let body = payload(&chunk);
        out.push_str(body["chunk"].as_str().unwrap());
        cursor = body["cursor"].as_u64().unwrap();
        if body["eof"].as_bool() == Some(true) || out.contains("ping-stdin") {
            break;
        }
        sleep(Duration::from_millis(30)).await;
    }
    assert!(out.contains("ping-stdin"), "stdin not echoed: {out:?}");
    client.cancel().await.expect("cancel");

    let ro = tempfile::tempdir().unwrap();
    let (ro_cfg, ro_db) = write_cfg(ro.path(), "read-only");
    let ro_client = spawn_client(&ro_cfg, &ro_db).await;
    let denied = ro_client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "nope"]
            })),
        )
        .await;
    let denied_text = err_text(&denied);
    assert!(
        denied_text.contains("UNAUTHORIZED") || denied_text.contains("read-only"),
        "read-only exec should fail: {denied:?}"
    );
    ro_client.cancel().await.expect("cancel");
}
