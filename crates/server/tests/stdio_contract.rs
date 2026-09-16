use codespace_domain::{
    SERVER_NAME, TOOL_WORKSPACE_INFO, TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP,
};
use rmcp::{model::CallToolRequestParams, object, transport::TokioChildProcess, ServiceExt};
use serde_json::Value;
use tokio::process::Command;

fn payload(result: &rmcp::model::CallToolResult) -> Value {
    result.structured_content.clone().unwrap_or_else(|| {
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
    })
}

#[tokio::test]
async fn stdio_lists_and_calls_workspace_info() {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let client =
        ().serve(TokioChildProcess::new(Command::new(bin)).expect("spawn stdio mcp"))
            .await
            .expect("initialize stdio mcp");

    let tools = client.list_all_tools().await.expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, vec![TOOL_WORKSPACE_INFO]);

    let result = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("tools/call workspace_info");

    let body = payload(&result);
    assert_eq!(body["server"], SERVER_NAME);
    assert_eq!(body["internal_model_calls"], false);
    assert_eq!(body["workspace_id_is_credential"], false);
    assert_eq!(body["workspace_id"], "demo");
    assert_eq!(
        body["tools_exposed"],
        serde_json::json!([TOOL_WORKSPACE_INFO])
    );
    assert_eq!(
        body["transports"],
        serde_json::json!([TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP])
    );

    client.cancel().await.expect("cancel client");
}
