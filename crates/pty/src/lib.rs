//! Isolated PTY adapter. Wraps `codex-utils-pty` and exposes only
//! CodeSpace-owned types. Not an MCP tool. Resize stays off this API.

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
/// Resize is not exposed on this API. `env` is the full environment after the
/// caller applied runner-local defaults.
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

    #[test]
    fn crate_is_isolated_adapter() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-pty");
    }

    #[test]
    fn default_size_is_24x80_and_matches_upstream() {
        assert_eq!(DEFAULT_ROWS, 24);
        assert_eq!(DEFAULT_COLS, 80);
        let upstream = codex_utils_pty::TerminalSize::default();
        assert_eq!(
            (upstream.rows, upstream.cols),
            (DEFAULT_ROWS, DEFAULT_COLS),
            "upstream TerminalSize::default drifted from advertised PTY size"
        );
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
}
