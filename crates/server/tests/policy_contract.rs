use codespace_domain::TOOL_WORKSPACE_INFO;
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;

#[tokio::test]
async fn unknown_workspace_is_rejected_known_is_selector() {
    let root = tempfile::tempdir().unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": {
                "demo": { "root": root.path().join("ws").to_string_lossy(), "profile": "read-only" }
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::create_dir(root.path().join("ws")).unwrap();

    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(bin).configure(|cmd| {
                cmd.env("CODESPACE_CONFIG", &cfg);
            }))
            .expect("spawn"),
        )
        .await
        .expect("init");

    let unknown = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "nope" })),
        )
        .await;
    let unknown_text = match &unknown {
        Ok(result) => result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join(""),
        Err(err) => err.to_string(),
    };
    assert!(
        unknown_text.contains("WORKSPACE_NOT_FOUND") || unknown_text.contains("unknown workspace"),
        "unexpected unknown workspace result: {unknown:?}"
    );

    let known = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo", "approved": true })),
        )
        .await
        .expect("known");
    let body = known.structured_content.clone().expect("structured");
    assert_eq!(body["workspace_id"], "demo");
    assert_eq!(body["workspace_id_is_credential"], false);
    assert_eq!(body["profile"], "read-only");

    client.cancel().await.expect("cancel");
}
