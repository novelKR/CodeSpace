//! The gateway with an independent Runner (P1R-C01), each started as an operator starts it: the
//! Runner outlives a gateway, the next gateway inherits its processes but starts nothing, and a
//! gateway that ends leaves or stops the Runner as its exit intent says. These tests need the
//! worker binary (`CODESPACE_RUNTIME_BIN`) and return early without it, as the gateway's other
//! worker tests do.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use codespace_domain::{TOOL_EXEC_COMMAND, TOOL_PROCESS_STATUS, TOOL_READ_PROCESS};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RunningService, ServiceError};
use rmcp::{object, RoleClient, ServiceExt};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout, Instant};

/// The process timeout that every gateway here gives its executions.
const TIMEOUT: Duration = Duration::from_secs(5);

fn runtime_bin() -> Option<PathBuf> {
    std::env::var_os("CODESPACE_RUNTIME_BIN").map(PathBuf::from)
}

fn payload(result: &CallToolResult) -> serde_json::Value {
    result.structured_content.clone().unwrap_or_else(|| {
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
    })
}

fn error_body(result: &Result<CallToolResult, ServiceError>) -> serde_json::Value {
    let text = match result {
        Ok(r) => r
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join(""),
        Err(err) => err.to_string(),
    };
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        return value;
    }
    let start = text.find('{').unwrap_or_else(|| panic!("{text}"));
    let end = text.rfind('}').unwrap_or_else(|| panic!("{text}"));
    serde_json::from_str(&text[start..=end]).unwrap_or_else(|_| panic!("{text}"))
}

