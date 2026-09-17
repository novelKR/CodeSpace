//! End-to-end: info → read → patch → exec. Concurrent shell+patch is busy.

use codespace_domain::{
    TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND, TOOL_READ, TOOL_READ_PROCESS, TOOL_TERMINATE_PROCESS,
    TOOL_WORKSPACE_INFO,
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

#[tokio::test]
async fn info_read_patch_exec_flow() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("hello.txt"), "hi\n").unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": { "demo": { "root": &ws, "profile": "workspace-write" } }
        })
        .to_string(),
    )
    .unwrap();
    let db = root.path().join("ops.sqlite");
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let helper = codespace_server::patch_helper::ensure_helper_for_tests();
    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(bin).configure(|cmd| {
                cmd.env("CODESPACE_CONFIG", &cfg)
                    .env("CODESPACE_OPERATIONS_DB", &db)
                    .env("CODESPACE_PATCH_BIN", &helper);
            }))
            .expect("spawn"),
        )
        .await
        .expect("init");

    let info = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("info");
    let info_body = payload(&info);
    assert_eq!(info_body["internal_model_calls"], false);
    assert_eq!(info_body["workspace_id_is_credential"], false);
    assert_eq!(info_body["execution"]["environment"]["kind"], "host");
    assert_eq!(info_body["execution"]["permissions"]["write"], true);
    assert_eq!(info_body["execution"]["process"]["available"], true);
    assert_eq!(
        info_body["execution"]["process"]["capabilities"]["tty"]["resize_supported"],
        false
    );
    assert_eq!(
        info_body["execution"]["isolation"]["command_sandbox"],
        "none"
    );
    assert_eq!(info_body["execution"]["network"]["enforcement"], "none");

    let read = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "hello.txt" })),
        )
        .await
        .expect("read");
    assert_eq!(payload(&read)["content"], "hi\n");

    let applied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: patched.txt\n+from-e2e\n*** End Patch\n",
                "operation_key": "e2e-1"
            })),
        )
        .await
        .expect("patch");
    assert_eq!(payload(&applied)["status"], "applied");
    assert_eq!(
        std::fs::read_to_string(ws.join("patched.txt")).unwrap(),
        "from-e2e\n"
    );

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "e2e-exec"]
            })),
        )
        .await
        .expect("exec");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(pid.starts_with("proc-"));
    assert_eq!(payload(&started)["dispatch_status"], "confirmed");

    let mut chunk = String::new();
    for _ in 0..50 {
        let out = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read process");
        chunk = payload(&out)["chunk"].as_str().unwrap_or("").to_string();
        if chunk.contains("e2e-exec") {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(chunk.contains("e2e-exec"), "{chunk:?}");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn live_shell_blocks_patch() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": { "demo": { "root": &ws, "profile": "workspace-write" } }
        })
        .to_string(),
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let helper = codespace_server::patch_helper::ensure_helper_for_tests();
    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(bin).configure(|cmd| {
                cmd.env("CODESPACE_CONFIG", &cfg)
                    .env("CODESPACE_PATCH_BIN", &helper);
            }))
            .expect("spawn"),
        )
        .await
        .expect("init");

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sleep", "20"]
            })),
        )
        .await
        .expect("sleep");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    let busy = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: x.txt\n+nope\n*** End Patch\n",
                "operation_key": "e2e-busy"
            })),
        )
        .await;
    let text = err_text(&busy);
    assert!(
        text.contains("WORKSPACE_BUSY") || text.contains("busy"),
        "{text}"
    );

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await
        .expect("terminate");
    client.cancel().await.expect("cancel");
}
