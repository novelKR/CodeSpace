use codespace_domain::{TOOL_FIND, TOOL_READ, TOOL_WORKSPACE_INFO};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;

#[tokio::test]
async fn read_and_find_use_versions_and_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("hello.txt"), "hi").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", ws.join("link")).unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": {
                "demo": { "root": ws, "profile": "read-only" }
            }
        })
        .to_string(),
    )
    .unwrap();

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

    let tools = client.list_all_tools().await.expect("list");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&TOOL_WORKSPACE_INFO));
    assert!(names.contains(&TOOL_READ));
    assert!(names.contains(&TOOL_FIND));

    let first = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "hello.txt" })),
        )
        .await
        .expect("read");
    let body = first.structured_content.clone().expect("structured");
    assert_eq!(body["content"], "hi");
    assert_eq!(body["path"], "hello.txt");
    assert_eq!(body["truncated"], false);
    let version = body["version"].as_str().unwrap().to_string();
    assert!(version.starts_with("sha256:"));

    let second = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "hello.txt" })),
        )
        .await
        .expect("reread");
    assert_eq!(second.structured_content.unwrap()["version"], version);

    let found = client
        .call_tool(
            CallToolRequestParams::new(TOOL_FIND)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("find");
    let paths = found.structured_content.unwrap()["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["hello.txt".to_string()]);
    assert!(paths.iter().all(|p| !p.starts_with('/')));

    let escape = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "../hello.txt" })),
        )
        .await;
    let escape_text = format!("{escape:?}");
    assert!(
        escape_text.contains("PATH_ESCAPE") || escape_text.contains("escape"),
        "{escape_text}"
    );

    let link = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "link" })),
        )
        .await;
    let link_text = format!("{link:?}");
    assert!(
        link_text.contains("SYMLINK_REJECTED") || link_text.contains("symlink"),
        "{link_text}"
    );

    client.cancel().await.expect("cancel");
}
