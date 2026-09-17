//! Restart, replay, and partial-apply honesty.

use codespace_domain::{
    ApplyPatchParams, OperationKey, WorkspaceId, TOOL_APPLY_PATCH, TOOL_OPERATION_STATUS,
};
use codespace_store::{Begin, Store};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use std::collections::BTreeMap;
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

async fn spawn_client(
    cfg: &std::path::Path,
    db: &std::path::Path,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let helper = codespace_server::patch_helper::ensure_helper_for_tests();
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("CODESPACE_CONFIG", cfg)
                .env("CODESPACE_OPERATIONS_DB", db)
                .env("CODESPACE_PATCH_BIN", &helper);
        }))
        .expect("spawn"),
    )
    .await
    .expect("init")
}

#[tokio::test]
async fn restart_replays_instead_of_reapplying_and_unknown_stays_inert() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("keep.txt"), "keep\n").unwrap();
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

    let client = spawn_client(&cfg, &db).await;
    let patch = "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** End Patch\n";
    let first = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "restart-1"
            })),
        )
        .await
        .expect("apply");
    let body = payload(&first);
    assert_eq!(body["status"], "applied");
    let op_id = body["operation_id"].as_str().unwrap().to_string();
    client.cancel().await.expect("stop first process");

    let client = spawn_client(&cfg, &db).await;
    let status = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": op_id })),
        )
        .await
        .expect("status after restart");
    assert_eq!(payload(&status)["status"], "applied");

    let replay = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "restart-1"
            })),
        )
        .await
        .expect("replay after restart");
    let replay_body = payload(&replay);
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["operation_id"], op_id);
    assert_eq!(
        std::fs::read_to_string(ws.join("created.txt")).unwrap(),
        "hello\n"
    );

    let conflict = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: other.txt\n+x\n*** End Patch\n",
                "operation_key": "restart-1"
            })),
        )
        .await;
    let text = err_text(&conflict);
    assert!(
        text.contains("OPERATION_KEY_CONFLICT") || text.contains("reused"),
        "{text}"
    );
    assert!(!ws.join("other.txt").exists());
    client.cancel().await.expect("cancel");

    let unfinished = ApplyPatchParams {
        workspace_id: WorkspaceId("demo".into()),
        patch: "pending".into(),
        expected_versions: BTreeMap::new(),
        operation_key: Some(OperationKey("unfinished-1".into())),
        check_only: false,
    };
    let store = Store::open(&db).unwrap();
    let Begin::Fresh(pending_id) = store
        .begin(
            unfinished.operation_key.as_ref(),
            "demo",
            &Store::fingerprint(&unfinished),
        )
        .unwrap()
    else {
        panic!("expected a fresh unfinished row");
    };
    drop(store);

    let client = spawn_client(&cfg, &db).await;
    let unknown = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": pending_id.0 })),
        )
        .await
        .expect("unknown after restart");
    assert_eq!(payload(&unknown)["status"], "unknown");
    assert!(!ws.join("pending.txt").exists());
    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn partial_failure_is_never_applied() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("keep.txt"), "keep\n").unwrap();
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
    let client = spawn_client(&cfg, &db).await;
    let patch = "*** Begin Patch\n*** Add File: first.txt\n+one\n*** Add File: first.txt/nested.txt\n+two\n*** End Patch\n";
    let result = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "partial-1"
            })),
        )
        .await
        .expect("apply returns a status");
    let body = payload(&result);
    let status = body["status"].as_str().unwrap();
    assert_ne!(status, "applied");
    assert!(
        status == "failed_rolled_back" || status == "failed_partial" || status == "unknown",
        "{body}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("keep.txt")).unwrap(),
        "keep\n"
    );
    client.cancel().await.expect("cancel");
}
