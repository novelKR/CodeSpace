//! The gateway with an independent Runner (P1R-C01), each started as an operator starts it: the
//! Runner outlives a gateway, the next gateway inherits its processes but starts nothing, and a
//! gateway that ends leaves or stops the Runner as its exit intent says. Each gateway records
//! its executions in the operations database, which it is the only writer of (P1R-C02), and
//! takes control of the Runner with a Claim for that database (P1R-C03). The tests with a
//! Runner need the worker binary (`CODESPACE_RUNTIME_BIN`) and return early without it, as the
//! gateway's other worker tests do.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{
    ProcessState, ProcessTermination, TOOL_EXEC_COMMAND, TOOL_PROCESS_STATUS, TOOL_READ_PROCESS,
};
use codespace_runner::{ClaimError, ClaimRefusal, UdsRunner};
use codespace_store::{ProcessOutcome, ProcessPhase, Store, WriterLock};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RunningService, ServiceError};
use rmcp::{object, RoleClient, ServiceExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
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
                "workspaces": {
                    "demo": { "root": ws, "profile": "workspace-write" },
                    "online": { "root": ws, "profile": "workspace-write", "network": "enabled" }
                }
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

    /// The operations database that every gateway here records into.
    fn database(&self) -> PathBuf {
        self.root.path().join("ops.sqlite")
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
            .env("CODESPACE_OPERATIONS_DB", self.database())
            .env(
                "CODESPACE_PROCESS_TIMEOUT_SECS",
                TIMEOUT.as_secs().to_string(),
            )
            .args(args)
            .kill_on_drop(true);
        command
    }

    /// The settings of a gateway connected to the independent Runner, whose normal shutdown
    /// asks `exit_intent` of it.
    fn independent(&self, exit_intent: &str) -> Vec<String> {
        [
            "--runner",
            "uds",
            "--runner-socket",
            &self.socket.display().to_string(),
            "--runner-lifecycle",
            "independent",
            "--runner-exit-intent",
            exit_intent,
        ]
        .map(String::from)
        .to_vec()
    }

    /// A gateway connected to the independent Runner, whose normal shutdown asks `exit_intent`
    /// of it.
    async fn gateway(&self, exit_intent: &str) -> Gateway {
        let args = self.independent(exit_intent);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut process = self
            .gateway_command(&args)
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
        refused(self.gateway_command(args)).await
    }
}

/// The error of a gateway `command` that does not start.
async fn refused(mut command: Command) -> String {
    let output = timeout(
        Duration::from_secs(30),
        command.stdin(Stdio::null()).output(),
    )
    .await
    .expect("the gateway ends")
    .expect("run the gateway");
    assert!(!output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The lines a process writes to `stderr`, read for as long as it writes them.
fn log_lines(stderr: tokio::process::ChildStderr) -> mpsc::UnboundedReceiver<String> {
    let (lines, logged) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut read = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = read.next_line().await {
            let _ = lines.send(line);
        }
    });
    logged
}

