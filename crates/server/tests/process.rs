use codespace_domain::{
    LIVE_TOOLS, TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND, TOOL_FIND, TOOL_PROCESS_RESIZE,
    TOOL_PROCESS_STATUS, TOOL_READ, TOOL_READ_PROCESS, TOOL_TERMINATE_PROCESS, TOOL_WORKSPACE_INFO,
    TOOL_WRITE_STDIN,
};
use codespace_server::config::{HttpConfig, MCP_PATH};
use codespace_server::http::router_with_registry;
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    },
    object,
    transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess},
    ClientLifecycleMode, ClientServiceExt, ServiceExt,
};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

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

async fn spawn_client(
    cfg: &std::path::Path,
    with_patch_helper: bool,
    extra_env: &[(&str, &str)],
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let helper = with_patch_helper.then(codespace_server::patch_helper::ensure_helper_for_tests);
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("CODESPACE_CONFIG", cfg);
            if let Some(helper) = &helper {
                cmd.env("CODESPACE_PATCH_BIN", helper);
            }
            for (key, value) in extra_env {
                cmd.env(*key, *value);
            }
        }))
        .expect("spawn"),
    )
    .await
    .expect("init")
}

fn write_workspace(profile: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "workspaces": {
                "demo": { "root": ws, "profile": profile }
            }
        })
        .to_string(),
    )
    .unwrap();
    (root, cfg)
}

