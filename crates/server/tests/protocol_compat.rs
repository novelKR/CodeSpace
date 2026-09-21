//! Forced MCP protocol-version contracts.
//!
//! Auto { 2026-07-28 preferred, 2025-11-25 legacy } tests in `http_contract.rs`
//! and `transport_contract.rs` do **not** replace 2025-11-25-only coverage.

use std::net::SocketAddr;
use std::sync::Arc;

use codespace_domain::{
    Profile, WorkspaceId, LIVE_TOOLS, SERVER_NAME, TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND,
    TOOL_OPERATION_STATUS, TOOL_PROCESS_RESIZE, TOOL_PROCESS_STATUS, TOOL_READ,
    TOOL_STEER_CLAIM_NEXT, TOOL_STEER_COMPLETE, TOOL_STEER_STATUS, TOOL_WORKSPACE_INFO,
    TOOL_WORK_FINISH, TOOL_WORK_OPEN, TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP,
};
use codespace_policy::{Registry, Workspace};
use codespace_server::config::{HttpConfig, INBOX_PATH, MCP_PATH};
use codespace_server::http::{http_router, router};
use codespace_store::Store;
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    },
    object,
    transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess},
    ClientLifecycleMode, ClientServiceExt,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

fn payload(result: &rmcp::model::CallToolResult) -> Value {
    result.structured_content.clone().unwrap_or_else(|| {
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
    })
}

fn assert_workspace_info_contract(body: &Value) {
    assert_eq!(body["server"], SERVER_NAME);
    assert_eq!(body["internal_model_calls"], false);
    assert_eq!(body["workspace_id_is_credential"], false);
    assert_eq!(body["workspace_id"], Value::Null);
    assert_eq!(body["tools_exposed"], serde_json::json!(LIVE_TOOLS));
    assert_eq!(
        body["transports"],
        serde_json::json!([TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP])
    );
    assert!(body.get("execution").is_none() || body["execution"].is_null());
}

fn assert_live_tools(names: impl IntoIterator<Item = impl AsRef<str>>) {
    let mut names: Vec<String> = names.into_iter().map(|n| n.as_ref().to_string()).collect();
    names.sort();
    let mut expected: Vec<String> = LIVE_TOOLS.iter().map(|s| (*s).to_string()).collect();
    expected.sort();
    assert!(
        names.iter().any(|n| n == TOOL_WORKSPACE_INFO),
        "tools/list must include workspace_info, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == TOOL_PROCESS_STATUS),
        "tools/list must include process_status, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == TOOL_PROCESS_RESIZE),
        "tools/list must include process_resize, got {names:?}"
    );
    assert_eq!(names, expected);
}

fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("codespace-protocol-compat", "0.0.0"),
    )
}

fn lifecycle_1125() -> ClientLifecycleMode {
    ClientLifecycleMode::Initialize
}

fn lifecycle_0728() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: None,
    }
}

fn stdio_transport() -> TokioChildProcess {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    TokioChildProcess::new(Command::new(bin)).expect("spawn stdio mcp")
}

async fn spawn_http() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http");
    let addr = listener.local_addr().expect("local addr");
    let app = router(HttpConfig {
        host: "127.0.0.1".into(),
        port: addr.port(),
        bearer_token: None,
    });
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("http serve");
    });
    addr
}

#[tokio::test]
async fn stdio_forced_2025_11_25_workspace_info() {
    let client = client_config()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve_with_lifecycle(stdio_transport(), lifecycle_1125())
        .await
        .expect("stdio initialize 2025-11-25");
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2025_11_25
    );

    let tools = client.list_all_tools().await.expect("tools/list");
    assert_live_tools(tools.iter().map(|t| t.name.as_ref()));

    let body = payload(
        &client
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
            .await
            .expect("tools/call workspace_info"),
    );
    assert_workspace_info_contract(&body);

    client.cancel().await.expect("cancel stdio");
}

#[tokio::test]
async fn http_forced_2025_11_25_workspace_info() {
    let addr = spawn_http().await;
    let client = client_config()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_uri(format!("http://{addr}{MCP_PATH}")),
            lifecycle_1125(),
        )
        .await
        .expect("http initialize 2025-11-25");
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2025_11_25
    );

    let tools = client.list_all_tools().await.expect("tools/list");
    assert_live_tools(tools.iter().map(|t| t.name.as_ref()));

    let body = payload(
        &client
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
            .await
            .expect("tools/call workspace_info"),
    );
    assert_workspace_info_contract(&body);

    client.cancel().await.expect("cancel http");
}

/// Forced 2026-07-28 uses discover with no initialize fallback. Core
/// tools/call must succeed without MRTR, Tasks, subscriptions, or
/// application-level Mcp-Name routing.
#[tokio::test]
async fn stdio_forced_2026_07_28_without_legacy_fallback() {
    let client = client_config()
        .serve_with_lifecycle(stdio_transport(), lifecycle_0728())
        .await
        .expect("stdio negotiate 2026-07-28 without legacy fallback");
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2026_07_28
    );

    let tools = client.list_all_tools().await.expect("tools/list");
    assert_live_tools(tools.iter().map(|t| t.name.as_ref()));

    let body = payload(
        &client
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
            .await
            .expect("tools/call must not need 0728-only headers"),
    );
    assert_workspace_info_contract(&body);

    client.cancel().await.expect("cancel stdio");
}

