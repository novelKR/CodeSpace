use clap::{Parser, ValueEnum};

pub const MCP_PATH: &str = "/mcp";
pub const INBOX_PATH: &str = "/inbox";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TransportMode {
    Stdio,
    Http,
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

    /// Runner backend. `in-process` is the default host supervisor. `uds` uses ContainerRunner.
    #[arg(long, env = "CODESPACE_RUNNER", default_value = "in-process")]
    pub runner: String,

    /// Unix socket for `CODESPACE_RUNNER=uds`.
    #[arg(long, env = "CODESPACE_RUNNER_SOCKET")]
    pub runner_socket: Option<std::path::PathBuf>,

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
