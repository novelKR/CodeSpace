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
    assert_eq!(preview_body["status"], "checked");
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
    let version = read_body["version"].as_str().unwrap();
    assert!(version.starts_with("sha256:"));
    let change = body["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "created.txt")
        .expect("created.txt change");
    assert_eq!(change["kind"], "add");
    assert_eq!(change["after_version"], version);
    assert!(change["before_version"].is_null());

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn concurrent_apply_patch_serializes_without_busy() {
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

    let first = client.call_tool(CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(
        object!({
            "workspace_id": "demo",
            "patch": "*** Begin Patch\n*** Add File: a.txt\n+one\n*** End Patch\n",
            "operation_key": "sched-a"
        }),
    ));
    let second = client.call_tool(CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(
        object!({
            "workspace_id": "demo",
            "patch": "*** Begin Patch\n*** Add File: b.txt\n+two\n*** End Patch\n",
            "operation_key": "sched-b"
        }),
    ));
    let (first, second) = tokio::join!(first, second);
    let first_body = payload(&first.expect("first patch"));
    let second_body = payload(&second.expect("second patch"));
    assert_eq!(first_body["status"], "applied");
    assert_eq!(second_body["status"], "applied");
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/a.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/b.txt")).unwrap(),
        "two\n"
    );

    client.cancel().await.expect("cancel");
}
