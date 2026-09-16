//! Forced MCP protocol-version contracts.
//!
//! Auto { 2026-07-28 preferred, 2025-11-25 legacy } tests in `http_contract.rs`
//! and `transport_contract.rs` do **not** replace 2025-11-25-only coverage.

use std::net::SocketAddr;

use codespace_domain::{
    LIVE_TOOLS, SERVER_NAME, TOOL_WORKSPACE_INFO, TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP,
};
use codespace_server::config::{HttpConfig, MCP_PATH};
use codespace_server::http::router;
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    },
    transport::{StreamableHttpClientTransport, TokioChildProcess},
    ClientLifecycleMode, ClientServiceExt,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::Command;

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
