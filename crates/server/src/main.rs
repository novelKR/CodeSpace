use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use codespace_policy::Registry;
use codespace_runner::{RuntimeBackend, ShellRelease};
use codespace_server::config::{Cli, RunnerMode, TransportMode};
use codespace_server::http::serve_http;
use codespace_server::logging;
use codespace_server::runtime::RuntimeProcess;
use codespace_server::stdio;
use codespace_store::Store;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let cli = Cli::parse();
    let registry = load_registry(&cli)?;
    let store = load_store(&cli)?;
    let started = start_runner(&cli, store.clone()).await?;
    let runner = started.runner.clone();
    let result = match cli.mode() {
        TransportMode::Stdio => stdio::serve_with_runner(registry, store, runner).await,
        TransportMode::Http => {
            let (bound, _cancel) = serve_http(cli.http_config(), registry, store, runner).await?;
            tracing::info!(%bound, "http ready");
            tokio::signal::ctrl_c().await?;
            Ok(())
        }
    };
    drop(started);
    result
}

struct StartedRunner {
    runner: RuntimeBackend,
    _process: Option<RuntimeProcess>,
}

async fn start_runner(cli: &Cli, store: Arc<Store>) -> Result<StartedRunner> {
    let on_release: ShellRelease = {
        let store_for_lease = store.clone();
        Arc::new(move |process_id: &str| {
            store_for_lease.release_process(process_id);
        })
    };
    match cli.runner {
        RunnerMode::InProcess => Ok(StartedRunner {
            runner: RuntimeBackend::in_process(on_release),
            _process: None,
        }),
        RunnerMode::Uds => {
            if let Some(bin) = &cli.runtime_bin {
                let (process, runner) =
                    RuntimeProcess::spawn(bin, cli.runner_dir.as_deref(), on_release, store)
                        .await?;
                return Ok(StartedRunner {
                    runner: RuntimeBackend::Uds(runner),
                    _process: Some(process),
                });
            }
            let socket = cli.runner_socket.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "uds runner requires --runtime-bin (spawn) or --runner-socket (connect to an existing worker)"
                )
            })?;
            let runner = RuntimeProcess::connect_existing(socket, on_release, store).await?;
            Ok(StartedRunner {
                runner: RuntimeBackend::Uds(runner),
                _process: None,
            })
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
