//! Managed processes. Request lifetime is not process lifetime: dropping an
//! HTTP request does not kill the child (`kill_on_drop` stays false).

use std::collections::HashMap;
use std::sync::Arc;

use codespace_domain::{
    ErrorBody, ErrorCode, ExecCommandParams, ExecCommandResult, ProcessId, ReadProcessParams,
    ReadProcessResult, TerminateProcessParams, TerminateProcessResult, WriteStdinParams,
    WriteStdinResult,
};
use codespace_policy::{Action, ClientClaims, Registry};
use codespace_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_LIVE_PROCESSES: usize = 4;

struct Managed {
    workspace_id: String,
    stdin: Option<ChildStdin>,
    child: Option<Child>,
    output: Vec<u8>,
    eof: bool,
}

#[derive(Clone)]
pub struct ProcessSupervisor {
    inner: Arc<Mutex<HashMap<String, Managed>>>,
    store: Arc<Store>,
}

impl ProcessSupervisor {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            store,
        }
    }

    pub async fn exec(
        &self,
        registry: &Registry,
        params: ExecCommandParams,
    ) -> Result<ExecCommandResult, ErrorBody> {
        if params.command.is_empty() || params.command[0].is_empty() {
            return Err(ErrorBody::new(ErrorCode::InvalidPatch, "empty command"));
        }
        let ws = registry.get(&params.workspace_id.0)?;
        codespace_policy::allow(ws, Action::Exec, &ClientClaims::default())?;
        {
            let guard = self.inner.lock().await;
            let live = guard.values().filter(|proc| !proc.eof).count();
            if live >= MAX_LIVE_PROCESSES {
                return Err(ErrorBody::new(
                    ErrorCode::WorkspaceBusy,
                    "too many live processes",
                ));
            }
        }
        let id = format!("proc-{}", uuid::Uuid::new_v4());
        self.store.mark_shell_busy(&params.workspace_id.0, &id)?;

        let mut cmd = tokio::process::Command::new(&params.command[0]);
        if params.command.len() > 1 {
            cmd.args(&params.command[1..]);
        }
        cmd.current_dir(&ws.root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                self.store.clear_shell(&params.workspace_id.0, &id);
                return Err(ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()));
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        {
            let mut guard = self.inner.lock().await;
            guard.insert(
                id.clone(),
                Managed {
                    workspace_id: params.workspace_id.0.clone(),
                    stdin,
                    child: Some(child),
                    output: Vec::new(),
                    eof: false,
                },
            );
        }
        if let Some(stdout) = stdout {
            self.spawn_pump(id.clone(), stdout);
        }
        if let Some(stderr) = stderr {
            self.spawn_pump(id.clone(), stderr);
        }
        let timeout = params.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let supervisor = self.clone();
        let timeout_id = id.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(timeout)).await;
            let _ = supervisor
                .terminate(TerminateProcessParams {
                    process_id: ProcessId(timeout_id),
                })
                .await;
        });
        let supervisor = self.clone();
        let wait_id = id.clone();
        tokio::spawn(async move {
            supervisor.wait_exit(&wait_id).await;
        });
        Ok(ExecCommandResult {
            process_id: ProcessId(id),
        })
    }

    fn spawn_pump<R>(&self, id: String, mut reader: R)
    where
        R: AsyncReadExt + Unpin + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut guard = inner.lock().await;
                        if let Some(proc) = guard.get_mut(&id) {
                            let room = MAX_OUTPUT_BYTES.saturating_sub(proc.output.len());
                            if room > 0 {
                                proc.output.extend_from_slice(&buf[..n.min(room)]);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    async fn wait_exit(&self, id: &str) {
        // Poll `try_wait` so `terminate` can still `start_kill` the same Child.
        // Taking the child out would make cancel a no-op.
        loop {
            let finished = {
                let mut guard = self.inner.lock().await;
                let Some(proc) = guard.get_mut(id) else {
                    return;
                };
                if proc.eof {
                    let _ = proc
                        .child
                        .as_mut()
                        .and_then(|c| c.try_wait().ok().flatten());
                    return;
                }
                match proc.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(_)) | Err(_) => true,
                        Ok(None) => false,
                    },
                    None => false,
                }
            };
            if finished {
                let mut guard = self.inner.lock().await;
                if let Some(proc) = guard.get_mut(id) {
                    proc.eof = true;
                    proc.stdin = None;
                    let ws = proc.workspace_id.clone();
                    drop(guard);
                    self.store.clear_shell(&ws, id);
                }
                return;
            }
            sleep(Duration::from_millis(15)).await;
        }
    }

    pub async fn write_stdin(
        &self,
        params: WriteStdinParams,
    ) -> Result<WriteStdinResult, ErrorBody> {
        let stdin = {
            let mut guard = self.inner.lock().await;
            let proc = guard
                .get_mut(&params.process_id.0)
                .ok_or_else(|| ErrorBody::new(ErrorCode::ProcessNotFound, "unknown process_id"))?;
            if proc.eof {
                return Err(ErrorBody::new(
                    ErrorCode::ProcessNotFound,
                    "process stdin is closed",
                ));
            }
            proc.stdin.take()
        };
        if let Some(mut stdin) = stdin {
            let result = stdin
                .write_all(params.data.as_bytes())
                .await
                .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()));
            let mut guard = self.inner.lock().await;
            if let Some(proc) = guard.get_mut(&params.process_id.0) {
                proc.stdin = Some(stdin);
            }
            result?;
        }
        Ok(WriteStdinResult {
            process_id: params.process_id,
        })
    }

    pub async fn read(&self, params: ReadProcessParams) -> Result<ReadProcessResult, ErrorBody> {
        let guard = self.inner.lock().await;
        let proc = guard
            .get(&params.process_id.0)
            .ok_or_else(|| ErrorBody::new(ErrorCode::ProcessNotFound, "unknown process_id"))?;
        let start = params.cursor as usize;
        let slice = if start >= proc.output.len() {
            &[][..]
        } else {
            &proc.output[start..]
        };
        Ok(ReadProcessResult {
            process_id: params.process_id,
            cursor: (start + slice.len()) as u64,
            chunk: String::from_utf8_lossy(slice).into_owned(),
            eof: proc.eof,
        })
    }

    pub async fn terminate(
        &self,
        params: TerminateProcessParams,
    ) -> Result<TerminateProcessResult, ErrorBody> {
        let mut guard = self.inner.lock().await;
        let proc = guard
            .get_mut(&params.process_id.0)
            .ok_or_else(|| ErrorBody::new(ErrorCode::ProcessNotFound, "unknown process_id"))?;
        if let Some(child) = proc.child.as_mut() {
            let _ = child.start_kill();
        }
        proc.eof = true;
        let ws = proc.workspace_id.clone();
        let pid = params.process_id.0.clone();
        drop(guard);
        self.store.clear_shell(&ws, &pid);
        Ok(TerminateProcessResult {
            process_id: params.process_id,
            terminated: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use codespace_domain::{
        ExecCommandParams, ProcessId, Profile, ReadProcessParams, TerminateProcessParams,
        WorkspaceId, WriteStdinParams,
    };
    use codespace_policy::{Registry, Workspace};
    use codespace_store::Store;
    use tempfile::tempdir;

    use super::{ProcessSupervisor, MAX_OUTPUT_BYTES};

    fn write_registry(root: &std::path::Path) -> Registry {
        let mut registry = Registry::new();
        registry.insert(Workspace {
            id: WorkspaceId("demo".into()),
            root: root.to_path_buf(),
            profile: Profile::WorkspaceWrite,
        });
        registry
    }

    #[tokio::test]
    async fn timeout_of_old_process_does_not_clear_newer_shell() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let supervisor = ProcessSupervisor::new(store.clone());
        let registry = write_registry(dir.path());

        let first = supervisor
            .exec(
                &registry,
                ExecCommandParams {
                    workspace_id: WorkspaceId("demo".into()),
                    command: vec!["/bin/sleep".into(), "30".into()],
                    timeout_ms: Some(30_000),
                },
            )
            .await
            .unwrap();
        supervisor
            .terminate(TerminateProcessParams {
                process_id: first.process_id.clone(),
            })
            .await
            .unwrap();

        let second = supervisor
            .exec(
                &registry,
                ExecCommandParams {
                    workspace_id: WorkspaceId("demo".into()),
                    command: vec!["/bin/sleep".into(), "30".into()],
                    timeout_ms: Some(30_000),
                },
            )
            .await
            .unwrap();

        let _ = supervisor
            .terminate(TerminateProcessParams {
                process_id: first.process_id,
            })
            .await;
        assert!(
            store.try_acquire_write("demo").is_err(),
            "newer shell must still hold the busy flag"
        );
        supervisor
            .terminate(TerminateProcessParams {
                process_id: second.process_id,
            })
            .await
            .unwrap();
        assert!(store.try_acquire_write("demo").is_ok());
    }

    #[tokio::test]
    async fn output_is_capped_and_cursor_advances() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let supervisor = ProcessSupervisor::new(store);
        let registry = write_registry(dir.path());
        let started = supervisor
            .exec(
                &registry,
                ExecCommandParams {
                    workspace_id: WorkspaceId("demo".into()),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "dd if=/dev/zero bs=1024 count=200 2>/dev/null | tr '\\0' 'A'".into(),
                    ],
                    timeout_ms: Some(5_000),
                },
            )
            .await
            .unwrap();

        let mut cursor = 0u64;
        let mut total = 0usize;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        loop {
            let chunk = supervisor
                .read(ReadProcessParams {
                    process_id: started.process_id.clone(),
                    cursor,
                })
                .await
                .unwrap();
            total += chunk.chunk.len();
            cursor = chunk.cursor;
            if chunk.eof || tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(total <= MAX_OUTPUT_BYTES);
        assert!(total > 0);
        let _ = supervisor
            .terminate(TerminateProcessParams {
                process_id: started.process_id,
            })
            .await;
    }

    #[tokio::test]
    async fn invented_process_id_is_rejected() {
        let store = Arc::new(Store::memory().unwrap());
        let supervisor = ProcessSupervisor::new(store);
        let err = supervisor
            .read(ReadProcessParams {
                process_id: ProcessId("proc-invented".into()),
                cursor: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, codespace_domain::ErrorCode::ProcessNotFound);
        let err = supervisor
            .write_stdin(WriteStdinParams {
                process_id: ProcessId("proc-invented".into()),
                data: "x".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, codespace_domain::ErrorCode::ProcessNotFound);
    }
}
