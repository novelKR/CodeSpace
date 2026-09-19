use codespace_domain::{
    TOOL_APPLY_PATCH, TOOL_APPROVAL_CREATE, TOOL_APPROVAL_RESOLVE, TOOL_EXEC_COMMAND,
    TOOL_OPERATION_RESUME, TOOL_OPERATION_STATUS, TOOL_WORKSPACE_INFO,
};
use rmcp::{
    model::CallToolRequestParams,
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use tokio::process::Command;

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

fn error_body(
    result: &Result<rmcp::model::CallToolResult, rmcp::service::ServiceError>,
) -> serde_json::Value {
    let text = err_text(result);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        return value;
    }
    let start = text.find('{').unwrap_or_else(|| panic!("{text}"));
    let end = text.rfind('}').unwrap_or_else(|| panic!("{text}"));
    serde_json::from_str(&text[start..=end]).unwrap_or_else(|_| panic!("{text}"))
}

fn write_workspace(
    profile: &str,
    approvals: Option<&str>,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let cfg = root.path().join("workspaces.json");
    let mut demo = serde_json::json!({
        "root": ws,
        "profile": profile
    });
    if let Some(mode) = approvals {
        demo["approvals"] = serde_json::Value::String(mode.to_string());
    }
    std::fs::write(
        &cfg,
        serde_json::json!({ "workspaces": { "demo": demo } }).to_string(),
    )
    .unwrap();
    (root, cfg, ws)
}

async fn spawn_client(
    cfg: &std::path::Path,
    db: &std::path::Path,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_codespace-mcp");
    let helper = codespace_server::patch_helper::ensure_helper_for_tests();
    ().serve(
        TokioChildProcess::new(Command::new(bin).configure(|cmd| {
            cmd.env("CODESPACE_CONFIG", cfg)
                .env("CODESPACE_OPERATIONS_DB", db)
                .env("CODESPACE_PATCH_BIN", helper);
        }))
        .expect("spawn"),
    )
    .await
    .expect("init")
}

const ADD_PATCH: &str = "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** End Patch\n";

