use codespace_domain::{LIVE_TOOLS, TOOL_WORKSPACE_INFO};
use codespace_server::config::{HttpConfig, MCP_PATH};
use codespace_server::http::router;
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    },
    transport::{StreamableHttpClientTransport, TokioChildProcess},
    ClientLifecycleMode, ClientServiceExt, ServiceExt,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::Command;

fn payload(result: &rmcp::model::CallToolResult) -> Value {
    result.structured_content.clone().unwrap_or_else(|| {
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
    })
}

#[tokio::test]
async fn stdio_and_http_share_workspace_info_contract() {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let stdio =
        ().serve(TokioChildProcess::new(Command::new(bin)).expect("spawn"))
            .await
            .expect("stdio init");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = router(HttpConfig {
        host: "127.0.0.1".into(),
        port: addr.port(),
        bearer_token: None,
    });
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let http = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("w03-contract", "0.0.0"),
    )
    .serve_with_lifecycle(
        StreamableHttpClientTransport::from_uri(format!("http://{addr}{MCP_PATH}")),
        // Prefers 2026-07-28 and may fall back to 2025-11-25. This is not
        // 2025-11-25-only coverage; see protocol_compat.rs.
        ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        },
    )
    .await
    .expect("http init");

    let stdio_tools = stdio.list_all_tools().await.expect("stdio list");
    let http_tools = http.list_all_tools().await.expect("http list");
    let mut stdio_names: Vec<&str> = stdio_tools.iter().map(|t| t.name.as_ref()).collect();
    let mut http_names: Vec<&str> = http_tools.iter().map(|t| t.name.as_ref()).collect();
    stdio_names.sort();
    http_names.sort();
    let mut expected = LIVE_TOOLS.to_vec();
    expected.sort();
    assert_eq!(stdio_names, expected);
    assert_eq!(stdio_names, http_names);
    let stdio_info = stdio_tools
        .iter()
        .find(|t| t.name == TOOL_WORKSPACE_INFO)
        .expect("stdio workspace_info");
    let http_info = http_tools
        .iter()
        .find(|t| t.name == TOOL_WORKSPACE_INFO)
        .expect("http workspace_info");
    assert_eq!(stdio_info.input_schema, http_info.input_schema);

    let stdio_body = payload(
        &stdio
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
            .await
            .expect("stdio call"),
    );
    let http_body = payload(
        &http
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO))
            .await
            .expect("http call"),
    );
    assert_eq!(stdio_body["workspace_id"], serde_json::Value::Null);
    assert_eq!(stdio_body["internal_model_calls"], false);
    assert_eq!(stdio_body["workspace_id"], http_body["workspace_id"]);
    assert_eq!(
        stdio_body["workspace_id_is_credential"],
        http_body["workspace_id_is_credential"]
    );
    assert_eq!(stdio_body["tools_exposed"], http_body["tools_exposed"]);
    assert_eq!(stdio_body["transports"], http_body["transports"]);

    stdio.cancel().await.expect("cancel stdio");
    http.cancel().await.expect("cancel http");
}
