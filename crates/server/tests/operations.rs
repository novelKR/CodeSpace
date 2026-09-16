use codespace_domain::{TOOL_APPLY_PATCH, TOOL_OPERATION_STATUS, TOOL_WORKSPACE_INFO};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;

async fn spawn(
    config: &std::path::Path,
    store: Option<&std::path::Path>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("CODESPACE_CONFIG", config);
            if let Some(store) = store {
                cmd.env("CODESPACE_STORE", store);
            }
        }))
        .expect("spawn"),
    )
    .await
    .expect("init")
}

#[tokio::test]
async fn apply_patch_replays_same_key_and_rejects_reuse() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
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
    let store = root.path().join("ops.sqlite");

    let client = spawn(&cfg, Some(&store)).await;

    let first = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\n",
                "operation_key": "k1"
            })),
        )
        .await
        .expect("first apply");
    let first_body = first.structured_content.clone().expect("structured");
    assert_eq!(first_body["replayed"], false);
    assert_eq!(first_body["status"], "applied");
    let op_id = first_body["operation_id"].as_str().unwrap().to_string();
    assert!(op_id.starts_with("op-"));

    let replay = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\n",
                "operation_key": "k1"
            })),
        )
        .await
        .expect("replay");
    let replay_body = replay.structured_content.clone().expect("structured");
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["operation_id"], op_id);

    let status = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": op_id })),
        )
        .await
        .expect("status");
    let status_body = status.structured_content.clone().expect("structured");
    assert_eq!(status_body["status"], "applied");

    let conflict = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: b.txt\n+other\n*** End Patch\n",
                "operation_key": "k1"
            })),
        )
        .await;
    let conflict_text = format!("{conflict:?}");
    assert!(
        conflict_text.contains("OPERATION_KEY_CONFLICT") || conflict_text.contains("reused"),
        "{conflict_text}"
    );

    let missing = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": "op-missing" })),
        )
        .await;
    let missing_text = format!("{missing:?}");
    assert!(
        missing_text.contains("OPERATION_NOT_FOUND") || missing_text.contains("unknown operation"),
        "{missing_text}"
    );

    let unknown_ws = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "nope",
                "patch": "x",
                "operation_key": "k2"
            })),
        )
        .await;
    let unknown_text = format!("{unknown_ws:?}");
    assert!(
        unknown_text.contains("WORKSPACE_NOT_FOUND") || unknown_text.contains("unknown workspace"),
        "{unknown_text}"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn apply_patch_rejected_on_read_only_profile() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": {
                "demo": { "root": ws, "profile": "read-only" }
            }
        })
        .to_string(),
    )
    .unwrap();

    let client = spawn(&cfg, None).await;
    let denied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "x",
                "operation_key": "k1"
            })),
        )
        .await;
    let text = format!("{denied:?}");
    assert!(
        text.contains("UNAUTHORIZED") || text.contains("read-only"),
        "{text}"
    );

    let info = client
        .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
        .await
        .expect("info");
    let tools = info.structured_content.unwrap()["tools_exposed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(tools.contains(&TOOL_APPLY_PATCH.to_string()));
    assert!(tools.contains(&TOOL_OPERATION_STATUS.to_string()));

    client.cancel().await.expect("cancel");
}
