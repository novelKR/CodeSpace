use codespace_domain::{
    TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND, TOOL_READ_PROCESS, TOOL_TERMINATE_PROCESS,
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
async fn live_shell_blocks_patch_and_second_exec() {
    let (_root, cfg) = write_workspace("workspace-write");
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
    let pid = payload(&started)["process_id"]
        .as_str()
        .unwrap()
        .to_string();

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
