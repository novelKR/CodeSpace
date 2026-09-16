use codespace_domain::{TOOL_APPLY_PATCH, TOOL_READ};
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
async fn apply_writes_and_check_only_does_not() {
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

    let patch = "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** End Patch\n";
    let preview = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "check_only": true,
                "operation_key": "apply-preview"
            })),
        )
        .await
        .expect("check_only");
    let preview_body = payload(&preview);
    assert_ne!(preview_body["status"], "applied");
    assert!(!root.path().join("ws/created.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/keep.txt")).unwrap(),
        "keep\n"
    );

    let applied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": patch,
                "operation_key": "apply-real"
            })),
        )
        .await
        .expect("apply");
    let body = payload(&applied);
    assert_eq!(body["status"], "applied");
    assert_eq!(body["replayed"], false);
    let files = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(files.iter().any(|f| f.ends_with("created.txt")));
    let on_disk = std::fs::read_to_string(root.path().join("ws/created.txt")).unwrap();
    assert_eq!(on_disk, "hello\n");
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/keep.txt")).unwrap(),
        "keep\n"
    );

    let read = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "created.txt" })),
        )
        .await
        .expect("read");
    let read_body = payload(&read);
    assert_eq!(read_body["content"], "hello\n");
    assert!(read_body["version"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    client.cancel().await.expect("cancel");
}
