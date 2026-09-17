//! Managed workspace processes. Request lifetime is not process lifetime.
//! Pipe spawn uses host `tokio::process::Command`. `tty: true` uses the
//! isolated `codespace-pty` adapter. UDS dispatch lives in `UdsRunner`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codespace_domain::{ErrorBody, ErrorCode, ProcessId};
use codespace_policy::Workspace;
use codespace_pty::PtySession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{
    runner_local_exec_env, RunnerCwd, RunnerExecRequest, RunnerExecResult, RunnerReadProcess,
    RunnerReadResult, RunnerWriteStdin,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_PROCESSES: usize = 8;
pub const DEFAULT_COMPLETED_TTL: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_MAX_COMPLETED: usize = 64;

/// Called with a server-minted `process_id` when that process exits.
pub type ShellRelease = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    pub ttl: Duration,
    pub max_completed: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            ttl: DEFAULT_COMPLETED_TTL,
            max_completed: DEFAULT_MAX_COMPLETED,
        }
    }
}

fn max_processes() -> usize {
    std::env::var("CODESPACE_MAX_PROCESSES")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_PROCESSES)
}

#[derive(Clone)]
pub struct InProcessRunner {
    inner: Arc<Mutex<HashMap<String, Slot>>>,
    on_release: ShellRelease,
    retention: RetentionPolicy,
}

struct Slot {
    workspace_id: String,
    io: SessionIo,
    output: Arc<Mutex<OutputBuf>>,
    completed_at: Arc<Mutex<Option<Instant>>>,
}

enum SessionIo {
    Pipe {
        child: Arc<Mutex<Child>>,
        stdin: Arc<Mutex<Option<ChildStdin>>>,
    },
    Pty {
        session: Arc<PtySession>,
        writer: mpsc::Sender<Vec<u8>>,
    },
}

struct SpawnCtx {
    cwd: PathBuf,
    cap: usize,
    timeout: Duration,
    output: Arc<Mutex<OutputBuf>>,
    completed_at: Arc<Mutex<Option<Instant>>>,
    process_id: String,
}

#[derive(Default)]
struct OutputBuf {
    dropped: u64,
    total: u64,
    bytes: Vec<u8>,
    eof: bool,
    timed_out: bool,
}

impl InProcessRunner {
    pub fn new(on_release: ShellRelease) -> Self {
        Self::with_retention(on_release, RetentionPolicy::default())
    }

