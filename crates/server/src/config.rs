use clap::{Parser, ValueEnum};

pub const MCP_PATH: &str = "/mcp";
pub const INBOX_PATH: &str = "/inbox";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TransportMode {
    Stdio,
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunnerMode {
    #[value(name = "in-process")]
    InProcess,
    Uds,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "codespace-mcp",
    version,
    about = "CodeSpace execution-tools MCP"
)]
pub struct Cli {
    /// Use Streamable HTTP instead of stdio.
    #[arg(long)]
    pub http: bool,

    /// Transport. Overridden by --http.
    #[arg(long, value_enum, default_value_t = TransportMode::Stdio)]
    pub transport: TransportMode,

    #[arg(long, env = "CODESPACE_HTTP_HOST", default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, env = "CODESPACE_HTTP_PORT", default_value_t = 8787)]
    pub port: u16,

    /// Optional static Bearer for HTTP experiments. Never logged.
    #[arg(long, env = "CODESPACE_HTTP_TOKEN")]
    pub token: Option<String>,

    /// JSON workspace registry. If unset, the registry is empty (all ids unknown).
    #[arg(long, env = "CODESPACE_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// SQLite file for operations. Unset uses an in-memory database (no replay across restarts).
    #[arg(long, env = "CODESPACE_OPERATIONS_DB")]
    pub operations_db: Option<std::path::PathBuf>,

    /// Runner backend. Allowed values: `in-process` (default host supervisor) or `uds`.
    #[arg(long, env = "CODESPACE_RUNNER", value_enum, default_value_t = RunnerMode::InProcess)]
    pub runner: RunnerMode,

    /// Unix socket for `CODESPACE_RUNNER=uds` when connecting to an
    /// already-running worker. Spawn path uses `--runner-dir` instead
    /// and always binds `$dir/runner.sock`.
    #[arg(long, env = "CODESPACE_RUNNER_SOCKET")]
    pub runner_socket: Option<std::path::PathBuf>,

    /// Base directory for a spawned UDS worker. CodeSpace creates a
    /// unique `run-<pid>-<rand>/` child (mode 0700) under this path.
    /// `/`, `/tmp`, `/var/tmp`, and `$HOME` are rejected as this value.
    #[arg(long, env = "CODESPACE_RUNNER_DIR")]
    pub runner_dir: Option<std::path::PathBuf>,

    /// `codespace-codex-runtime` binary for uds mode.
    #[arg(long, env = "CODESPACE_RUNTIME_BIN")]
    pub runtime_bin: Option<std::path::PathBuf>,
}

impl Cli {
    pub fn mode(&self) -> TransportMode {
        if self.http {
            TransportMode::Http
        } else {
            self.transport
        }
    }

    pub fn http_config(&self) -> HttpConfig {
        HttpConfig {
            host: self.host.clone(),
            port: self.port,
            bearer_token: self
                .token
                .as_ref()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub host: String,
    pub port: u16,
    pub bearer_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_mode_rejects_unknown_value() {
        let err = Cli::try_parse_from(["codespace-mcp", "--runner", "udss"])
            .expect_err("unknown runner value");
        let message = err.to_string();
        assert!(message.contains("udss"), "{message}");
    }

    #[test]
    fn runner_mode_accepts_uds_and_in_process() {
        let uds = Cli::try_parse_from(["codespace-mcp", "--runner", "uds"]).expect("uds");
        assert_eq!(uds.runner, RunnerMode::Uds);
        let host =
            Cli::try_parse_from(["codespace-mcp", "--runner", "in-process"]).expect("in-process");
        assert_eq!(host.runner, RunnerMode::InProcess);
    }
}
