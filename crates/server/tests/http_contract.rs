use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{header, StatusCode};
use codespace_domain::{LIVE_TOOLS, TOOL_WORKSPACE_INFO};
use codespace_server::config::{HttpConfig, MCP_PATH};
use codespace_server::http::router;
use rmcp::{
    model::{ClientCapabilities, ClientConfig, Implementation, ProtocolVersion},
    transport::StreamableHttpClientTransport,
    ClientLifecycleMode, ClientServiceExt,
};
use tokio::net::TcpListener;

async fn spawn_http(config: HttpConfig) -> SocketAddr {
    let bind = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&bind).await.expect("bind http test");
    let addr = listener.local_addr().expect("local addr");
    let mut config = config;
    config.port = addr.port();
    let app = router(config);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("http serve");
    });
    addr
}

#[tokio::test]
async fn http_tools_list_matches_stdio_contract() {
    let addr = spawn_http(HttpConfig {
        host: "127.0.0.1".into(),
        port: 0,
        bearer_token: None,
    })
    .await;
    let uri = format!("http://{addr}{MCP_PATH}");
    let transport = StreamableHttpClientTransport::from_uri(uri);
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("codespace-contract", "0.0.0"),
    )
    .serve_with_lifecycle(
        transport,
        // Prefers 2026-07-28 and may fall back to 2025-11-25. This is not
        // 2025-11-25-only coverage; see protocol_compat.rs.
        ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        },
    )
    .await
    .expect("http initialize");

    let tools = client
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    let mut names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort();
    let mut expected = LIVE_TOOLS.to_vec();
    expected.sort();
    assert_eq!(names, expected);

    let result = client
        .call_tool(rmcp::model::CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
        .await
        .expect("tools/call");
    let body = result.structured_content.clone().unwrap_or_else(|| {
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
    });
    assert_eq!(body["workspace_id"], serde_json::Value::Null);
    assert_eq!(body["internal_model_calls"], false);

    client.cancel().await.expect("cancel http client");
}

#[tokio::test]
async fn http_bearer_rejects_at_transport_and_does_not_echo_token() {
    let token = "test-token-do-not-log";
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
        .expect("unauthorized post");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.text().await.expect("body");
    assert!(!body.contains(token));
    assert!(body.contains("unauthorized"));
}