#[tokio::test]
async fn http_forced_2026_07_28_without_legacy_fallback() {
    let addr = spawn_http().await;
    let client = client_config()
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_uri(format!("http://{addr}{MCP_PATH}")),
            lifecycle_0728(),
        )
        .await
        .expect("http negotiate 2026-07-28 without legacy fallback");
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2026_07_28
    );

    let tools = client.list_all_tools().await.expect("tools/list");
    assert_live_tools(tools.iter().map(|t| t.name.as_ref()));

    let body = payload(
        &client
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
            .await
            .expect("tools/call must not need 0728-only headers"),
    );
    assert_workspace_info_contract(&body);

    client.cancel().await.expect("cancel http");
}

fn write_workspace() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
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
    (root, cfg, db)
}

fn stdio_workspace(cfg: &std::path::Path, db: &std::path::Path) -> TokioChildProcess {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let patch = codespace_server::patch_helper::ensure_helper_for_tests();
    TokioChildProcess::new(Command::new(bin).configure(|cmd| {
        cmd.env("CODESPACE_CONFIG", cfg)
            .env("CODESPACE_OPERATIONS_DB", db)
            .env("CODESPACE_PATCH_BIN", patch);
    }))
    .expect("spawn stdio mcp")
}

async fn spawn_http_workspace() -> (tempfile::TempDir, SocketAddr, Arc<Store>) {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("hello.txt"), "hi\n").unwrap();
    let mut registry = Registry::new();
    registry.insert(Workspace::new(
        WorkspaceId("demo".into()),
        ws,
        Profile::WorkspaceWrite,
    ));
    let store = Arc::new(Store::memory().expect("store"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http");
    let addr = listener.local_addr().expect("local addr");
    let app = http_router(
        &HttpConfig {
            host: "127.0.0.1".into(),
            port: addr.port(),
            bearer_token: None,
        },
        registry,
        store.clone(),
        CancellationToken::new(),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("http serve");
    });
    (root, addr, store)
}

async fn connect_http(
    addr: SocketAddr,
    protocol: ProtocolVersion,
    lifecycle: ClientLifecycleMode,
) -> rmcp::service::RunningService<rmcp::RoleClient, rmcp::model::InitializeRequestParams> {
    client_config()
        .with_protocol_version(protocol)
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_uri(format!("http://{addr}{MCP_PATH}")),
            lifecycle,
        )
        .await
        .expect("http initialize")
}

#[tokio::test]
async fn http_forced_2025_11_25_process_resize_caps() {
    let (_root, addr, _store) = spawn_http_workspace().await;
    let client = connect_http(addr, ProtocolVersion::V_2025_11_25, lifecycle_1125()).await;
    let tools = client.list_all_tools().await.expect("tools/list");
    assert_live_tools(tools.iter().map(|t| t.name.as_ref()));
    assert!(tools.iter().any(|t| t.name.as_ref() == TOOL_PROCESS_RESIZE));
    let body = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                    .with_arguments(object!({ "workspace_id": "demo" })),
            )
            .await
            .expect("workspace_info"),
    );
    let tty = &body["execution"]["process"]["capabilities"]["tty"];
    assert_eq!(tty["resize_supported"], true);
    assert_eq!(tty["initial_rows"], 24);
    assert_eq!(tty["initial_cols"], 80);
    let lifetime = &body["execution"]["process"]["capabilities"]["lifetime"];
    assert_eq!(lifetime["owner"], "runner");
    assert_eq!(lifetime["client_disconnect"], "keep_running");
    assert_eq!(lifetime["runner_disconnect"], "terminate");
    assert_eq!(lifetime["gateway_shutdown"], "terminate");
    assert_eq!(lifetime["restart_recovery"], "none");
    let files = &body["execution"]["files"]["capabilities"];
    assert_eq!(files["read_range"], true);
    assert_eq!(files["find_pagination"], true);
    assert_eq!(files["read_max_bytes"], 1048576);
    assert_eq!(files["find_max_paths"], 10000);
    client.cancel().await.expect("cancel http");
}

