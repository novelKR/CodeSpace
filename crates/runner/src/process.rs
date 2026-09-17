//! Managed workspace processes. Request lifetime is not process lifetime.
//! Spawns host `tokio::process::Command`. Container dispatch is not wired.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codespace_domain::{
    ErrorBody, ErrorCode, ExecCommandParams, ExecCommandResult, ProcessId, ReadProcessParams,
    ReadProcessResult, TerminateProcessParams, WriteStdinParams,
};
use codespace_policy::Workspace;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::task::JoinHandle;

pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_PROCESSES: usize = 8;
pub const DEFAULT_COMPLETED_TTL: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_MAX_COMPLETED: usize = 64;

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

fn process_timeout() -> Duration {
    std::env::var("CODESPACE_PROCESS_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

fn max_processes() -> usize {
    std::env::var("CODESPACE_MAX_PROCESSES")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_PROCESSES)
}

fn child_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into())
}

pub trait Runner: Send + Sync {
    fn exec(
        &self,
        ws: &Workspace,
        process_id: ProcessId,
        params: &ExecCommandParams,
    ) -> Result<ExecCommandResult, ErrorBody>;
    fn write_stdin(
        &self,
        params: WriteStdinParams,
    ) -> impl std::future::Future<Output = Result<(), ErrorBody>> + Send;
    fn read_process(&self, params: ReadProcessParams) -> Result<ReadProcessResult, ErrorBody>;
    fn terminate(&self, params: TerminateProcessParams) -> Result<(), ErrorBody>;
    fn workspace_of(&self, process_id: &str) -> Option<String>;
    fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody>;
}

#[derive(Clone)]
pub struct InProcessRunner {
    inner: Arc<Mutex<HashMap<String, Slot>>>,
    on_release: ShellRelease,
    retention: RetentionPolicy,
}

struct Slot {
    workspace_id: String,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    output: Arc<Mutex<OutputBuf>>,
    completed_at: Arc<Mutex<Option<Instant>>>,
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
}