    pub fn with_retention(on_release: ShellRelease, retention: RetentionPolicy) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            on_release,
            retention,
        }
    }

    fn evict_completed(&self, map: &mut HashMap<String, Slot>) {
        let now = Instant::now();
        let ttl = self.retention.ttl;
        map.retain(|_, slot| {
            let completed = slot.completed_at.lock().ok().and_then(|guard| *guard);
            !matches!(completed, Some(at) if now.duration_since(at) > ttl)
        });
        let mut completed: Vec<(String, Instant)> = map
            .iter()
            .filter_map(|(id, slot)| {
                slot.completed_at
                    .lock()
                    .ok()
                    .and_then(|guard| guard.map(|at| (id.clone(), at)))
            })
            .collect();
        if completed.len() <= self.retention.max_completed {
            return;
        }
        completed.sort_by_key(|(_, at)| *at);
        let drop_n = completed.len() - self.retention.max_completed;
        for (id, _) in completed.into_iter().take(drop_n) {
            map.remove(&id);
        }
    }

    pub async fn spawn_host(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, ErrorBody> {
        ws.require_exec()?;
        if req.argv.is_empty() || req.argv[0].is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "command must be a non-empty argv (no shell)",
            ));
        }
        let cap = req.output_bytes_cap.max(1) as usize;
        let timeout = Duration::from_millis(req.timeout_ms.max(1));
        let cwd = match req.cwd {
            RunnerCwd::WorkspaceRoot => ws.root.clone(),
        };
        let ctx = SpawnCtx {
            cwd,
            cap,
            timeout,
            output: Arc::new(Mutex::new(OutputBuf::default())),
            completed_at: Arc::new(Mutex::new(None)),
            process_id: req.process_id.0.clone(),
        };
        if req.tty {
            self.spawn_pty(ws, req, ctx).await
        } else {
            self.spawn_pipe(ws, req, ctx)
        }
    }

    fn spawn_pipe(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        ctx: SpawnCtx,
    ) -> Result<RunnerExecResult, ErrorBody> {
        let SpawnCtx {
            cwd,
            cap,
            timeout,
            output,
            completed_at,
            process_id,
        } = ctx;
        let mut child = Command::new(&req.argv[0]);
        if req.argv.len() > 1 {
            child.args(&req.argv[1..]);
        }
        child
            .current_dir(&cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if req.env.use_runner_defaults {
            for (key, value) in runner_local_exec_env(&cwd) {
                child.env(key, value);
            }
        }
        for (key, value) in &req.env.overrides {
            child.env(key, value);
        }
        let mut spawned = child
            .spawn()
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
        let stdin = spawned.stdin.take();
        let stdout = spawned.stdout.take();
        let stderr = spawned.stderr.take();
        let child = Arc::new(Mutex::new(spawned));
        let slot = Slot {
            workspace_id: ws.id.0.clone(),
            io: SessionIo::Pipe {
                child: child.clone(),
                stdin: Arc::new(Mutex::new(stdin)),
            },
            output: output.clone(),
            completed_at: completed_at.clone(),
        };
        {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            if live_count(&map) >= max_processes() {
                let _ = child.lock().expect("child").start_kill();
                return Err(ErrorBody::new(
                    ErrorCode::WorkspaceBusy,
                    "live process limit reached",
                ));
            }
            map.insert(process_id.clone(), slot);
        }

        let out_handle = stdout.map(|out| {
            let buf = output.clone();
            tokio::spawn(async move { pump_reader(out, buf, cap).await })
        });
        let err_handle = stderr.map(|err| {
            let buf = output.clone();
            tokio::spawn(async move { pump_reader(err, buf, cap).await })
        });

        let wait_child = child.clone();
        let wait_out = output.clone();
        let wait_release = self.on_release.clone();
        let wait_process = process_id;
        tokio::spawn(async move {
            reap_child(wait_child).await;
            join_pump(out_handle).await;
            join_pump(err_handle).await;
            if let Ok(mut buf) = wait_out.lock() {
                buf.eof = true;
            }
            if let Ok(mut done) = completed_at.lock() {
                *done = Some(Instant::now());
            }
            wait_release(&wait_process);
        });

        let timeout_child = child;
        let timeout_out = output;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let mut ch = timeout_child.lock().expect("child");
            if ch.try_wait().ok().flatten().is_none() {
                let _ = ch.start_kill();
                if let Ok(mut buf) = timeout_out.lock() {
                    buf.timed_out = true;
                }
            }
        });

        Ok(RunnerExecResult {
            process_id: req.process_id,
        })
    }

    async fn spawn_pty(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        ctx: SpawnCtx,
    ) -> Result<RunnerExecResult, ErrorBody> {
        let SpawnCtx {
            cwd,
            cap,
            timeout,
            output,
            completed_at,
            process_id,
        } = ctx;
        let mut env = HashMap::new();
        if req.env.use_runner_defaults {
            for (key, value) in runner_local_exec_env(&cwd) {
                env.insert(key, value);
            }
            env.insert("TERM".into(), "xterm".into());
        }
        for (key, value) in &req.env.overrides {
            env.insert(key.clone(), value.clone());
        }
        let args = if req.argv.len() > 1 {
            req.argv[1..].to_vec()
        } else {
            Vec::new()
        };
        let mut session = codespace_pty::spawn(&req.argv[0], &args, &cwd, &env)
            .await
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err))?;
        let stdout = session
            .take_stdout()
            .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidPatch, "PTY stdout missing"))?;
        let exit = session
            .take_exit()
            .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidPatch, "PTY exit missing"))?;
        let writer = session.writer();
        let session = Arc::new(session);
        let slot = Slot {
            workspace_id: ws.id.0.clone(),
            io: SessionIo::Pty {
                session: session.clone(),
                writer,
            },
            output: output.clone(),
            completed_at: completed_at.clone(),
        };
        {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            if live_count(&map) >= max_processes() {
                session.kill();
                return Err(ErrorBody::new(
                    ErrorCode::WorkspaceBusy,
                    "live process limit reached",
                ));
            }
            map.insert(process_id.clone(), slot);
        }

        let out_handle = {
            let buf = output.clone();
            Some(tokio::spawn(
                async move { pump_chunks(stdout, buf, cap).await },
            ))
        };
        let wait_out = output.clone();
        let wait_release = self.on_release.clone();
        let wait_process = process_id;
        tokio::spawn(async move {
            let _ = exit.await;
            join_pump(out_handle).await;
            if let Ok(mut buf) = wait_out.lock() {
                buf.eof = true;
            }
            if let Ok(mut done) = completed_at.lock() {
                *done = Some(Instant::now());
            }
            wait_release(&wait_process);
        });

        let timeout_session = session;
        let timeout_out = output;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if !timeout_session.has_exited() {
                timeout_session.kill();
                if let Ok(mut buf) = timeout_out.lock() {
                    buf.timed_out = true;
                }
            }
        });

        Ok(RunnerExecResult {
            process_id: req.process_id,
        })
    }

    pub async fn write_host_stdin(&self, req: RunnerWriteStdin) -> Result<(), ErrorBody> {
        enum WriteTarget {
            Pipe(Arc<Mutex<Option<ChildStdin>>>),
            Pty(mpsc::Sender<Vec<u8>>),
        }
        let target = {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            let slot = map
                .get(&req.process_id.0)
                .ok_or_else(|| missing(&req.process_id.0))?;
            match &slot.io {
                SessionIo::Pipe { stdin, .. } => WriteTarget::Pipe(stdin.clone()),
                SessionIo::Pty { writer, .. } => WriteTarget::Pty(writer.clone()),
            }
        };
        match target {
            WriteTarget::Pipe(stdin) => {
                let mut pipe =
                    stdin.lock().expect("stdin").take().ok_or_else(|| {
                        ErrorBody::new(ErrorCode::ProcessNotFound, "stdin is closed")
                    })?;
                let result = async {
                    pipe.write_all(req.data.as_bytes()).await?;
                    pipe.flush().await?;
                    Ok::<(), std::io::Error>(())
                }
                .await;
                stdin.lock().expect("stdin").replace(pipe);
                result.map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))
            }
            WriteTarget::Pty(writer) => writer
                .send(req.data.into_bytes())
                .await
                .map_err(|_| ErrorBody::new(ErrorCode::ProcessNotFound, "stdin is closed")),
        }
    }

    pub fn read_host_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&req.process_id.0)
            .ok_or_else(|| missing(&req.process_id.0))?;
        let buf = slot.output.lock().expect("output");
        if buf.timed_out && req.cursor >= buf.total {
            return Err(ErrorBody::new(
                ErrorCode::Timeout,
                "managed process exceeded time limit",
            ));
        }
        let start = req.cursor.max(buf.dropped);
        let skip = (start - buf.dropped) as usize;
        let chunk = if skip >= buf.bytes.len() {
            Vec::new()
        } else {
            buf.bytes[skip..].to_vec()
        };
        let next = start + chunk.len() as u64;
        Ok(RunnerReadResult {
            process_id: req.process_id,
            cursor: next,
            chunk: String::from_utf8_lossy(&chunk).into_owned(),
            eof: buf.eof && next >= buf.total,
        })
    }

    pub fn kill_host(&self, process_id: &ProcessId) -> Result<(), ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&process_id.0)
            .ok_or_else(|| missing(&process_id.0))?;
        request_kill(slot)?;
        Ok(())
    }

    pub fn host_workspace_of(&self, process_id: &str) -> Option<String> {
        let mut map = self.inner.lock().ok()?;
        self.evict_completed(&mut map);
        map.get(process_id).map(|slot| slot.workspace_id.clone())
    }

    pub fn kill_host_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let mut killed = 0u32;
        for slot in map.values() {
            if slot.workspace_id != workspace_id {
                continue;
            }
            if request_kill(slot)? {
                killed += 1;
            }
        }
        Ok(killed)
    }
}

