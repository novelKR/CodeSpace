use anyhow::Result;
use clap::Parser;
use codespace_server::config::{Cli, TransportMode};
use codespace_server::http::serve_http;
use codespace_server::logging;
use codespace_server::stdio;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let cli = Cli::parse();
    match cli.mode() {
        TransportMode::Stdio => stdio::serve().await,
        TransportMode::Http => {
            let (bound, _cancel) = serve_http(cli.http_config()).await?;
            tracing::info!(%bound, "http ready");
            tokio::signal::ctrl_c().await?;
            Ok(())
        }
    }
}
