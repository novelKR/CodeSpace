use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use codespace_policy::Registry;
use codespace_server::config::{Cli, TransportMode};
use codespace_server::http::serve_http;
use codespace_server::logging;
use codespace_server::stdio;
use codespace_store::Store;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let cli = Cli::parse();
    let registry = load_registry(&cli)?;
    let store = load_store(&cli)?;
    match cli.mode() {
        TransportMode::Stdio => stdio::serve_with(registry, store).await,
        TransportMode::Http => {
            let (bound, _cancel) = serve_http(cli.http_config(), registry, store).await?;
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

fn load_store(cli: &Cli) -> Result<Arc<Store>> {
    let store = match &cli.operations_db {
        Some(path) => Store::open(path).map_err(anyhow::Error::msg)?,
        None => Store::memory().map_err(anyhow::Error::msg)?,
    };
    Ok(Arc::new(store))
}