async fn reap_child(child: Arc<Mutex<Child>>) {
    loop {
        {
            let mut ch = child.lock().expect("child");
            if ch.try_wait().ok().flatten().is_some() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn join_pump(handle: Option<JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.await;
    }
}

fn request_kill(slot: &Slot) -> Result<bool, ErrorBody> {
    match &slot.io {
        SessionIo::Pipe { child, .. } => {
            let mut child = child.lock().expect("child");
            if child.try_wait().ok().flatten().is_some() {
                return Ok(false);
            }
            child
                .start_kill()
                .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
            Ok(true)
        }
        SessionIo::Pty { session, .. } => {
            if session.has_exited() {
                return Ok(false);
            }
            session.kill();
            Ok(true)
        }
    }
}

async fn pump_chunks(mut rx: mpsc::Receiver<Vec<u8>>, output: Arc<Mutex<OutputBuf>>, cap: usize) {
    while let Some(chunk) = rx.recv().await {
        let mut out = output.lock().expect("output");
        out.total += chunk.len() as u64;
        out.bytes.extend_from_slice(&chunk);
        if out.bytes.len() > cap {
            let extra = out.bytes.len() - cap;
            out.bytes.drain(..extra);
            out.dropped += extra as u64;
        }
    }
}

async fn pump_reader<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    output: Arc<Mutex<OutputBuf>>,
    cap: usize,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let mut out = output.lock().expect("output");
                out.total += n as u64;
                out.bytes.extend_from_slice(&buf[..n]);
                if out.bytes.len() > cap {
                    let extra = out.bytes.len() - cap;
                    out.bytes.drain(..extra);
                    out.dropped += extra as u64;
                }
            }
            Err(_) => break,
        }
    }
}