impl Runner for InProcessRunner {
    fn exec(
        &self,
        ws: &Workspace,
        process_id: ProcessId,
        params: &ExecCommandParams,
    ) -> Result<ExecCommandResult, ErrorBody> {
        if params.command.is_empty() || params.command[0].is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "command must be a non-empty argv (no shell)",
            ));
        }
        let mut child = Command::new(&params.command[0]);
        if params.command.len() > 1 {
            child.args(&params.command[1..]);
        }
        child
            .current_dir(&ws.root)
            .env_clear()
            .env("PATH", child_path())
            .env("HOME", &ws.root)
            .env("LANG", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut spawned = child
            .spawn()
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
        let stdin = spawned.stdin.take();
        let stdout = spawned.stdout.take();
        let stderr = spawned.stderr.take();
        let child = Arc::new(Mutex::new(spawned));
        let output = Arc::new(Mutex::new(OutputBuf::default()));
        let completed_at = Arc::new(Mutex::new(None));
        let slot = Slot {
            workspace_id: params.workspace_id.0.clone(),
            child: child.clone(),
            stdin: Arc::new(Mutex::new(stdin)),
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
            map.insert(process_id.0.clone(), slot);
        }

        let out_handle = stdout.map(|out| {
            let buf = output.clone();
            tokio::spawn(async move { pump_reader(out, buf).await })
        });
        let err_handle = stderr.map(|err| {
            let buf = output.clone();
            tokio::spawn(async move { pump_reader(err, buf).await })
        });

        let wait_child = child.clone();
        let wait_out = output.clone();
        let wait_release = self.on_release.clone();
        let wait_ws = params.workspace_id.0.clone();
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
            wait_release(&wait_ws);
        });

        let timeout_child = child;
        let timeout_out = output;
        tokio::spawn(async move {
            tokio::time::sleep(process_timeout()).await;
            let mut ch = timeout_child.lock().expect("child");
            if ch.try_wait().ok().flatten().is_none() {
                let _ = ch.start_kill();
                if let Ok(mut buf) = timeout_out.lock() {
                    buf.timed_out = true;
                }
            }
        });

        Ok(ExecCommandResult {
            process_id,
            coordination: None,
        })
    }

    async fn write_stdin(&self, params: WriteStdinParams) -> Result<(), ErrorBody> {
        let stdin = {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            let slot = map
                .get(&params.process_id.0)
                .ok_or_else(|| missing(&params.process_id.0))?;
            slot.stdin.clone()
        };
        let mut pipe = stdin
            .lock()
            .expect("stdin")
            .take()
            .ok_or_else(|| ErrorBody::new(ErrorCode::ProcessNotFound, "stdin is closed"))?;
        let result = async {
            pipe.write_all(params.data.as_bytes()).await?;
            pipe.flush().await?;
            Ok::<(), std::io::Error>(())
        }
        .await;
        stdin.lock().expect("stdin").replace(pipe);
        result.map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))
    }

    fn read_process(&self, params: ReadProcessParams) -> Result<ReadProcessResult, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&params.process_id.0)
            .ok_or_else(|| missing(&params.process_id.0))?;
        let buf = slot.output.lock().expect("output");
        if buf.timed_out && params.cursor >= buf.total {
            return Err(ErrorBody::new(
                ErrorCode::Timeout,
                "managed process exceeded time limit",
            ));
        }
        let start = params.cursor.max(buf.dropped);
        let skip = (start - buf.dropped) as usize;
        let chunk = if skip >= buf.bytes.len() {
            Vec::new()
        } else {
            buf.bytes[skip..].to_vec()
        };
        let next = start + chunk.len() as u64;
        Ok(ReadProcessResult {
            process_id: params.process_id,
            cursor: next,
            chunk: String::from_utf8_lossy(&chunk).into_owned(),
            eof: buf.eof && next >= buf.total,
            coordination: None,
        })
    }

    fn terminate(&self, params: TerminateProcessParams) -> Result<(), ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&params.process_id.0)
            .ok_or_else(|| missing(&params.process_id.0))?;
        let mut child = slot.child.lock().expect("child");
        if child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }
        child
            .start_kill()
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
        Ok(())
    }

    fn workspace_of(&self, process_id: &str) -> Option<String> {
        let mut map = self.inner.lock().ok()?;
        self.evict_completed(&mut map);
        map.get(process_id).map(|slot| slot.workspace_id.clone())
    }

    fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let mut killed = 0u32;
        for slot in map.values() {
            if slot.workspace_id != workspace_id {
                continue;
            }
            let eof = slot.output.lock().map(|buf| buf.eof).unwrap_or(true);
            if eof {
                continue;
            }
            let mut child = slot.child.lock().expect("child");
            if child.try_wait().ok().flatten().is_some() {
                continue;
            }
            child
                .start_kill()
                .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
            killed += 1;
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

async fn pump_reader<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    output: Arc<Mutex<OutputBuf>>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let mut out = output.lock().expect("output");
                out.total += n as u64;
                out.bytes.extend_from_slice(&buf[..n]);
                if out.bytes.len() > MAX_OUTPUT_BYTES {
                    let extra = out.bytes.len() - MAX_OUTPUT_BYTES;
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
    use codespace_domain::{ProcessId, Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    fn workspace(root: &std::path::Path) -> Workspace {
        Workspace {
            id: WorkspaceId("demo".into()),
            root: root.to_path_buf(),
            profile: Profile::WorkspaceWrite,
        }
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
        let params = ExecCommandParams {
            workspace_id: WorkspaceId("demo".into()),
            command: vec!["/bin/echo".into(), "hi".into()],
            work_id: None,
        };
        let process_id = ProcessId("proc-ttl".into());
        runner.exec(&ws, process_id.clone(), &params).unwrap();
        for _ in 0..50 {
            if released.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(released.load(Ordering::SeqCst) >= 1);
        runner
            .read_process(ReadProcessParams {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let err = runner
            .read_process(ReadProcessParams {
                process_id,
                cursor: 0,
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessNotFound);
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
                first.clone(),
                &ExecCommandParams {
                    workspace_id: WorkspaceId("demo".into()),
                    command: vec!["/bin/echo".into(), "one".into()],
                    work_id: None,
                },
            )
            .unwrap();
        for _ in 0..50 {
            if runner
                .read_process(ReadProcessParams {
                    process_id: first.clone(),
                    cursor: 0,
                })
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
                second.clone(),
                &ExecCommandParams {
                    workspace_id: WorkspaceId("demo".into()),
                    command: vec!["/bin/echo".into(), "two".into()],
                    work_id: None,
                },
            )
            .unwrap();
        for _ in 0..50 {
            if runner
                .read_process(ReadProcessParams {
                    process_id: second.clone(),
                    cursor: 0,
                })
                .unwrap()
                .eof
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Trigger eviction of the oldest completed slot.
        let _ = runner.read_process(ReadProcessParams {
            process_id: second,
            cursor: 0,
        });
        let err = runner
            .read_process(ReadProcessParams {
                process_id: first,
                cursor: 0,
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessNotFound);
    }
}