#[tokio::test]
async fn exec_echo_is_readable_and_unknown_id_is_rejected() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "hello-codespace"]
            })),
        )
        .await
        .expect("exec");
    let body = payload(&started);
    let pid = body["process_id"].as_str().unwrap().to_string();
    assert!(pid.starts_with("proc-"));
    assert_eq!(body["dispatch_status"], "confirmed");

    let mut chunk = String::new();
    for _ in 0..50 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read");
        let out = payload(&read);
        chunk = out["chunk"].as_str().unwrap_or("").to_string();
        if out["eof"] == true || chunk.contains("hello-codespace") {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.contains("hello-codespace"),
        "missing process output: {chunk:?}"
    );

    let mut status_body = serde_json::Value::Null;
    for _ in 0..50 {
        let status = client
            .call_tool(
                CallToolRequestParams::new(TOOL_PROCESS_STATUS)
                    .with_arguments(object!({ "process_id": pid })),
            )
            .await
            .expect("process_status");
        status_body = payload(&status);
        if status_body["state"] == "exited" {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(status_body["state"], "exited");
    assert_eq!(status_body["termination"], "exited");
    assert_eq!(status_body["exit_code"], 0);
    assert_eq!(status_body["eof"], true);

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

    let _ = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WRITE_STDIN)
                .with_arguments(object!({ "process_id": pid, "data": "" })),
        )
        .await;
    let _ = client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await;

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn pipe_exec_combines_stdout_and_stderr_without_order() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;
    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sh", "-c", "echo OUT-MARKER; echo ERR-MARKER >&2"]
            })),
        )
        .await
        .expect("exec");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut chunk = String::new();
    for _ in 0..50 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read");
        let out = payload(&read);
        chunk = out["chunk"].as_str().unwrap_or("").to_string();
        if chunk.contains("OUT-MARKER") && chunk.contains("ERR-MARKER") {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.contains("OUT-MARKER") && chunk.contains("ERR-MARKER"),
        "combined stream must include both stdout and stderr markers, got {chunk:?}"
    );
    let _ = client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await;
    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn live_shell_blocks_patch_and_second_exec() {
    let (root, cfg) = write_workspace("workspace-write");
    std::fs::write(root.path().join("ws").join("note.txt"), "hi").unwrap();
    let client = spawn_client(&cfg, true, &[]).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sleep", "20"]
            })),
        )
        .await
        .expect("sleep");
    let started_body = payload(&started);
    let pid = started_body["process_id"].as_str().unwrap().to_string();
    assert_eq!(started_body["dispatch_status"], "confirmed");

    let live_read = client
        .call_tool(
            CallToolRequestParams::new(TOOL_READ).with_arguments(object!({
                "workspace_id": "demo",
                "path": "note.txt"
            })),
        )
        .await
        .expect("read while process live");
    assert_eq!(payload(&live_read)["content"], "hi");

    let live_find = client
        .call_tool(
            CallToolRequestParams::new(TOOL_FIND)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("find while process live");
    let paths = payload(&live_find)["paths"].as_array().cloned().unwrap();
    assert!(
        paths.iter().any(|p| p.as_str() == Some("note.txt")),
        "find while process live, got {paths:?}"
    );

    let busy_patch = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: x.txt\n+nope\n*** End Patch\n",
                "operation_key": "busy-1"
            })),
        )
        .await;
    let busy_text = err_text(&busy_patch);
    assert!(
        busy_text.contains("WORKSPACE_BUSY") || busy_text.contains("busy"),
        "{busy_text}"
    );

    let busy_exec = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "second"]
            })),
        )
        .await;
    let exec_text = err_text(&busy_exec);
    assert!(
        exec_text.contains("WORKSPACE_BUSY") || exec_text.contains("busy"),
        "{exec_text}"
    );

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await
        .expect("terminate");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn terminate_cancels_and_releases_busy() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sleep", "20"]
            })),
        )
        .await
        .expect("sleep");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await
        .expect("terminate");

    let mut released = false;
    for _ in 0..80 {
        let again = client
            .call_tool(
                CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                    "workspace_id": "demo",
                    "command": ["/bin/echo", "released"]
                })),
            )
            .await;
        if again.is_ok() {
            released = true;
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        released,
        "terminate must cancel the live shell and clear WORKSPACE_BUSY"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn write_stdin_reaches_process() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sh", "-c", "IFS= read -r line; printf '%s\\n' \"$line\""]
            })),
        )
        .await
        .expect("exec");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_WRITE_STDIN)
                .with_arguments(object!({ "process_id": pid, "data": "from-stdin\n" })),
        )
        .await
        .expect("stdin");

    let mut chunk = String::new();
    for _ in 0..50 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read");
        let out = payload(&read);
        chunk = out["chunk"].as_str().unwrap_or("").to_string();
        if chunk.contains("from-stdin") {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.contains("from-stdin"),
        "write_stdin must reach the managed process: {chunk:?}"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn output_is_bounded() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;

    let overflow = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": [
                    "python3",
                    "-c",
                    "import sys; sys.stdout.write('A'*300000); sys.stdout.flush()"
                ]
            })),
        )
        .await
        .expect("overflow exec");
    let overflow_pid = payload(&overflow)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut chunk = String::new();
    for _ in 0..80 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": overflow_pid, "cursor": 0 })),
            )
            .await
            .expect("read overflow");
        let out = payload(&read);
        chunk = out["chunk"].as_str().unwrap_or("").to_string();
        if out["eof"] == true {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.len() <= codespace_runner::MAX_OUTPUT_BYTES,
        "output ring must stay bounded, got {}",
        chunk.len()
    );
    assert!(chunk.contains('A'), "expected retained tail of overflow");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn timeout_kills_managed_process() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[("CODESPACE_PROCESS_TIMEOUT_SECS", "1")]).await;

    let timed = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sleep", "20"]
            })),
        )
        .await
        .expect("timeout exec");
    let timed_pid = payload(&timed)["process_id"].as_str().unwrap().to_string();

    let mut saw_timeout = false;
    for _ in 0..80 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": timed_pid, "cursor": 0 })),
            )
            .await;
        let text = err_text(&read);
        if text.contains("TIMEOUT") || text.contains("time limit") {
            saw_timeout = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(saw_timeout, "expected TIMEOUT after process time limit");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn read_only_profile_rejects_exec() {
    let (_root, cfg) = write_workspace("read-only");
    let client = spawn_client(&cfg, false, &[]).await;
    let denied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "nope"]
            })),
        )
        .await;
    let text = err_text(&denied);
    assert!(
        text.contains("UNAUTHORIZED") || text.contains("read-only"),
        "{text}"
    );
    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn linux_container_environment_rejects_exec() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let cfg = root.path().join("workspaces.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "environments": { "box": { "kind": "linux-container" } },
            "workspaces": {
                "demo": { "root": ws, "profile": "workspace-write", "environment": "box" }
            }
        })
        .to_string(),
    )
    .unwrap();
    let client = spawn_client(&cfg, false, &[]).await;
    let denied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "nope"]
            })),
        )
        .await;
    let text = err_text(&denied);
    assert!(
        text.contains("UNAUTHORIZED") || text.contains("linux-container"),
        "{text}"
    );
    assert!(
        !text.contains("\"operation_id\""),
        "linux-container must fail before minting operation_id: {text}"
    );

    let patch_denied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+x\n*** End Patch\n"
            })),
        )
        .await;
    let patch_text = err_text(&patch_denied);
    assert!(
        patch_text.contains("UNAUTHORIZED") || patch_text.contains("linux-container"),
        "{patch_text}"
    );
    assert!(
        !patch_text.contains("\"operation_id\""),
        "linux-container apply_patch must not mint operation_id: {patch_text}"
    );
    client.cancel().await.expect("cancel");
}

