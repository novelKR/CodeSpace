use codespace_domain::TOOL_WORKSPACE_INFO;
use codespace_server::config::{HttpConfig, MCP_PATH};
use codespace_server::http::router;
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    },
    object,
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
        ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        },
    )
    .await
    .expect("http init");

    let stdio_tools = stdio.list_all_tools().await.expect("stdio list");
    let http_tools = http.list_all_tools().await.expect("http list");
    let stdio_names: Vec<&str> = stdio_tools.iter().map(|t| t.name.as_ref()).collect();
    let http_names: Vec<&str> = http_tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(stdio_names, vec![TOOL_WORKSPACE_INFO]);
    assert_eq!(stdio_names, http_names);
    assert_eq!(stdio_tools[0].input_schema, http_tools[0].input_schema);

    let args = object!({ "workspace_id": "demo" });
    let stdio_body = payload(
        &stdio
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO).with_arguments(args.clone()))
            .await
            .expect("stdio call"),
    );
    let http_body = payload(
        &http
            .call_tool(CallToolRequestParams::new(TOOL_WORKSPACE_INFO).with_arguments(args))
            .await
            .expect("http call"),
    );
    assert_eq!(stdio_body["workspace_id"], "demo");
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
