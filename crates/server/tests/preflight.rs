use codespace_domain::{TOOL_APPLY_PATCH, TOOL_READ};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;

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
async fn preflight_rejects_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(ws.join("b.txt"), "beta\n").unwrap();
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

    let mismatch = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "check_only": true,
                "operation_key": "preflight-1",
                "patch": "*** Begin Patch\n*** Update File: a.txt\n@@\n-alpha\n+ALPHA\n*** Update File: b.txt\n@@\n-missing\n+BETA\n*** End Patch\n"
            })),
        )
        .await;
    let text = err_text(&mismatch);
    assert!(
        text.contains("INVALID_PATCH") || text.contains("context"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/b.txt")).unwrap(),
        "beta\n"
    );

    let first = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "a.txt" })),
        )
        .await
        .expect("read");
    let version = first.structured_content.unwrap()["version"]
        .as_str()
        .unwrap()
        .to_string();

    let conflict = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "check_only": true,
                "operation_key": "preflight-2",
                "expected_versions": { "a.txt": "sha256:deadbeef" },
                "patch": "*** Begin Patch\n*** Update File: a.txt\n@@\n-alpha\n+ALPHA\n*** End Patch\n"
            })),
        )
        .await;
    let conflict_text = err_text(&conflict);
    assert!(
        conflict_text.contains("VERSION_CONFLICT") || conflict_text.contains("version"),
        "{conflict_text}"
    );

    let exists = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "check_only": true,
                "operation_key": "preflight-3",
                "expected_versions": { "a.txt": version },
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+nope\n*** End Patch\n"
            })),
        )
        .await;
    let exists_text = err_text(&exists);
    assert!(
        exists_text.contains("ADD_FILE_EXISTS") || exists_text.contains("already exists"),
        "{exists_text}"
    );

    let escape = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "check_only": true,
                "operation_key": "preflight-4",
                "patch": "*** Begin Patch\n*** Add File: ../escape.txt\n+nope\n*** End Patch\n"
            })),
        )
        .await;
    let escape_text = err_text(&escape);
    assert!(
        escape_text.contains("PATH_ESCAPE") || escape_text.contains("escape"),
        "{escape_text}"
    );

    std::os::unix::fs::symlink("/etc/passwd", root.path().join("ws/link")).unwrap();
    let link = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "check_only": true,
                "operation_key": "preflight-5",
                "patch": "*** Begin Patch\n*** Update File: link\n@@\n-root\n+pwned\n*** End Patch\n"
            })),
        )
        .await;
    let link_text = err_text(&link);
    assert!(
        link_text.contains("SYMLINK_REJECTED")
            || link_text.contains("symlink")
            || link_text.contains("INVALID_PATCH"),
        "{link_text}"
    );

    assert_eq!(
        std::fs::read_to_string(root.path().join("ws/a.txt")).unwrap(),
        "alpha\n"
    );

    client.cancel().await.expect("cancel");
}
