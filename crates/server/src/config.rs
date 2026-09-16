use clap::{Parser, ValueEnum};

pub const MCP_PATH: &str = "/mcp";

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