/// The first line logged that contains `text`.
async fn logged(lines: &mut mpsc::UnboundedReceiver<String>, text: &str) -> String {
    timeout(Duration::from_secs(15), async {
        loop {
            let line = lines.recv().await.expect("the gateway logs it");
            if line.contains(text) {
                return line;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the gateway does not log {text:?}"))
}

struct Gateway {
    client: RunningService<RoleClient, ()>,
    process: Child,
}

impl Gateway {
    async fn exec(&self, command: &[&str]) -> Result<CallToolResult, ServiceError> {
        self.exec_in("demo", command).await
    }

    async fn exec_in(
        &self,
        workspace_id: &str,
        command: &[&str],
    ) -> Result<CallToolResult, ServiceError> {
        self.client
            .call_tool(
                CallToolRequestParams::new(TOOL_EXEC_COMMAND).with_arguments(object!({
                    "workspace_id": workspace_id,
                    "command": command
                })),
            )
            .await
    }

    /// Start `command` and return its process id.
    async fn started(&self, command: &[&str]) -> String {
        self.started_in("demo", command).await
    }

    async fn started_in(&self, workspace_id: &str, command: &[&str]) -> String {
        let started = self.exec_in(workspace_id, command).await.expect("exec");
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

    /// The process's status once it has exited.
    async fn ended(&self, process_id: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = self.status(process_id).await;
            if status["state"] == "exited" {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the process does not end: {status}"
            );
            sleep(Duration::from_millis(40)).await;
        }
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

/// The parent of the live process `pid`, as `ps` reports it on Linux and macOS.
fn parent_of(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
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

/// A gateway with an independent Runner needs an operations database, and is its only writer
/// (P1R-C02): before it connects to the Runner, it waits a bounded time for the gateway that
/// holds the database's lock, and then refuses to start. A gateway in another mode takes no
/// lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_with_an_independent_runner_is_its_databases_only_writer() {
    let setup = Setup::new();
    let args = setup.independent("detach");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut without = setup.gateway_command(&args);
    without.env_remove("CODESPACE_OPERATIONS_DB");
    let refusal = refused(without).await;
    assert!(refusal.contains("needs --operations-db"), "{refusal}");

    let held = WriterLock::acquire(&setup.database(), Duration::ZERO, |_| {
        unreachable!("nothing holds the lock yet")
    })
    .unwrap();
    let started = Instant::now();
    let mut waiting = setup.gateway_command(&args);
    waiting.env("RUST_LOG", "info");
    let refusal = refused(waiting).await;
    let waited = started.elapsed();
    assert!(refusal.contains("waiting for it to end"), "{refusal}");
    assert!(
        refusal.contains("has another writer")
            && refusal.contains(&format!("process {}", std::process::id())),
        "{refusal}"
    );
    assert!(waited >= Duration::from_secs(15), "{waited:?}");

    // A gateway in another mode records nothing and takes no lock.
    let mut managed = setup
        .gateway_command(&[])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn a managed gateway");
    let stdout = managed.stdout.take().unwrap();
    let stdin = managed.stdin.take().unwrap();
    let client = ().serve((stdout, stdin)).await.expect("initialize");
    let ended = Gateway {
        client,
        process: managed,
    }
    .shut_down()
    .await;
    assert!(ended.success(), "{ended:?}");
    drop(held);
}

/// A gateway started while the gateway before it still ends, as when a client restarts its
/// stdio gateway, waits for it to let go of the operations database and then serves (P1R-C02).
/// The records the gateway before it wrote name each execution, the Runner it was sent to and
/// its root process, and say how it ended or that it still runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_gateway_waits_for_the_one_before_it_and_finds_its_records() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let mut runner = setup.start_runner(&bin).await;
    let first = setup.gateway("detach").await;
    let ended = first.started(&["/bin/sh", "-c", "echo $$; exit 4"]).await;
    first.ended(&ended).await;
    let root_pid = first.output(&ended, "\n").await;
    let running = first
        .started(&["/bin/sh", "-c", "echo started; exec sleep 30"])
        .await;
    first.output(&running, "started").await;

    let args = setup.independent("stop");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut process = setup
        .gateway_command(&args)
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the next gateway");
    let mut lines = log_lines(process.stderr.take().unwrap());
    let first_pid = first.process.id().unwrap();
    let line = logged(&mut lines, "waiting for it to end").await;
    assert!(line.contains(&format!("Some({first_pid})")), "{line}");
    let stdout = process.stdout.take().unwrap();
    let stdin = process.stdin.take().unwrap();
    let serving = tokio::spawn(async move { ().serve((stdout, stdin)).await });
    sleep(Duration::from_millis(500)).await;
    assert!(
        !serving.is_finished(),
        "the next gateway serves while the first one holds the database"
    );
    let shut = first.shut_down().await;
    assert!(shut.success(), "{shut:?}");
    logged(&mut lines, "the only writer of the operations database").await;
    let client = timeout(Duration::from_secs(15), serving)
        .await
        .expect("the next gateway serves")
        .unwrap()
        .expect("initialize");
    let next = Gateway { client, process };
    assert_eq!(next.status(&running).await["state"], "running");

    let store = Store::open(&setup.database()).unwrap();
    let runner_pid = runner.id().unwrap();
    let record = store.process_record(&ended).unwrap().expect("its record");
    assert_eq!(record.phase, ProcessPhase::Terminal, "{record:?}");
    assert_eq!(
        record.outcome,
        Some(ProcessOutcome::Ended(ProcessTermination::Exited))
    );
    assert_eq!(record.exit_code, Some(4));
    assert_eq!(record.runner.pid, runner_pid);
    let root = record.root.expect("its root process");
    // Unsandboxed, the root is the shell itself. The Linux sandbox's root is its helper, and
    // the shell's `$$` is its PID in the sandbox's own PID namespace.
    if !codespace_runner::linux_sandbox_available() {
        assert_eq!(root.pid.to_string(), root_pid.trim());
    }
    let record = store.process_record(&running).unwrap().expect("its record");
    assert_eq!(record.phase, ProcessPhase::Running, "{record:?}");
    assert_eq!(record.runner.pid, runner_pid);
    // The running execution's root is a live process that the Runner created, sandboxed or not.
    let root = record.root.expect("its root process");
    assert_eq!(parent_of(root.pid), Some(runner_pid), "{root:?}");
    let version: i64 = rusqlite::Connection::open(setup.database())
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, codespace_store::SCHEMA_VERSION);

    let shut = next.shut_down().await;
    assert!(shut.success(), "{shut:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
}

/// While a gateway controls the independent Runner, even one stopped with SIGSTOP whose
/// connection stays open, another Claim is refused: nothing pushes a live controller out
/// (P1R-C03). Once that gateway ends, the next Claim is granted after it, states the processes
/// it inherits, and lets a later gateway take over in turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_is_refused_while_a_stopped_gateway_holds_the_runner_and_granted_once_it_ends() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let mut runner = setup.start_runner(&bin).await;
    let first = setup.gateway("detach").await;
    let process_id = first
        .started(&["/bin/sh", "-c", "echo started; exec sleep 30"])
        .await;
    first.output(&process_id, "started").await;
    // The store id is no secret from the same user: it guards against a mistaken gateway.
    let store_id = Store::open(&setup.database()).unwrap().store_id().unwrap();
    let stopped = std::process::Command::new("kill")
        .args(["-STOP", &first.process.id().unwrap().to_string()])
        .status()
        .unwrap();
    assert!(stopped.success());
    let client = UdsRunner::connect(&setup.socket, Arc::new(|_: &str| {}))
        .await
        .unwrap();
    match client.claim(&store_id).await {
        Err(ClaimError::Refused { refusal, message }) => {
            assert_eq!(refusal, ClaimRefusal::ControllerActive, "{message}");
            assert!(message.contains("still open"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    let killed = Instant::now();
    first.kill().await;
    let claim = loop {
        match client.claim(&store_id).await {
            Ok(claim) => break claim,
            // Until the Runner reads the end of the killed gateway's connection.
            Err(ClaimError::Refused {
                refusal: ClaimRefusal::ControllerActive,
                message,
            }) => {
                assert!(killed.elapsed() < Duration::from_secs(10), "{message}");
                sleep(Duration::from_millis(20)).await;
            }
            other => panic!("unexpected {other:?}"),
        }
    };
    eprintln!(
        "p1r-c03 claim latency: granted {:?} after the stopped gateway was killed",
        killed.elapsed()
    );
    assert_eq!((claim.epoch, claim.previous_controller), (2, true));
    assert!(
        claim
            .processes
            .iter()
            .any(|entry| entry.status.process_id.0 == process_id
                && entry.status.state == ProcessState::Running),
        "{claim:?}"
    );
    client.detach().await.unwrap();

    let next = setup.gateway("stop").await;
    assert_eq!(next.status(&process_id).await["state"], "running");
    let refused = next.exec(&["/bin/echo", "new"]).await;
    assert_eq!(error_body(&refused)["code"], "RECOVERY_PENDING");
    let ended = next.shut_down().await;
    assert!(ended.success(), "{ended:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
}

/// A gateway whose operations database is not the one the independent Runner was first
/// claimed for is refused, and leaves the Runner as it was (P1R-C03): the gateway of the
/// Runner's own database then starts work normally, as nothing was started before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_with_another_operations_database_is_refused() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let mut runner = setup.start_runner(&bin).await;
    let first = setup.gateway("detach").await;
    let ended = first.shut_down().await;
    assert!(ended.success(), "{ended:?}");

    let args = setup.independent("stop");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut other = setup.gateway_command(&args);
    other.env(
        "CODESPACE_OPERATIONS_DB",
        setup.root.path().join("other.sqlite"),
    );
    let refusal = refused(other).await;
    assert!(
        refusal.contains("refused this gateway's Claim (store_mismatch)"),
        "{refusal}"
    );
    let next = setup.gateway("stop").await;
    let process_id = next.started(&["/bin/echo", "after"]).await;
    next.output(&process_id, "after").await;
    let ended = next.shut_down().await;
    assert!(ended.success(), "{ended:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
}

/// Connects to the Unix socket named by its argument and says how far it got: whether it sees
/// the socket, then `connected`, or the call that failed and its errno.
const CONNECT: &str = r#"use Socket;
my $path = shift;
print -S $path ? "visible\n" : "invisible\n";
socket(my $s, AF_UNIX, SOCK_STREAM, 0) or do { printf "socket %d\n", $! + 0; exit 0 };
connect($s, pack_sockaddr_un($path)) or do { printf "connect %d\n", $! + 0; exit 0 };
print "connected\n";
"#;

/// A command that the Linux sandbox runs cannot connect to the independent Runner's socket,
/// whether its workspace's network is restricted or enabled, so it cannot take control of the
/// Runner (P1R-C03); the same command run by the test connects. Without the sandbox, which an
/// enabled network needs, a command connects as any process of the same user does: the
/// exposure that the operator documents name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sandboxed_command_cannot_reach_the_runner_socket() {
    let Some(bin) = runtime_bin() else {
        return;
    };
    let setup = Setup::new();
    let mut runner = setup.start_runner(&bin).await;
    let socket = setup.socket.display().to_string();
    let direct = std::process::Command::new("/usr/bin/perl")
        .args(["-e", CONNECT, &socket])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&direct.stdout),
        "visible\nconnected\n",
        "{direct:?}"
    );
    let sandboxed = codespace_runner::linux_sandbox_available();
    let workspaces: &[&str] = if sandboxed {
        &["demo", "online"]
    } else {
        &["demo"]
    };
    let gateway = setup.gateway("stop").await;
    for &workspace_id in workspaces {
        let process_id = gateway
            .started_in(workspace_id, &["/usr/bin/perl", "-e", CONNECT, &socket])
            .await;
        let status = gateway.ended(&process_id).await;
        assert_eq!(status["exit_code"], 0, "{workspace_id}: {status}");
        let output = gateway.output(&process_id, "\n").await;
        let reached = output.lines().last().unwrap_or_default().to_string();
        if sandboxed {
            assert!(
                ["socket 1", "socket 13", "connect 1", "connect 13"].contains(&reached.as_str()),
                "{workspace_id}: {output:?}"
            );
        } else {
            assert_eq!(reached, "connected", "{workspace_id}: {output:?}");
        }
    }
    // The gateway still controls the Runner.
    let process_id = gateway.started(&["/bin/echo", "after"]).await;
    gateway.output(&process_id, "after").await;
    let ended = gateway.shut_down().await;
    assert!(ended.success(), "{ended:?}");
    let status = exited(&mut runner).await;
    assert!(status.success(), "{status:?}");
}
