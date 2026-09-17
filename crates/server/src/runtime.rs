//! Gateway-owned Unix worker. One child, one private dir, one socket.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use codespace_runner::{allocate_private_runner_dir, runner_socket_path, ShellRelease, UdsRunner};
use codespace_store::Store;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::Notify;

pub struct RuntimeProcess {
    pid: u32,
    dir: PathBuf,
    socket: PathBuf,
    shutdown: Arc<Notify>,
    wait: Option<tokio::task::JoinHandle<()>>,
}

impl RuntimeProcess {
    pub async fn spawn(
        bin: &Path,
        runner_dir: Option<&Path>,
        on_process_exit: ShellRelease,
        store: Arc<Store>,
    ) -> Result<(Self, UdsRunner)> {
        let dir = allocate_private_runner_dir(runner_dir)
            .map_err(|err| anyhow!("private runner dir: {err}"))?;
        let socket = runner_socket_path(&dir);
        let mut child = Command::new(bin)
            .arg(&socket)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {}", bin.display()))?;
        let pid = child
            .id()
            .ok_or_else(|| anyhow!("worker pid missing after spawn"))?;
        let shutdown = Arc::new(Notify::new());
        let wait_shutdown = shutdown.clone();
        let wait_store = store.clone();
        let wait = tokio::spawn(async move {
            tokio::select! {
                _ = child.wait() => {}
                _ = wait_shutdown.notified() => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            }
            wait_store.release_all_processes();
        });
        let stream = wait_for_socket(&socket, pid).await?;
        let kill = shutdown.clone();
        let runner = UdsRunner::from_stream_with_disconnect(
            stream,
            on_process_exit,
            Arc::new(move || {
                kill.notify_one();
            }),
        );
        runner
            .handshake()
            .await
            .map_err(|err| anyhow!("runner hello: {err}"))?;
        Ok((
            Self {
                pid,
                dir,
                socket,
                shutdown,
                wait: Some(wait),
            },
            runner,
        ))
    }

    pub async fn connect_existing(
        socket: &Path,
        on_process_exit: ShellRelease,
        store: Arc<Store>,
    ) -> Result<UdsRunner> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect {}", socket.display()))?;
        let runner = UdsRunner::from_stream_with_disconnect(
            stream,
            on_process_exit,
            Arc::new(move || {
                store.release_all_processes();
            }),
        );
        runner
            .handshake()
            .await
            .map_err(|err| anyhow!("runner hello: {err}"))?;
        Ok(runner)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub async fn wait_exit(&mut self) {
        self.shutdown.notify_one();
        kill_pid(self.pid);
        if let Some(wait) = self.wait.take() {
            let _ = wait.await;
        }
    }
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        self.shutdown.notify_one();
        kill_pid(self.pid);
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn wait_for_socket(socket: &Path, pid: u32) -> Result<UnixStream> {
    for _ in 0..100 {
        if !pid_alive(pid) {
            return Err(anyhow!("worker exited before the runner socket was ready"));
        }
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    Err(anyhow!("worker socket not ready at {}", socket.display()))
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks whether `pid` exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn kill_pid(pid: u32) {
    // SAFETY: `pid` is a child we spawned.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_store::Store;
    use std::sync::Arc;

    fn runtime_bin() -> Option<PathBuf> {
        std::env::var_os("CODESPACE_RUNTIME_BIN").map(PathBuf::from)
    }

    #[tokio::test]
    async fn spawn_hello_then_drop_kills_worker() {
        let Some(bin) = runtime_bin() else {
            return;
        };
        let store = Arc::new(Store::memory().unwrap());
        store.mark_shell_busy("demo", "proc-keep").unwrap();
        let (mut proc, _runner) =
            RuntimeProcess::spawn(&bin, None, Arc::new(|_| {}), store.clone())
                .await
                .expect("spawn runtime");
        let pid = proc.pid();
        let dir = proc.dir().to_path_buf();
        proc.wait_exit().await;
        drop(proc);
        for _ in 0..50 {
            if !pid_alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid_alive(pid), "worker pid {pid} still alive");
        for _ in 0..50 {
            if store.try_acquire_write("demo").is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _lease = store
            .try_acquire_write("demo")
            .expect("process leases released after worker death");
        assert!(!dir.exists() || std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0) == 0);
    }
}
