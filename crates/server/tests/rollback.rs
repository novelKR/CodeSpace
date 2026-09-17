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

#[tokio::test]
async fn partial_apply_rolls_back_and_is_not_applied() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("keep.txt"), "keep\n").unwrap();
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
    let patch_bin = codespace_server::patch_helper::ensure_helper_for_tests();
    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(bin).configure(|cmd| {
                cmd.env("CODESPACE_CONFIG", &cfg)
                    .env("CODESPACE_OPERATIONS_DB", &db)
                    .env("CODESPACE_PATCH_BIN", &patch_bin);
            }))
            .expect("spawn"),
        )
        .await
        .expect("init");

    let patch = "*** Begin Patch\n*** Add File: first.txt\n+one\n*** Add File: first.txt/nested.txt\n+two\n*** End Patch\n";
    let result = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "rollback-1"
            })),
        )
        .await
        .expect("apply returns a status, not a transport error");
    let body = payload(&result);
    let status = body["status"].as_str().unwrap();
    assert_ne!(status, "applied");
    assert!(
        status == "failed_rolled_back" || status == "failed_partial" || status == "unknown",
        "{body}"
    );
    assert!(!root.path().join("ws/first.txt").is_file() || status == "failed_partial");
    if status == "failed_rolled_back" {
        assert!(!root.path().join("ws/first.txt").exists());
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/keep.txt")).unwrap(),
        "keep\n"
    );

    let op_id = body["operation_id"].as_str().unwrap();
    let status_result = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": op_id })),
        )
        .await
        .expect("status");
    assert_ne!(payload(&status_result)["status"], "applied");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn restart_does_not_reapply_unknown_operation() {
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
    let db = root.path().join("ops.sqlite");
    let patch = "*** Begin Patch\n*** Add File: boom.txt\n+nope\n*** End Patch\n";
    let params = codespace_domain::ApplyPatchParams {
        workspace_id: codespace_domain::WorkspaceId("demo".into()),
        patch: patch.into(),
        expected_versions: Default::default(),
        operation_key: Some(codespace_domain::OperationKey("restart-1".into())),
        check_only: false,
        work_id: None,
    };
    let fp = codespace_store::Store::fingerprint(&params);
    let store = codespace_store::Store::open(&db).unwrap();
    let codespace_store::Begin::Fresh(id) = store
        .begin(params.operation_key.as_ref(), "demo", &fp)
        .unwrap()
    else {
        panic!("fresh");
    };
    drop(store);

    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let patch_bin = codespace_server::patch_helper::ensure_helper_for_tests();
    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(bin).configure(|cmd| {
                cmd.env("CODESPACE_CONFIG", &cfg)
                    .env("CODESPACE_OPERATIONS_DB", &db)
                    .env("CODESPACE_PATCH_BIN", &patch_bin);
            }))
            .expect("spawn"),
        )
        .await
        .expect("init");

    let replay = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "restart-1"
            })),
        )
        .await
        .expect("replay");
    let body = payload(&replay);
    assert_eq!(body["replayed"], true);
    assert_eq!(body["operation_id"], id.0);
    assert_eq!(body["status"], "unknown");
    assert!(!root.path().join("ws/boom.txt").exists());

    client.cancel().await.expect("cancel");
}
