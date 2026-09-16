use codespace_domain::{TOOL_APPLY_PATCH, TOOL_OPERATION_STATUS};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;

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
async fn operation_key_replays_and_conflicts_without_rewriting() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("keep.txt"), "keep").unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": {
                "demo": { "root": ws, "profile": "workspace-write" }
            }
        })
        .to_string(),
    )
    .unwrap();
    let db = root.path().join("ops.sqlite");

    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(bin).configure(|cmd| {
                cmd.env("CODESPACE_CONFIG", &cfg)
                    .env("CODESPACE_OPERATIONS_DB", &db)
                    .env(
                        "CODESPACE_PATCH_BIN",
                        codespace_server::patch_helper::ensure_helper_for_tests(),
                    );
            }))
            .expect("spawn"),
        )
        .await
        .expect("init");

    let patch = "*** Begin Patch\n*** Add File: new.txt\n+hello\n*** End Patch\n";
    let first = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "k-1"
            })),
        )
        .await
        .expect("first apply");
    let body = payload(&first);
    assert_eq!(body["replayed"], false);
    assert_eq!(body["status"], "applied");
    let op_id = body["operation_id"].as_str().unwrap().to_string();
    assert!(op_id.starts_with("op-"));
    assert_ne!(op_id, "1");
    let keep = std::fs::read_to_string(root.path().join("ws").join("keep.txt")).unwrap();
    assert_eq!(keep, "keep");
    let created = std::fs::read_to_string(root.path().join("ws").join("new.txt")).unwrap();
    assert_eq!(created, "hello\n");

    let replay = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "k-1"
            })),
        )
        .await
        .expect("replay");
    let replay_body = payload(&replay);
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["operation_id"], op_id);

    let conflict = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: other.txt\n+x\n*** End Patch\n",
                "operation_key": "k-1"
            })),
        )
        .await;
    let conflict_text = err_text(&conflict);
    assert!(
        conflict_text.contains("OPERATION_KEY_CONFLICT") || conflict_text.contains("reused"),
        "{conflict_text}"
    );

    let status = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": op_id })),
        )
        .await
        .expect("status");
    let status_body = payload(&status);
    assert_eq!(status_body["status"], "applied");
    assert_eq!(status_body["replayed"], false);

    let missing = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": "op-invented" })),
        )
        .await;
    let missing_text = err_text(&missing);
    assert!(
        missing_text.contains("OPERATION_NOT_FOUND") || missing_text.contains("unknown"),
        "{missing_text}"
    );

    let unknown_ws = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "nope",
                "patch": patch,
                "operation_key": "k-2"
            })),
        )
        .await;
    let unknown_text = err_text(&unknown_ws);
    assert!(
        unknown_text.contains("WORKSPACE_NOT_FOUND") || unknown_text.contains("unknown workspace"),
        "{unknown_text}"
    );

    client.cancel().await.expect("cancel");
}