fn live_count(map: &HashMap<String, Slot>) -> usize {
    map.values()
        .filter(|slot| slot.output.lock().map(|buf| !buf.eof).unwrap_or(false))
        .count()
}

fn missing(id: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::ProcessNotFound,
        format!("unknown process_id `{id}`"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Runner;
    use codespace_domain::{ProcessId, Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    fn isatty_argv() -> Vec<String> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "if [ -t 0 ]; then echo ISATTY; else echo NOTTY; fi".into(),
        ]
    }

    async fn wait_chunk(runner: &InProcessRunner, process_id: &ProcessId) -> String {
        let mut chunk = String::new();
        for _ in 0..50 {
            let result = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            chunk = result.chunk;
            if result.eof {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        chunk
    }

    fn workspace(root: &std::path::Path) -> Workspace {
        Workspace::new(
            WorkspaceId("demo".into()),
            root.to_path_buf(),
            Profile::WorkspaceWrite,
        )
    }

    #[tokio::test]
    async fn completed_handles_evict_after_ttl() {
        let dir = tempdir().unwrap();
        let released = Arc::new(AtomicUsize::new(0));
        let flag = released.clone();
        let runner = InProcessRunner::with_retention(
            Arc::new(move |_| {
                flag.fetch_add(1, Ordering::SeqCst);
            }),
            RetentionPolicy {
                ttl: Duration::from_millis(150),
                max_completed: 64,
            },
        );
        let ws = workspace(dir.path());
        let process_id = ProcessId("proc-ttl".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "hi".into()],
                    process_id.clone(),
                    codespace_domain::Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if released.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(released.load(Ordering::SeqCst) >= 1);
        runner
            .read_process(RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let err = runner
            .read_process(RunnerReadProcess {
                process_id,
                cursor: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotFound)
        );
    }

    #[tokio::test]
    async fn completed_handles_evict_by_max_count() {
        let dir = tempdir().unwrap();
        let runner = InProcessRunner::with_retention(
            Arc::new(|_| {}),
            RetentionPolicy {
                ttl: Duration::from_secs(60),
                max_completed: 1,
            },
        );
        let ws = workspace(dir.path());
        let first = ProcessId("proc-old".into());
        let second = ProcessId("proc-new".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "one".into()],
                    first.clone(),
                    codespace_domain::Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if runner
                .read_process(RunnerReadProcess {
                    process_id: first.clone(),
                    cursor: 0,
                })
                .await
                .unwrap()
                .eof
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "two".into()],
                    second.clone(),
                    codespace_domain::Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if runner
                .read_process(RunnerReadProcess {
                    process_id: second.clone(),
                    cursor: 0,
                })
                .await
                .unwrap()
                .eof
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Trigger eviction of the oldest completed slot.
        let _ = runner
            .read_process(RunnerReadProcess {
                process_id: second,
                cursor: 0,
            })
            .await;
        let err = runner
            .read_process(RunnerReadProcess {
                process_id: first,
                cursor: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotFound)
        );
    }

    #[tokio::test]
    async fn linux_container_environment_is_closed_failure() {
        let dir = tempdir().unwrap();
        let mut ws = workspace(dir.path());
        ws.environment_kind = codespace_policy::EnvironmentKind::LinuxContainer;
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let err = runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into()],
                    ProcessId("proc-box".into()),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::Unauthorized)
        );
    }

    #[tokio::test]
    async fn tty_true_stdin_is_a_tty() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-tty".into());
        let mut req =
            RunnerExecRequest::for_host(isatty_argv(), process_id.clone(), Profile::WorkspaceWrite);
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        let chunk = wait_chunk(&runner, &process_id).await;
        assert!(
            chunk.contains("ISATTY"),
            "tty:true should see a TTY, got {chunk:?}"
        );
        assert!(!chunk.contains("NOTTY"), "chunk={chunk:?}");
    }

    #[tokio::test]
    async fn tty_false_stdin_is_a_pipe() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pipe".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    isatty_argv(),
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        let chunk = wait_chunk(&runner, &process_id).await;
        assert!(
            chunk.contains("NOTTY"),
            "omitted/false tty should be a pipe, got {chunk:?}"
        );
    }

    #[tokio::test]
    async fn tty_write_stdin_roundtrip() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pty-io".into());
        let mut req = RunnerExecRequest::for_host(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "IFS= read -r line; printf 'got:%s\\n' \"$line\"".into(),
            ],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        runner
            .write_stdin(RunnerWriteStdin {
                process_id: process_id.clone(),
                data: "hello\n".into(),
            })
            .await
            .unwrap();
        let chunk = wait_chunk(&runner, &process_id).await;
        assert!(
            chunk.contains("hello"),
            "PTY write_stdin/read_process roundtrip, got {chunk:?}"
        );
    }
}
