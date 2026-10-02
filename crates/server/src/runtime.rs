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
use tokio::task::JoinHandle;

pub struct RuntimeProcess {
    dir: PathBuf,
    socket: PathBuf,
    shutdown: Arc<Notify>,
    wait: Option<JoinHandle<()>>,
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
        let mut child = worker_command(bin, &socket)
            .spawn()
            .with_context(|| format!("spawn {}", bin.display()))?;
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
        let stream = wait_for_socket(&socket, &wait).await?;
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

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub async fn wait_exit(&mut self) {
        self.shutdown.notify_one();
        if let Some(wait) = self.wait.take() {
            let _ = wait.await;
        }
    }
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        self.shutdown.notify_one();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The worker gets the socket path, a null stdin, the Gateway's stdout and stderr, and no other
/// descriptor of the Gateway's.
fn worker_command(bin: &Path, socket: &Path) -> Command {
    let mut command = Command::new(bin);
    command.arg(socket).stdin(Stdio::null()).kill_on_drop(true);
    codespace_runner::exclude_unrelated(&mut command);
    command
}

async fn wait_for_socket(socket: &Path, wait: &JoinHandle<()>) -> Result<UnixStream> {
    for _ in 0..100 {
        if wait.is_finished() {
            return Err(anyhow!("worker exited before the runner socket was ready"));
        }
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    Err(anyhow!("worker socket not ready at {}", socket.display()))
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
    async fn worker_keeps_only_its_standard_descriptors() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        // The server crate has no libc dependency; F_DUPFD is 0 on macOS and Linux.
        extern "C" {
            fn fcntl(fd: std::os::raw::c_int, cmd: std::os::raw::c_int, ...)
                -> std::os::raw::c_int;
        }
        // A descriptor that another thread's creation window could leave inheritable (#79).
        let null = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD returns a new descriptor without close-on-exec, owned below.
        let held = unsafe { fcntl(null.as_raw_fd(), 0, 400) };
        assert!(held >= 400, "F_DUPFD");
        // SAFETY: as above.
        let held = unsafe { OwnedFd::from_raw_fd(held) };
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report");
        // `/bin/sh` reads the script given where the socket path goes; executing a freshly
        // written file could meet ETXTBSY from a concurrent fork.
        let script = dir.path().join("worker.sh");
        std::fs::write(
            &script,
            format!(
                "for n in 1 {}; do if [ -e /dev/fd/$n ]; then echo held; else echo clear; fi; done > '{}'\n",
                held.as_raw_fd(),
                report.display()
            ),
        )
        .unwrap();
        let status = worker_command(Path::new("/bin/sh"), &script)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let seen = std::fs::read_to_string(&report).unwrap();
        assert_eq!(seen, "held\nclear\n");
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
        let dir = proc.dir().to_path_buf();
        proc.wait_exit().await;
        drop(proc);
        let _lease = store
            .try_acquire_write("demo")
            .expect("process leases released after worker death");
        assert!(!dir.exists() || std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0) == 0);
    }
}
