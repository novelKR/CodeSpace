use anyhow::Result;
use clap::Parser;
use codespace_policy::Registry;
use codespace_server::config::{Cli, TransportMode};
use codespace_server::http::serve_http_with;
use codespace_server::logging;
use codespace_server::mcp::CodeSpace;
use codespace_server::stdio;
use codespace_store::{OperationStore, WorkspaceLocks};

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let cli = Cli::parse();
    let registry = load_registry(&cli)?;
    let store = load_store(&cli)?;
    let server = CodeSpace::new_with(registry, store, WorkspaceLocks::new());
    match cli.mode() {
        TransportMode::Stdio => stdio::serve_instance(server).await,
        TransportMode::Http => {
            let (bound, _cancel) = serve_http_with(cli.http_config(), server).await?;
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

fn load_store(cli: &Cli) -> Result<std::sync::Arc<OperationStore>> {
    match &cli.store {
        Some(path) => OperationStore::open(path).map_err(anyhow::Error::msg),
        None => Ok(OperationStore::memory()),
    }
}