/// A workspace, the gateways' operations database, and where the Runner's socket goes.
struct Setup {
    root: tempfile::TempDir,
    cfg: PathBuf,
    ws: PathBuf,
    socket: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let cfg = root.path().join("workspaces.json");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "workspaces": { "demo": { "root": ws, "profile": "workspace-write" } }
            })
            .to_string(),
        )
        .unwrap();
        // The Runner creates its socket's directory itself.
        let socket = root.path().join("runner").join("runner.sock");
        Self {
            root,
            cfg,
            ws,
            socket,
        }
    }

    /// An independent Runner, started as an operator starts it, once it listens.
    async fn start_runner(&self, bin: &Path) -> Child {
        let runner = Command::new(bin)
            .arg(&self.socket)
            .args(["--lifecycle", "independent"])
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the Runner");
        // A connection that says nothing leaves the Runner as it was.
        let deadline = Instant::now() + Duration::from_secs(15);
        while std::os::unix::net::UnixStream::connect(&self.socket).is_err() {
            assert!(Instant::now() < deadline, "the Runner does not listen");
            sleep(Duration::from_millis(20)).await;
        }
        runner
    }

    /// The gateway binary with these settings alone: the environment of the test run may name a
    /// worker for the gateway to start, which an independent Runner's gateway refuses.
    fn gateway_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codespace-mcp"));
        for variable in [
            "CODESPACE_RUNNER",
            "CODESPACE_RUNNER_SOCKET",
            "CODESPACE_RUNNER_DIR",
            "CODESPACE_RUNTIME_BIN",
        ] {
            command.env_remove(variable);
        }
        command
            .env("CODESPACE_CONFIG", &self.cfg)
            .env(
                "CODESPACE_OPERATIONS_DB",
                self.root.path().join("ops.sqlite"),
            )
            .env(
                "CODESPACE_PROCESS_TIMEOUT_SECS",
                TIMEOUT.as_secs().to_string(),
            )
            .args(args)
            .kill_on_drop(true);
        command
    }

    /// A gateway connected to the independent Runner, whose normal shutdown asks `exit_intent`
    /// of it.
    async fn gateway(&self, exit_intent: &str) -> Gateway {
        let socket = self.socket.display().to_string();
        let mut process = self
            .gateway_command(&[
                "--runner",
                "uds",
                "--runner-socket",
                &socket,
                "--runner-lifecycle",
                "independent",
                "--runner-exit-intent",
                exit_intent,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the gateway");
        let stdout = process.stdout.take().unwrap();
        let stdin = process.stdin.take().unwrap();
        let client = ().serve((stdout, stdin)).await.expect("initialize");
        Gateway { client, process }
    }

    /// The error of a gateway that does not start with `args`.
    async fn refused(&self, args: &[&str]) -> String {
        let output = timeout(
            Duration::from_secs(30),
            self.gateway_command(args).stdin(Stdio::null()).output(),
        )
        .await
        .expect("the gateway ends")
        .expect("run the gateway");
        assert!(!output.status.success(), "{args:?}: {output:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

struct Gateway {
    client: RunningService<RoleClient, ()>,
    process: Child,
}

impl Gateway {
    async fn exec(&self, command: &[&str]) -> Result<CallToolResult, ServiceError> {
        self.client
            .call_tool(
                CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                    "workspace_id": "demo",
                    "command": command
                })),
            )
            .await
    }

    /// Start `command` and return its process id.
    async fn started(&self, command: &[&str]) -> String {
        let started = self.exec(command).await.expect("exec");
        let body = payload(&started);
        assert_eq!(body["dispatch_status"], "confirmed", "{body}");
        body["process_id"].as_str().unwrap().to_string()
    }

    async fn status(&self, process_id: &str) -> serde_json::Value {
        let status = self
            .client
            .call_tool(
                CallToolRequestParams::new(TOOL_PROCESS_STATUS)
                    .with_arguments(object!({ "process_id": process_id })),
            )
            .await
            .expect("process_status");
        payload(&status)
    }

    /// The process's output once it contains `expected`.
    async fn output(&self, process_id: &str, expected: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let read = self
                .client
                .call_tool(
                    CallToolRequestParams::new(TOOL_READ_PROCESS)
                        .with_arguments(object!({ "process_id": process_id, "cursor": 0 })),
                )
                .await
                .expect("read_process");
            let chunk = payload(&read)["chunk"].as_str().unwrap_or("").to_string();
            if chunk.contains(expected) {
                return chunk;
            }
            assert!(Instant::now() < deadline, "missing {expected:?}: {chunk:?}");
            sleep(Duration::from_millis(40)).await;
        }
    }

    /// End the gateway's stdin, as a client that ends does, and wait for it to exit.
    async fn shut_down(self) -> ExitStatus {
        let Self {
            client,
            mut process,
        } = self;
        client.cancel().await.expect("close the gateway's stdin");
        exited(&mut process).await
    }

    /// Send the gateway `signal` and wait for it to exit.
    async fn signal(self, signal: &str) -> ExitStatus {
        let Self {
            client,
            mut process,
        } = self;
        let pid = process.id().unwrap().to_string();
        let sent = std::process::Command::new("kill")
            .args([signal, &pid])
            .status()
            .unwrap();
        assert!(sent.success());
        let status = exited(&mut process).await;
        drop(client);
        status
    }

    /// SIGKILL: the gateway says nothing to its Runner.
    async fn kill(self) {
        let Self {
            client,
            mut process,
        } = self;
        process.kill().await.expect("SIGKILL the gateway");
        drop(client);
    }
}

async fn exited(process: &mut Child) -> ExitStatus {
    timeout(Duration::from_secs(15), process.wait())
        .await
        .expect("the process exits")
        .expect("wait for the process")
}

/// Whether the count in `beat` still goes up.
async fn still_beating(beat: &Path) -> bool {
    let read = || std::fs::read_to_string(beat).unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while read().trim().is_empty() && Instant::now() < deadline {
        sleep(Duration::from_millis(20)).await;
    }
    let before = read();
    sleep(Duration::from_millis(600)).await;
    before != read()
}

/// A gateway killed while its process runs leaves the process to the Runner: the next gateway
/// reads it and sees it end at its original timeout, not at one restarted when that gateway
/// connected, but starts nothing itself, and its normal shutdown with the intent `stop` then
/// stops the Runner (P1R-C01).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_independent_runner_keeps_a_killed_gateways_process_to_its_original_timeout() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let mut runner = setup.start_runner(&bin).await;
    let first = setup.gateway("detach").await;
    let started = Instant::now();
    let process_id = first
        .started(&["/bin/sh", "-c", "echo started; exec sleep 60"])
        .await;
    first.output(&process_id, "started").await;
    first.kill().await;
    // Long enough that a timeout restarted by the next gateway would come visibly later.
    sleep(Duration::from_secs(3)).await;
    let reconnected = Instant::now();
    let next = setup.gateway("stop").await;
    assert_eq!(next.status(&process_id).await["state"], "running");
    next.output(&process_id, "started").await;
    let refused = next.exec(&["/bin/echo", "new"]).await;
    assert_eq!(error_body(&refused)["code"], "RECOVERY_PENDING");
    let status = loop {
        let status = next.status(&process_id).await;
        if status["state"] == "exited" {
            break status;
        }
        assert!(
            reconnected.elapsed() < TIMEOUT,
            "the process outlived its timeout: {status}"
        );
        sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status["termination"], "timeout", "{status}");
    assert!(started.elapsed() >= TIMEOUT);
    let ended = next.shut_down().await;
    assert!(ended.success(), "{ended:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
    assert!(!setup.socket.exists());
}