async fn spawn_http(cfg: &std::path::Path) -> std::net::SocketAddr {
    let registry = codespace_policy::Registry::load_path(cfg).expect("registry");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = router_with_registry(
        HttpConfig {
            host: "127.0.0.1".into(),
            port: addr.port(),
            bearer_token: None,
        },
        registry,
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("http serve");
    });
    addr
}

async fn http_client(
    addr: std::net::SocketAddr,
) -> rmcp::service::RunningService<rmcp::RoleClient, rmcp::model::InitializeRequestParams> {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("codespace-process", "0.0.0"),
    )
    .with_protocol_version(ProtocolVersion::V_2025_11_25)
    .serve_with_lifecycle(
        StreamableHttpClientTransport::from_uri(format!("http://{addr}{MCP_PATH}")),
        ClientLifecycleMode::Initialize,
    )
    .await
    .expect("http initialize 2025-11-25")
}

#[tokio::test]
async fn http_disconnect_is_not_process_death() {
    let (root, cfg) = write_workspace("workspace-write");
    let ws = root.path().join("ws");
    let addr = spawn_http(&cfg).await;
    let client = http_client(addr).await;

    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": [
                    "/bin/sh",
                    "-c",
                    "printf started > marker.txt; sleep 2; printf survived > marker.txt"
                ]
            })),
        )
        .await
        .expect("exec");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

    let marker = ws.join("marker.txt");
    for _ in 0..50 {
        if marker.exists() {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(marker.exists(), "managed process should have started");

    client.cancel().await.expect("drop first session");
    sleep(Duration::from_millis(2200)).await;
    let body = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        body, "survived",
        "HTTP disconnect must not kill the managed process, got {body:?}"
    );

    let client2 = http_client(addr).await;
    let read = client2
        .call_tool(
            CallToolRequestParams::new(TOOL_READ_PROCESS)
                .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
        )
        .await;
    let text = err_text(&read);
    assert!(
        read.is_ok(),
        "process_id must remain valid after reconnect: {text}"
    );

    let _ = client2
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await;
    client2.cancel().await.expect("cancel");
}

