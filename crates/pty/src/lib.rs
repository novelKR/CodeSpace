//! Isolated PTY adapter. Wraps `codex-utils-pty` and exposes only
//! CodeSpace-owned types. Not an MCP tool.

use std::collections::HashMap;
use std::path::Path;

use tokio::sync::{mpsc, oneshot};

/// Interactive process attached to a PTY. Codex types stay inside this crate.
pub struct PtySession {
    handle: codex_utils_pty::ProcessHandle,
    stdout_rx: Option<mpsc::Receiver<Vec<u8>>>,
    exit_rx: Option<oneshot::Receiver<i32>>,
}

impl PtySession {
    /// Channel for writing bytes to the PTY master (child stdin).
    pub fn writer(&self) -> mpsc::Sender<Vec<u8>> {
        self.handle.writer_sender()
    }

    /// Take the stdout receiver once. PTY output is merged on the master;
    /// stderr is unused.
    pub fn take_stdout(&mut self) -> Option<mpsc::Receiver<Vec<u8>>> {
        self.stdout_rx.take()
    }

    /// Take the exit oneshot once.
    pub fn take_exit(&mut self) -> Option<oneshot::Receiver<i32>> {
        self.exit_rx.take()
    }

    pub fn has_exited(&self) -> bool {
        self.handle.has_exited()
    }

    /// Change the PTY size in character cells. Codex types stay inside.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.handle
            .resize(codex_utils_pty::TerminalSize { rows, cols })
            .map_err(|err| err.to_string())
    }

    /// Kill the child (Unix process group) without exposing Codex signals.
    pub fn kill(&self) {
        self.handle.request_terminate();
    }
}

/// Default PTY size advertised by `workspace_info.execution.process.capabilities.tty`.
/// Domain constants must match these; do not rely on upstream Default.
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 80;

/// Spawn `program` + `args` on a PTY at [`DEFAULT_ROWS`]×[`DEFAULT_COLS`].
/// `env` is the full environment after the caller applied runner-local defaults.
pub async fn spawn(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<PtySession, String> {
    if program.is_empty() {
        return Err("command must be a non-empty argv (no shell)".into());
    }
    let spawned = codex_utils_pty::spawn_pty_process(
        program,
        args,
        cwd,
        env,
        &None,
        codex_utils_pty::TerminalSize {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
        },
        &[],
    )
    .await
    .map_err(|err| err.to_string())?;
    Ok(PtySession {
        handle: spawned.session,
        stdout_rx: Some(spawned.stdout_rx),
        exit_rx: Some(spawned.exit_rx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn crate_is_isolated_adapter() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-pty");
    }

    #[tokio::test]
    async fn created_terminal_has_advertised_size() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
        let mut session = spawn("/bin/stty", &["size".into()], dir.path(), &env)
            .await
            .expect("spawn stty");
        let mut output = session.take_stdout().expect("stdout");
        let code = tokio::time::timeout(Duration::from_secs(5), session.take_exit().unwrap())
            .await
            .expect("exit timeout")
            .expect("exit receiver");
        assert_eq!(code, 0);
        let bytes = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            while let Some(chunk) = output.recv().await {
                bytes.extend(chunk);
            }
            bytes
        })
        .await
        .expect("output timeout");
        assert_eq!(String::from_utf8(bytes).unwrap().trim(), "24 80");
    }

    #[tokio::test]
    async fn spawn_true_sees_a_tty() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), dir.path().display().to_string()),
            ("LANG".into(), "C".into()),
            ("TERM".into(), "xterm".into()),
        ]);
        let mut session = spawn("/bin/test", &["-t".into(), "0".into()], dir.path(), &env)
            .await
            .expect("spawn PTY");
        let exit = session.take_exit().expect("exit");
        let code = tokio::time::timeout(Duration::from_secs(5), exit)
            .await
            .expect("exit wait")
            .expect("exit recv");
        assert_eq!(code, 0, "stdin should be a TTY");
    }

    async fn collect_until(output: &mut mpsc::Receiver<Vec<u8>>, needle: &str) -> String {
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let chunk = tokio::time::timeout(Duration::from_millis(500), output.recv())
                .await
                .ok()
                .flatten();
            let Some(chunk) = chunk else {
                continue;
            };
            text.push_str(&String::from_utf8_lossy(&chunk));
            if text.replace("\r\n", "\n").contains(needle) {
                return text;
            }
        }
        panic!("timed out waiting for {needle:?}, got {text:?}");
    }

    #[tokio::test]
    async fn resize_changes_stty_size() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), dir.path().display().to_string()),
            ("LANG".into(), "C".into()),
            ("TERM".into(), "xterm".into()),
        ]);
        let script =
            "stty -echo; printf 'start:%s\\n' \"$(stty size)\"; IFS= read _line; printf 'after:%s\\n' \"$(stty size)\"";
        let mut session = spawn("/bin/sh", &["-c".into(), script.into()], dir.path(), &env)
            .await
            .expect("spawn stty");
        let mut output = session.take_stdout().expect("stdout");
        let writer = session.writer();
        let start = collect_until(&mut output, "start:24 80").await;
        assert!(
            start.replace("\r\n", "\n").contains("start:24 80"),
            "initial size, got {start:?}"
        );
        session.resize(40, 120).expect("resize");
        writer.send(b"go\n".to_vec()).await.expect("write");
        let rest = collect_until(&mut output, "after:40 120").await;
        let combined = format!("{start}{rest}").replace("\r\n", "\n");
        assert!(
            combined.contains("after:40 120"),
            "resized size, got {combined:?}"
        );
        let code = tokio::time::timeout(Duration::from_secs(5), session.take_exit().unwrap())
            .await
            .expect("exit timeout")
            .expect("exit receiver");
        assert_eq!(code, 0);
    }
}