/// A gateway's normal shutdown, the end of stdin or SIGTERM, leaves or stops its independent
/// Runner as the gateway's exit intent says, and SIGUSR1, which ends the gateway for a restart,
/// always leaves it (P1R-C01).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_leaves_or_stops_its_independent_runner_as_its_exit_intent_says() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let mut runner = setup.start_runner(&bin).await;
    let beat = setup.ws.join("beat");
    let first = setup.gateway("detach").await;
    first
        .started(&[
            "/bin/sh",
            "-c",
            "i=0; while :; do i=$((i+1)); echo $i > beat; sleep 0.1; done",
        ])
        .await;
    assert!(still_beating(&beat).await);
    let ended = first.shut_down().await;
    assert!(ended.success(), "{ended:?}");
    assert!(still_beating(&beat).await);
    assert!(runner.try_wait().unwrap().is_none());

    let restarted = setup.gateway("stop").await;
    let ended = restarted.signal("-USR1").await;
    assert!(ended.success(), "{ended:?}");
    assert!(still_beating(&beat).await);
    assert!(runner.try_wait().unwrap().is_none());

    let last = setup.gateway("stop").await;
    let ended = last.signal("-TERM").await;
    assert!(ended.success(), "{ended:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
    assert!(!still_beating(&beat).await);
    assert!(!setup.socket.exists());
}

/// A gateway does not start with lifecycle settings that do not fit together, nor on a Runner of
/// the other lifecycle, and a gateway refused by an independent Runner leaves it as it was: the
/// next gateway starts work normally (P1R-C01).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_refuses_independent_settings_or_a_runner_that_do_not_fit() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let socket = setup.socket.display().to_string();
    let uds = ["--runner", "uds", "--runner-socket", &socket];
    for (args, problem) in [
        (
            [&uds[..], &["--runner-lifecycle", "independent"]].concat(),
            "needs --runner-exit-intent",
        ),
        (
            [&uds[..], &["--runner-exit-intent", "stop"]].concat(),
            "only for --runner-lifecycle independent",
        ),
        (
            vec![
                "--runner-lifecycle",
                "independent",
                "--runner-exit-intent",
                "stop",
            ],
            "needs --runner uds",
        ),
    ] {
        let refused = setup.refused(&args).await;
        assert!(refused.contains(problem), "{args:?}: {refused}");
    }

    let mut runner = setup.start_runner(&bin).await;
    let refused = setup.refused(&uds).await;
    assert!(
        refused.contains("is an independent Runner"),
        "a managed gateway: {refused}"
    );
    let next = setup.gateway("stop").await;
    let process_id = next.started(&["/bin/echo", "after"]).await;
    next.output(&process_id, "after").await;
    let ended = next.shut_down().await;
    assert!(ended.success(), "{ended:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");

    // A worker that belongs to its gateway is not an independent Runner.
    let managed_socket = setup.root.path().join("managed.sock");
    let mut managed = Command::new(&bin)
        .arg(&managed_socket)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn a managed worker");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !managed_socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the managed worker does not listen"
        );
        sleep(Duration::from_millis(20)).await;
    }
    let managed_socket = managed_socket.display().to_string();
    let refused = setup
        .refused(&[
            "--runner",
            "uds",
            "--runner-socket",
            &managed_socket,
            "--runner-lifecycle",
            "independent",
            "--runner-exit-intent",
            "detach",
        ])
        .await;
    assert!(
        refused.contains("not an independent Runner"),
        "an independent gateway: {refused}"
    );
    exited(&mut managed).await;
}