#[tokio::test]
async fn exec_command_schema_has_optional_tty_and_live_tools_unchanged() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;
    let tools = client.list_all_tools().await.expect("tools/list");
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort();
    let mut expected = LIVE_TOOLS.to_vec();
    expected.sort();
    assert_eq!(names, expected, "tools/list must match LIVE_TOOLS");
    assert!(
        names.contains(&TOOL_PROCESS_STATUS),
        "LIVE_TOOLS must include process_status, got {names:?}"
    );
    assert!(
        names.contains(&TOOL_PROCESS_RESIZE),
        "LIVE_TOOLS must include process_resize, got {names:?}"
    );

    let status_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_PROCESS_STATUS)
        .expect("process_status");
    let status_out = serde_json::to_value(status_tool.output_schema.as_ref()).unwrap();
    let status_dumped = status_out.to_string();
    assert!(
        status_dumped.contains("termination"),
        "process_status result schema must include termination: {status_dumped}"
    );
    assert!(
        status_dumped.contains("output_total"),
        "process_status result schema must include output_total: {status_dumped}"
    );

    let resize_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_PROCESS_RESIZE)
        .expect("process_resize");
    let resize_in = serde_json::to_value(&resize_tool.input_schema).unwrap();
    let resize_in_dumped = resize_in.to_string();
    assert!(
        resize_in_dumped.contains("process_id"),
        "process_resize input must include process_id: {resize_in_dumped}"
    );
    assert!(
        resize_in_dumped.contains("rows") && resize_in_dumped.contains("cols"),
        "process_resize input must include rows and cols: {resize_in_dumped}"
    );
    let resize_out = serde_json::to_value(resize_tool.output_schema.as_ref()).unwrap();
    let resize_out_dumped = resize_out.to_string();
    assert!(
        resize_out_dumped.contains("\"ok\"")
            && resize_out_dumped.contains("rows")
            && resize_out_dumped.contains("cols"),
        "process_resize result schema must include ok, rows, cols: {resize_out_dumped}"
    );

    let read_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_READ)
        .expect("read");
    let read_desc = read_tool.description.as_deref().unwrap_or("");
    assert!(
        read_desc.contains("next_offset") && read_desc.contains("content_lossy"),
        "read description must mention next_offset and content_lossy: {read_desc}"
    );
    let read_in = serde_json::to_value(&read_tool.input_schema)
        .unwrap()
        .to_string();
    assert!(
        read_in.contains("offset") && read_in.contains("limit"),
        "read input schema must include offset and limit: {read_in}"
    );
    let read_out = serde_json::to_value(read_tool.output_schema.as_ref())
        .unwrap()
        .to_string();
    assert!(
        read_out.contains("byte_count")
            && read_out.contains("truncated")
            && read_out.contains("content_lossy")
            && read_out.contains("next_offset"),
        "read result schema must include byte_count, truncated, content_lossy, next_offset: {read_out}"
    );
    let find_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_FIND)
        .expect("find");
    let find_desc = find_tool.description.as_deref().unwrap_or("");
    assert!(
        find_desc.contains("next_offset")
            && find_desc.contains("incomplete")
            && find_desc.contains("listing_version"),
        "find description must mention next_offset, incomplete, listing_version: {find_desc}"
    );
    let find_in = serde_json::to_value(&find_tool.input_schema)
        .unwrap()
        .to_string();
    assert!(
        find_in.contains("offset") && find_in.contains("limit"),
        "find input schema must include offset and limit: {find_in}"
    );
    let find_out = serde_json::to_value(find_tool.output_schema.as_ref())
        .unwrap()
        .to_string();
    assert!(
        find_out.contains("incomplete")
            && find_out.contains("listing_version")
            && find_out.contains("next_offset")
            && find_out.contains("truncated"),
        "find result schema must include incomplete, listing_version, next_offset, truncated: {find_out}"
    );

    let exec = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_EXEC_COMMAND)
        .expect("exec_command");
    let exec_desc = exec.description.as_deref().unwrap_or("");
    assert!(exec_desc.contains("dispatch_status=unknown"), "{exec_desc}");
    assert!(exec_desc.contains("PROCESS_SPAWN_FAILED"), "{exec_desc}");
    assert!(
        exec_desc.contains("distinct from dispatch_status=unknown"),
        "{exec_desc}"
    );
    assert!(exec_desc.contains("WORKSPACE_BUSY"), "{exec_desc}");
    assert!(
        exec_desc.contains("uncertain attempt") || exec_desc.contains("backend remains reachable"),
        "{exec_desc}"
    );
    assert!(
        !exec_desc.contains("inspect or terminate the returned process_id instead"),
        "{exec_desc}"
    );
    let info_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_WORKSPACE_INFO)
        .expect("workspace_info");
    let info_desc = info_tool.description.as_deref().unwrap_or("");
    assert!(info_desc.contains("process.available"), "{info_desc}");
    assert!(info_desc.contains("files.*.available"), "{info_desc}");
    assert!(info_desc.contains("occupancy"), "{info_desc}");
    assert!(info_desc.contains("WORKSPACE_BUSY"), "{info_desc}");
    let patch = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TOOL_APPLY_PATCH)
        .expect("apply_patch");
    let patch_desc = patch.description.as_deref().unwrap_or("");
    assert!(patch_desc.contains("status=unknown"), "{patch_desc}");
    let schema = serde_json::to_value(&exec.input_schema).unwrap();
    let dumped = schema.to_string();
    let output = serde_json::to_value(exec.output_schema.as_ref()).unwrap();
    let output_dumped = output.to_string();
    assert!(
        output_dumped.contains("dispatch_status"),
        "exec_command result schema must include dispatch_status: {output_dumped}"
    );
    assert!(
        dumped.contains("\"tty\""),
        "exec_command input schema must include tty: {dumped}"
    );
    assert!(
        !dumped.contains("environment_id"),
        "exec_command must not grow environment_id: {dumped}"
    );
    assert!(
        !dumped.contains("\"cwd\""),
        "exec_command must not grow cwd: {dumped}"
    );
    assert!(
        !dumped.contains("tty_size"),
        "exec_command must not grow tty_size: {dumped}"
    );

    let info = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("workspace_info");
    let exec = &payload(&info)["execution"];
    assert_eq!(exec["permissions"]["exec"], true);
    assert_eq!(exec["environment"]["exec_supported"], true);
    assert_eq!(exec["environment"]["file_read_supported"], true);
    assert_eq!(exec["environment"]["file_write_supported"], true);
    assert_eq!(exec["files"]["read"]["available"], true);
    assert_eq!(exec["files"]["find"]["available"], true);
    assert_eq!(exec["files"]["patch"]["available"], true);
    assert_eq!(exec["files"]["capabilities"]["read_range"], true);
    assert_eq!(exec["files"]["capabilities"]["find_pagination"], true);
    assert_eq!(exec["files"]["capabilities"]["read_max_bytes"], 1048576);
    assert_eq!(exec["files"]["capabilities"]["find_max_paths"], 10000);
    assert_eq!(exec["process"]["available"], true);
    assert_eq!(exec["process"]["capabilities"]["tty"]["supported"], true);
    assert_eq!(
        exec["process"]["capabilities"]["tty"]["resize_supported"],
        true
    );
    assert_eq!(
        exec["process"]["capabilities"]["lifetime"]["owner"],
        "runner"
    );
    assert_eq!(
        exec["process"]["capabilities"]["lifetime"]["client_disconnect"],
        "keep_running"
    );
    assert_eq!(
        exec["process"]["capabilities"]["lifetime"]["runner_disconnect"],
        "terminate"
    );
    assert_eq!(
        exec["process"]["capabilities"]["lifetime"]["gateway_shutdown"],
        "terminate"
    );
    assert_eq!(
        exec["process"]["capabilities"]["lifetime"]["restart_recovery"],
        "none"
    );
    if codespace_runner::linux_sandbox_available() {
        assert_eq!(exec["isolation"]["command_sandbox"], "linux-sandbox");
        assert_eq!(exec["network"]["enforcement"], "enforced");
    } else {
        assert_eq!(exec["isolation"]["command_sandbox"], "none");
        assert_eq!(exec["network"]["enforcement"], "none");
    }
    assert_eq!(exec["network"]["policy"], "restricted");
    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn exec_tty_true_sees_a_tty() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;
    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sh", "-c", "if [ -t 0 ]; then echo ISATTY; else echo NOTTY; fi"],
                "tty": true
            })),
        )
        .await
        .expect("exec tty");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut chunk = String::new();
    for _ in 0..50 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read");
        let out = payload(&read);
        chunk = out["chunk"].as_str().unwrap_or("").to_string();
        if out["eof"] == true || chunk.contains("ISATTY") {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.contains("ISATTY"),
        "exec_command tty:true should attach a PTY, got {chunk:?}"
    );
    let _ = client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pid })),
        )
        .await;
    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn process_resize_pty_roundtrip_and_error_codes() {
    let (_root, cfg) = write_workspace("workspace-write");
    let client = spawn_client(&cfg, false, &[]).await;
    let started = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": [
                    "/bin/sh",
                    "-c",
                    "stty -echo; printf 'start:%s\\n' \"$(stty size)\"; IFS= read _line; printf 'after:%s\\n' \"$(stty size)\""
                ],
                "tty": true
            })),
        )
        .await
        .expect("exec tty resize");
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut chunk = String::new();
    for _ in 0..50 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read");
        chunk = payload(&read)["chunk"]
            .as_str()
            .unwrap_or("")
            .replace("\r\n", "\n");
        if chunk.contains("start:24 80") {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.contains("start:24 80"),
        "initial PTY size, got {chunk:?}"
    );
    let resized = client
        .call_tool(
            CallToolRequestParams::new(TOOL_PROCESS_RESIZE).with_arguments(object!({
                "process_id": pid,
                "rows": 40,
                "cols": 120
            })),
        )
        .await
        .expect("process_resize");
    let body = payload(&resized);
    assert_eq!(body["ok"], true);
    assert_eq!(body["rows"], 40);
    assert_eq!(body["cols"], 120);
    let _ = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WRITE_STDIN)
                .with_arguments(object!({ "process_id": pid, "data": "go\n" })),
        )
        .await
        .expect("write go");
    for _ in 0..50 {
        let read = client
            .call_tool(
                CallToolRequestParams::new(TOOL_READ_PROCESS)
                    .with_arguments(object!({ "process_id": pid, "cursor": 0 })),
            )
            .await
            .expect("read after resize");
        chunk = payload(&read)["chunk"]
            .as_str()
            .unwrap_or("")
            .replace("\r\n", "\n");
        if chunk.contains("after:40 120") || payload(&read)["eof"] == true {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    assert!(
        chunk.contains("after:40 120"),
        "resized PTY size, got {chunk:?}"
    );
    for _ in 0..50 {
        let status = client
            .call_tool(
                CallToolRequestParams::new(TOOL_PROCESS_STATUS)
                    .with_arguments(object!({ "process_id": pid })),
            )
            .await
            .expect("status");
        if payload(&status)["state"] == "exited" {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }

    let missing = client
        .call_tool(
            CallToolRequestParams::new(TOOL_PROCESS_RESIZE).with_arguments(object!({
                "process_id": "proc-invented",
                "rows": 24,
                "cols": 80
            })),
        )
        .await;
    let missing_text = err_text(&missing);
    assert!(missing_text.contains("PROCESS_NOT_FOUND"), "{missing_text}");

    let pipe = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sleep", "30"]
            })),
        )
        .await
        .expect("pipe sleep");
    let pipe_pid = payload(&pipe)["process_id"].as_str().unwrap().to_string();
    let not_tty = client
        .call_tool(
            CallToolRequestParams::new(TOOL_PROCESS_RESIZE).with_arguments(object!({
                "process_id": pipe_pid,
                "rows": 24,
                "cols": 80
            })),
        )
        .await;
    let not_tty_text = err_text(&not_tty);
    assert!(not_tty_text.contains("PROCESS_NOT_TTY"), "{not_tty_text}");
    let _ = client
        .call_tool(
            CallToolRequestParams::new(TOOL_TERMINATE_PROCESS)
                .with_arguments(object!({ "process_id": pipe_pid })),
        )
        .await;
    for _ in 0..50 {
        let status = client
            .call_tool(
                CallToolRequestParams::new(TOOL_PROCESS_STATUS)
                    .with_arguments(object!({ "process_id": pipe_pid })),
            )
            .await
            .expect("pipe status");
        if payload(&status)["state"] == "exited" {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }

    let finished = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/echo", "done"],
                "tty": true
            })),
        )
        .await
        .expect("tty true exit");
    let finished_pid = payload(&finished)["process_id"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..50 {
        let status = client
            .call_tool(
                CallToolRequestParams::new(TOOL_PROCESS_STATUS)
                    .with_arguments(object!({ "process_id": finished_pid })),
            )
            .await
            .expect("status");
        if payload(&status)["state"] == "exited" {
            break;
        }
        sleep(Duration::from_millis(40)).await;
    }
    let not_running = client
        .call_tool(
            CallToolRequestParams::new(TOOL_PROCESS_RESIZE).with_arguments(object!({
                "process_id": finished_pid,
                "rows": 24,
                "cols": 80
            })),
        )
        .await;
    let not_running_text = err_text(&not_running);
    assert!(
        not_running_text.contains("PROCESS_NOT_RUNNING"),
        "{not_running_text}"
    );

    client.cancel().await.expect("cancel");
}
