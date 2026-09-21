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
    assert_eq!(body["byte_count"], 2);
    assert!(body.get("offset").is_none());
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

    let missing = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "missing.txt" })),
        )
        .await;
    let missing_text = format!("{missing:?}");
    assert!(missing_text.contains("FILE_NOT_FOUND"), "{missing_text}");

    std::fs::write(ws.join("foo"), "not-a-dir").unwrap();
    let not_dir = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "foo/bar.txt" })),
        )
        .await;
    let not_dir_text = format!("{not_dir:?}");
    assert!(
        not_dir_text.contains("PATH_NOT_DIRECTORY"),
        "{not_dir_text}"
    );

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

    let fifo = ws.join("pipe.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo should exist");
    let special = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "pipe.fifo" })),
        )
        .await;
    let special_text = format!("{special:?}");
    assert!(
        special_text.contains("SPECIAL_FILE_REJECTED"),
        "{special_text}"
    );

    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "leak").unwrap();
    std::os::unix::fs::symlink(outside.path(), ws.join("via")).unwrap();
    let via = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ)
                .with_arguments(object!({ "workspace_id": "demo", "path": "via/secret.txt" })),
        )
        .await;
    let via_text = format!("{via:?}");
    assert!(
        via_text.contains("SYMLINK_REJECTED") || via_text.contains("symlink"),
        "{via_text}"
    );

    std::fs::remove_dir_all(&ws).unwrap();
    let gone = client
        .call_tool(
            CallToolRequestParams::new(TOOL_FIND)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await;
    let gone_text = format!("{gone:?}");
    assert!(gone_text.contains("FILE_OPERATION_FAILED"), "{gone_text}");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn read_and_find_paginate_windows() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("big.txt"), "abcdef").unwrap();
    std::fs::write(ws.join("a.rs"), "a").unwrap();
    std::fs::write(ws.join("b.rs"), "b").unwrap();
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

    let info = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("workspace_info");
    let caps =
        &info.structured_content.clone().expect("structured")["execution"]["files"]["capabilities"];
    assert_eq!(caps["read_range"], true);
    assert_eq!(caps["find_pagination"], true);
    assert_eq!(caps["read_max_bytes"], 1048576);
    assert_eq!(caps["find_max_paths"], 10000);

    let first = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ).with_arguments(object!({
                "workspace_id": "demo",
                "path": "big.txt",
                "limit": 3
            })),
        )
        .await
        .expect("read window");
    let body = first.structured_content.clone().expect("structured");
    assert_eq!(body["content"], "abc");
    assert_eq!(body["truncated"], true);
    assert_eq!(body["byte_count"], 3);
    assert!(body.get("offset").is_none());
    let version = body["version"].clone();

    let rest = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ).with_arguments(object!({
                "workspace_id": "demo",
                "path": "big.txt",
                "offset": 3,
                "limit": 3
            })),
        )
        .await
        .expect("read rest");
    let rest_body = rest.structured_content.clone().expect("structured");
    assert_eq!(rest_body["content"], "def");
    assert_eq!(rest_body["truncated"], false);
    assert_eq!(rest_body["offset"], 3);
    assert_eq!(rest_body["byte_count"], 3);
    assert_eq!(rest_body["version"], version);

    let page = client
        .call_tool(
            CallToolRequestParams::new(TOOL_FIND).with_arguments(object!({
                "workspace_id": "demo",
                "limit": 1
            })),
        )
        .await
        .expect("find page");
    let page_body = page.structured_content.clone().expect("structured");
    assert_eq!(page_body["truncated"], true);
    assert_eq!(page_body["paths"].as_array().unwrap().len(), 1);
    assert!(page_body.get("offset").is_none());

    let next = client
        .call_tool(
            CallToolRequestParams::new(TOOL_FIND).with_arguments(object!({
                "workspace_id": "demo",
                "offset": 1,
                "limit": 2
            })),
        )
        .await
        .expect("find rest");
    let next_body = next.structured_content.clone().expect("structured");
    assert_eq!(next_body["offset"], 1);
    assert_eq!(next_body["truncated"], false);
    assert_eq!(next_body["paths"].as_array().unwrap().len(), 2);

    let bad = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ).with_arguments(object!({
                "workspace_id": "demo",
                "path": "big.txt",
                "limit": 0
            })),
        )
        .await;
    let bad_text = format!("{bad:?}");
    assert!(bad_text.contains("OUTPUT_LIMIT"), "{bad_text}");

    client.cancel().await.expect("cancel");
}
