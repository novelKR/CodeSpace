//! Managed workspace processes. Request lifetime is not process lifetime.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codespace_domain::{
    ErrorBody, ErrorCode, ExecCommandParams, ExecCommandResult, ProcessId, ReadProcessParams,
    ReadProcessResult, TerminateProcessParams, WriteStdinParams,
};
use codespace_policy::{allow, Action, ClientClaims, Registry};
use codespace_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_PROCESSES: usize = 8;

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

#[derive(Clone)]
pub struct Supervisor {
    store: Arc<Store>,
    inner: Arc<Mutex<HashMap<String, Slot>>>,
}

struct Slot {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    output: Arc<Mutex<OutputBuf>>,
}

#[derive(Default)]
struct OutputBuf {
    dropped: u64,
    total: u64,
    bytes: Vec<u8>,
    eof: bool,
    timed_out: bool,
}

impl Supervisor {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn exec(
        &self,
        registry: &Registry,
        params: ExecCommandParams,
    ) -> Result<ExecCommandResult, ErrorBody> {
        if params.command.is_empty() || params.command[0].is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "command must be a non-empty argv (no shell)",
            ));
        }
        let ws = registry.get(&params.workspace_id.0)?;
        allow(ws, Action::Exec, &ClientClaims::default())?;
        let process_id = ProcessId(format!("proc-{}", Uuid::new_v4()));
        self.store
            .mark_shell_busy(&params.workspace_id.0, &process_id.0)?;

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
            // Dropping an HTTP session clone must not kill the child. The
            // shared supervisor holds the Child until timeout, terminate, or
            // exit. Server shutdown still drops that table.
            .kill_on_drop(true);
        let mut spawned = child.spawn().map_err(|err| {
            self.store.clear_shell(&params.workspace_id.0);
            ErrorBody::new(ErrorCode::InvalidPatch, err.to_string())
        })?;
        let stdin = spawned.stdin.take();
        let stdout = spawned.stdout.take();
        let stderr = spawned.stderr.take();
        let child = Arc::new(Mutex::new(spawned));
        let output = Arc::new(Mutex::new(OutputBuf::default()));
        let slot = Slot {
            child: child.clone(),
            stdin: Arc::new(Mutex::new(stdin)),
            output: output.clone(),
        };
        {
            let mut map = self.inner.lock().expect("supervisor");
            if live_count(&map) >= max_processes() {
                let _ = child.lock().expect("child").start_kill();
                self.store.clear_shell(&params.workspace_id.0);
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
        let wait_store = self.store.clone();
        let wait_ws = params.workspace_id.0.clone();
        tokio::spawn(async move {
            reap_child(wait_child).await;
            join_pump(out_handle).await;
            join_pump(err_handle).await;
            if let Ok(mut buf) = wait_out.lock() {
                buf.eof = true;
            }
            wait_store.clear_shell(&wait_ws);
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

        Ok(ExecCommandResult { process_id })
    }

    pub async fn write_stdin(&self, params: WriteStdinParams) -> Result<(), ErrorBody> {
        let stdin = {
            let map = self.inner.lock().expect("supervisor");
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

    pub fn read(&self, params: ReadProcessParams) -> Result<ReadProcessResult, ErrorBody> {
        let map = self.inner.lock().expect("supervisor");
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
        })
    }

    pub fn terminate(&self, params: TerminateProcessParams) -> Result<(), ErrorBody> {
        let map = self.inner.lock().expect("supervisor");
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