#[tokio::test]
async fn stdio_forced_2025_11_25_read_patch_exec() {
    let (_root, cfg, db) = write_workspace();
    let client = client_config()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve_with_lifecycle(stdio_workspace(&cfg, &db), lifecycle_1125())
        .await
        .expect("stdio initialize 2025-11-25");
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2025_11_25
    );

    let read = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ)
                    .with_arguments(object!({ "workspace_id": "demo", "path": "hello.txt" })),
            )
            .await
            .expect("read"),
    );
    assert_eq!(read["content"], "hi\n");
    assert!(read["coordination"].is_null());

    let patch = "*** Begin Patch\n*** Add File: extra.txt\n+ok\n*** End Patch\n";
    let applied = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                    "workspace_id": "demo",
                    "patch": patch,
                    "operation_key": "compat-apply"
                })),
            )
            .await
            .expect("apply"),
    );
    assert_eq!(applied["status"], "applied");
    let op_id = applied["operation_id"].as_str().unwrap();
    let status = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                    .with_arguments(object!({ "operation_id": op_id })),
            )
            .await
            .expect("operation_status"),
    );
    assert_eq!(status["status"], "applied");
    assert_eq!(status["kind"], "patch");
    assert_eq!(status["workspace_id"], "demo");
    assert_eq!(status["events"][0]["name"], "minted");
    assert_eq!(status["events"][1]["name"], "finished");
    assert_eq!(status["events"][1]["status"], "applied");
    let changes = status["changes"].as_array().expect("ledger changes");
    assert!(changes
        .iter()
        .any(|change| { change["path"] == "extra.txt" && change["after_version"].is_string() }));

    let started = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                    "workspace_id": "demo",
                    "command": ["/bin/echo", "compat"]
                })),
            )
            .await
            .expect("exec"),
    );
    assert!(started["process_id"].as_str().unwrap().starts_with("proc-"));
    client.cancel().await.expect("cancel stdio");
}

#[tokio::test]
async fn http_forced_2025_11_25_steering_checkpoint() {
    let (_root, addr, _store) = spawn_http_workspace().await;
    let client = connect_http(addr, ProtocolVersion::V_2025_11_25, lifecycle_1125()).await;
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2025_11_25
    );

    let opened = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_WORK_OPEN)
                    .with_arguments(object!({ "workspace_id": "demo", "title": "auth" })),
            )
            .await
            .expect("work_open"),
    );
    let work_id = opened["work_id"].as_str().unwrap().to_string();
    assert!(work_id.starts_with("work-"));

    let http = reqwest::Client::new();
    let created = http
        .post(format!("http://{addr}{INBOX_PATH}/works/{work_id}/intents"))
        .json(&serde_json::json!({
            "body": "README also",
            "delivery": "next_checkpoint"
        }))
        .send()
        .await
        .expect("create draft")
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(created["state"], "draft");
    let intent_id = created["intent_id"].as_str().unwrap();
    let revision = created["revision"].as_u64().unwrap();
    let queued = http
        .post(format!(
            "http://{addr}{INBOX_PATH}/intents/{intent_id}/queue"
        ))
        .json(&serde_json::json!({ "revision": revision }))
        .send()
        .await
        .expect("queue")
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(queued["state"], "queued");

    let status = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_STEER_STATUS)
                    .with_arguments(object!({ "work_id": work_id })),
            )
            .await
            .expect("steer_status"),
    );
    assert_eq!(status["queued"], 1);
    assert_eq!(status["claimable_now"], 1);
    assert!(status.get("content").is_none());
    assert!(status.get("body").is_none());

    let claimed = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_STEER_CLAIM_NEXT)
                    .with_arguments(object!({ "work_id": work_id })),
            )
            .await
            .expect("claim"),
    );
    assert_eq!(claimed["item"]["content"], "README also");
    let item_id = claimed["item"]["intent_id"].as_str().unwrap();

    let pending = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_WORK_FINISH)
                    .with_arguments(object!({ "work_id": work_id })),
            )
            .await
            .expect("finish pending"),
    );
    assert_eq!(pending["closed"], false);
    assert_eq!(pending["reason"], "pending_user_input");

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_STEER_COMPLETE)
                .with_arguments(object!({ "work_id": work_id, "intent_id": item_id })),
        )
        .await
        .expect("complete");
    let closed = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_WORK_FINISH)
                    .with_arguments(object!({ "work_id": work_id })),
            )
            .await
            .expect("finish closed"),
    );
    assert_eq!(closed["closed"], true);
    client.cancel().await.expect("cancel http");
}

#[tokio::test]
async fn http_forced_2026_07_28_steering_is_ordinary_tools_call() {
    let (_root, addr, _store) = spawn_http_workspace().await;
    let client = connect_http(addr, ProtocolVersion::V_2026_07_28, lifecycle_0728()).await;
    assert_eq!(
        client.peer_info().expect("peer info").protocol_version,
        ProtocolVersion::V_2026_07_28
    );
    let tools = client.list_all_tools().await.expect("tools/list");
    assert_live_tools(tools.iter().map(|t| t.name.as_ref()));
    let opened = payload(
        &client
            .call_tool(
                CallToolRequestParams::new(TOOL_WORK_OPEN)
                    .with_arguments(object!({ "workspace_id": "demo" })),
            )
            .await
            .expect("work_open on 0728 still tools/call"),
    );
    assert!(opened["work_id"].as_str().unwrap().starts_with("work-"));
    client.cancel().await.expect("cancel http");
}
