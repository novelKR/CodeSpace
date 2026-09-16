use anyhow::Result;
use clap::Parser;
use codespace_policy::Registry;
use codespace_server::config::{Cli, TransportMode};
use codespace_server::http::serve_http;
use codespace_server::logging;
use codespace_server::stdio;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let cli = Cli::parse();
    let registry = load_registry(&cli)?;
    match cli.mode() {
        TransportMode::Stdio => stdio::serve_with_registry(registry).await,
        TransportMode::Http => {
            let (bound, _cancel) = serve_http(cli.http_config(), registry).await?;
            tracing::info!(%bound, "http ready");
            tokio::signal::ctrl_c().await?;
            Ok(())
        }
    }
}

fn load_registry(cli: &Cli) -> Result<Registry> {
    match &cli.config {
        Some(path) => Registry::load_path(path).map_err(anyhow::Error::msg),
        None => Ok(Registry::new()),
    }
}