#[tokio::test]
async fn read_only_create_and_resume_cannot_open_write_or_exec() {
    let (root, cfg, ws) = write_workspace("read-only", None);
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, &db).await;

    let create = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_CREATE).with_arguments(object!({
                "tool": "apply_patch",
                "arguments": {
                    "workspace_id": "demo",
                    "patch": ADD_PATCH,
                    "approved": true,
                    "network": true
                }
            })),
        )
        .await;
    let created = error_body(&create);
    assert_eq!(created["code"], "UNAUTHORIZED");
    assert!(!ws.join("created.txt").exists());

    let apply = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": ADD_PATCH,
                "approved": true
            })),
        )
        .await;
    assert_eq!(error_body(&apply)["code"], "UNAUTHORIZED");
    assert!(!ws.join("created.txt").exists());

    let exec = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_CREATE).with_arguments(object!({
                "tool": "exec_command",
                "arguments": {
                    "workspace_id": "demo",
                    "command": ["/bin/echo", "nope"],
                    "approved": true
                }
            })),
        )
        .await;
    assert_eq!(error_body(&exec)["code"], "UNAUTHORIZED");

    let resume = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_RESUME).with_arguments(object!({
                "approval_id": "appr-missing"
            })),
        )
        .await;
    assert_eq!(error_body(&resume)["code"], "APPROVAL_NOT_FOUND");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn confirm_apply_holds_then_grant_resume_writes_once() {
    let (root, cfg, ws) = write_workspace("workspace-write", Some("confirm"));
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, &db).await;

    let info = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("info");
    assert_eq!(payload(&info)["execution"]["approvals"], "confirm");

    let held = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": ADD_PATCH,
                "operation_key": "hold-1"
            })),
        )
        .await;
    let body = error_body(&held);
    assert_eq!(body["code"], "APPROVAL_REQUIRED");
    let approval_id = body["approval_id"]
        .as_str()
        .expect("approval_id")
        .to_string();
    assert!(approval_id.starts_with("appr-"));
    assert!(!ws.join("created.txt").exists());

    let granted = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_RESOLVE).with_arguments(object!({
                "approval_id": approval_id,
                "decision": "grant"
            })),
        )
        .await
        .expect("grant");
    assert_eq!(payload(&granted)["state"], "granted");
    assert!(!ws.join("created.txt").exists());

    let resumed = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_RESUME)
                .with_arguments(object!({ "approval_id": approval_id })),
        )
        .await
        .expect("resume");
    let resume_body = payload(&resumed);
    assert_eq!(resume_body["state"], "consumed");
    assert_eq!(resume_body["apply_patch"]["status"], "applied");
    let operation_id = resume_body["apply_patch"]["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        std::fs::read_to_string(ws.join("created.txt")).unwrap(),
        "hello\n"
    );

    let replay = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_RESUME)
                .with_arguments(object!({ "approval_id": approval_id })),
        )
        .await
        .expect("replay");
    assert_eq!(
        payload(&replay)["apply_patch"]["operation_id"],
        operation_id
    );

    let status = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_STATUS)
                .with_arguments(object!({ "operation_id": operation_id })),
        )
        .await
        .expect("status");
    let status_body = payload(&status);
    assert_eq!(status_body["kind"], "patch");
    assert_eq!(status_body["status"], "applied");
    assert_eq!(status_body["files"][0], "created.txt");

    let second = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": "*** Begin Patch\n*** Add File: other.txt\n+x\n*** End Patch\n"
            })),
        )
        .await;
    assert_eq!(error_body(&second)["code"], "APPROVAL_REQUIRED");
    assert!(!ws.join("other.txt").exists());

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn deny_then_resume_fails() {
    let (root, cfg, ws) = write_workspace("workspace-write", Some("confirm"));
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, &db).await;

    let held = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": ADD_PATCH
            })),
        )
        .await;
    let approval_id = error_body(&held)["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    let denied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_RESOLVE).with_arguments(object!({
                "approval_id": approval_id,
                "decision": "deny"
            })),
        )
        .await
        .expect("deny");
    assert_eq!(payload(&denied)["state"], "denied");

    let resume = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_RESUME)
                .with_arguments(object!({ "approval_id": approval_id })),
        )
        .await;
    assert_eq!(error_body(&resume)["code"], "APPROVAL_CONFLICT");
    assert!(!ws.join("created.txt").exists());

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn off_apply_still_runs_immediately() {
    let (root, cfg, ws) = write_workspace("workspace-write", None);
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, &db).await;

    let info = client
        .call_tool(
            CallToolRequestParams::new(TOOL_WORKSPACE_INFO)
                .with_arguments(object!({ "workspace_id": "demo" })),
        )
        .await
        .expect("info");
    assert_eq!(payload(&info)["execution"]["approvals"], "off");

    let applied = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPLY_PATCH).with_arguments(object!({
                "workspace_id": "demo",
                "patch": ADD_PATCH,
                "operation_key": "immediate"
            })),
        )
        .await
        .expect("apply");
    assert_eq!(payload(&applied)["status"], "applied");
    assert_eq!(
        std::fs::read_to_string(ws.join("created.txt")).unwrap(),
        "hello\n"
    );

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn confirm_exec_holds_until_resume() {
    let (root, cfg, ws) = write_workspace("workspace-write", Some("confirm"));
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, &db).await;

    let held = client
        .call_tool(
            CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                "workspace_id": "demo",
                "command": ["/bin/sh", "-c", "printf x > held.txt"]
            })),
        )
        .await;
    let body = error_body(&held);
    assert_eq!(body["code"], "APPROVAL_REQUIRED");
    assert!(!ws.join("held.txt").exists());
    let approval_id = body["approval_id"].as_str().unwrap().to_string();

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_RESOLVE).with_arguments(object!({
                "approval_id": approval_id,
                "decision": "grant"
            })),
        )
        .await
        .expect("grant");

    let resumed = client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_RESUME)
                .with_arguments(object!({ "approval_id": approval_id })),
        )
        .await
        .expect("resume");
    let resume_body = payload(&resumed);
    assert!(resume_body["exec_command"]["process_id"]
        .as_str()
        .unwrap()
        .starts_with("proc-"));

    for _ in 0..50 {
        if ws.join("held.txt").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read_to_string(ws.join("held.txt")).unwrap(), "x");

    client.cancel().await.expect("cancel");
}

#[tokio::test]
async fn explicit_create_works_when_approvals_are_off() {
    let (root, cfg, ws) = write_workspace("workspace-write", Some("off"));
    let db = root.path().join("ops.sqlite");
    let client = spawn_client(&cfg, &db).await;

    let created = client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_CREATE).with_arguments(object!({
                "tool": "apply_patch",
                "arguments": {
                    "workspace_id": "demo",
                    "patch": ADD_PATCH
                }
            })),
        )
        .await
        .expect("create");
    let approval_id = payload(&created)["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(payload(&created)["state"], "pending");
    assert!(!ws.join("created.txt").exists());

    client
        .call_tool(
            CallToolRequestParams::new(TOOL_APPROVAL_RESOLVE).with_arguments(object!({
                "approval_id": approval_id,
                "decision": "grant"
            })),
        )
        .await
        .expect("grant");
    client
        .call_tool(
            CallToolRequestParams::new(TOOL_OPERATION_RESUME)
                .with_arguments(object!({ "approval_id": approval_id })),
        )
        .await
        .expect("resume");
    assert_eq!(
        std::fs::read_to_string(ws.join("created.txt")).unwrap(),
        "hello\n"
    );

    client.cancel().await.expect("cancel");
}
