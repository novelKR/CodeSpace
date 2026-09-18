//! Adversarial cases for path policy, handles, tokens, and version/context.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{header, StatusCode};
use codespace_domain::{
    TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND, TOOL_READ, TOOL_READ_PROCESS, TOOL_TERMINATE_PROCESS,
};
use codespace_server::config::{HttpConfig, MCP_PATH};
use codespace_server::http::router;
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::net::TcpListener;
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

fn write_ws(profile: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("keep.txt"), "keep\n").unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": { "demo": { "root": ws, "profile": profile } }
        })
        .to_string(),
    )
    .unwrap();
    let ws_out = root.path().join("ws");
    (root, cfg, ws_out)
}

async fn spawn_client(
    cfg: &std::path::Path,
    db: Option<&std::path::Path>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let helper = codespace_server::patch_helper::ensure_helper_for_tests();
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("CODESPACE_CONFIG", cfg)
                .env("CODESPACE_PATCH_BIN", &helper);
            if let Some(db) = db {
                cmd.env("CODESPACE_OPERATIONS_DB", db);
            }
        }))
        .expect("spawn"),
    )
    .await
    .expect("init")
}

#[tokio::test]
async fn file_tools_reject_escape_absolute_symlink_and_fifo() {
    let (root, cfg, ws) = write_ws("workspace-write");
    std::os::unix::fs::symlink("/etc/passwd", ws.join("link")).unwrap();
    let fifo = ws.join("pipe.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo should exist in CI");

    let client = spawn_client(&cfg, None).await;

    let dotdot = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "../keep.txt" })),
        )
        .await;
    let text = err_text(&dotdot);
    assert!(
        text.contains("PATH_ESCAPE") || text.contains("escape"),
        "{text}"
    );

    let abs = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "/etc/passwd" })),
        )
        .await;
    let text = err_text(&abs);
    assert!(
        text.contains("PATH_ESCAPE") || text.contains("escape"),
        "{text}"
    );

    let link = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "link" })),
        )
        .await;
    let text = err_text(&link);
    assert!(
        text.contains("SYMLINK_REJECTED") || text.contains("symlink"),
        "{text}"
    );

    let special = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "pipe.fifo" })),
        )
        .await;
    let text = err_text(&special);
    assert!(text.contains("SPECIAL_FILE_REJECTED"), "{text}");

    let _ = root;
    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn invented_handles_and_client_claims_are_rejected() {
    let (_root, cfg, _ws) = write_ws("read-only");
    let client = spawn_client(&cfg, None).await;

    let exec = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "nope"],
                "approved": true
            })),
        )
        .await;
    let text = err_text(&exec);
    assert!(
        text.contains("UNAUTHORIZED") || text.contains("read-only"),
        "{text}"
    );

    let missing = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ_PROCESS)
                .with_arguments(object!({ "process_id": "proc-invented" })),
        )
        .await;
    let text = err_text(&missing);
    assert!(
        text.contains("PROCESS_NOT_FOUND") || text.contains("unknown process"),
        "{text}"
    );

    let kill = client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": "proc-invented" })),
        )
        .await;
    let text = err_text(&kill);
    assert!(
        text.contains("PROCESS_NOT_FOUND") || text.contains("unknown process"),
        "{text}"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn version_conflict_and_context_mismatch_are_not_applied() {
    let (root, cfg, ws) = write_ws("workspace-write");
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, Some(&db)).await;

    let read = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "keep.txt" })),
        )
        .await
        .expect("read");
    let version = payload(&read)["version"].as_str().unwrap().to_string();
    std::fs::write(ws.join("keep.txt"), "externally-edited\n").unwrap();

    let conflict = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Update File: keep.txt\n@@\n-keep\n+new\n*** End Patch\n",
                "expected_versions": { "keep.txt": version },
                "operation_key": "ver-1"
            })),
        )
        .await;
    let text = err_text(&conflict);
    assert!(
        text.contains("VERSION_CONFLICT") || text.contains("version"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("keep.txt")).unwrap(),
        "externally-edited\n"
    );

    std::fs::write(ws.join("keep.txt"), "keep\n").unwrap();
    let mismatch = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Update File: keep.txt\n@@\n-this-context-is-wrong\n+new\n*** End Patch\n",
                "operation_key": "ctx-1"
            })),
        )
        .await;
    match &mismatch {
        Ok(result) => {
            let body = payload(result);
            let status = body["status"].as_str().unwrap_or("");
            assert_ne!(status, "applied", "{body}");
        }
        Err(err) => {
            let text = err.to_string();
            assert!(
                text.contains("INVALID_PATCH")
                    || text.contains("context")
                    || text.contains("rejected")
                    || text.contains("Failed to find expected lines"),
                "{text}"
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string(ws.join("keep.txt")).unwrap(),
        "keep\n"
    );

    client.cancel().await.expect("cancel");
}

async fn spawn_http(config: HttpConfig) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut config = config;
    config.port = addr.port();
    let app = router(config);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

#[tokio::test]
async fn bearer_errors_do_not_echo_token() {
    let token = "adversarial-secret-token";
    let addr = spawn_http(HttpConfig {
        host: "127.0.0.1".into(),
        port: 0,
        bearer_token: Some(token.into()),
    })
    .await;
    let url = format!("http://{addr}{MCP_PATH}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.text().await.expect("body");
    assert!(!body.contains(token));
    assert!(body.contains("unauthorized"));
}
