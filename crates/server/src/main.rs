use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use codespace_policy::Registry;
use codespace_runner::{RuntimeBackend, ShellRelease};
use codespace_server::config::{
    Cli, GatewayLifecycle, RunnerExitIntent, RunnerMode, TransportMode,
};
use codespace_server::http::serve_http_handler;
use codespace_server::logging;
use codespace_server::records::ProcessRecords;
use codespace_server::runtime::RuntimeProcess;
use codespace_server::stdio;
use codespace_server::CodeSpace;
use codespace_store::{Store, WriterLock};

/// How long a gateway with an independent Runner waits for the gateway before it to let go of
/// the operations database (P1R-C02): one that is ending detaches at once, or stops its Runner,
/// which waits up to 5 seconds for its processes.
const OPERATIONS_DB_LOCK_WAIT: Duration = Duration::from_secs(15);

fn main() -> Result<()> {
    // First, while this is the only thread: no later fork can then copy libnotify's
    // initialization in progress (see `prepare_fork_spawns`).
    codespace_runner::prepare_fork_spawns();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed building the Runtime");
    logging::init();
    let cli = Cli::parse();
    let lifecycle = cli.lifecycle().map_err(anyhow::Error::msg)?;
    // Released as the gateway exits, once its runtime is shut down.
    let _writer = lock_operations_db(&cli, lifecycle)?;
    let result = runtime.block_on(serve(cli, lifecycle));
    if let GatewayLifecycle::Independent { .. } = lifecycle {
        // A signal can end the gateway while a read of its stdin waits on one of the runtime's
        // blocking threads for as long as the client keeps stdin open: the gateway exits without
        // waiting for it (P1R-C01).
        runtime.shutdown_background();
    }
    result
}

/// A gateway with an independent Runner is its operations database's only writer (P1R-C02): it
/// takes the lock beside the database before it opens it, waiting a bounded time for a gateway
/// that is still ending, and refuses to start after that.
fn lock_operations_db(cli: &Cli, lifecycle: GatewayLifecycle) -> Result<Option<WriterLock>> {
    let (GatewayLifecycle::Independent { .. }, Some(database)) = (lifecycle, &cli.operations_db)
    else {
        return Ok(None);
    };
    let lock = WriterLock::acquire(database, OPERATIONS_DB_LOCK_WAIT, |holder| {
        tracing::info!(
            holder = ?holder,
            wait = ?OPERATIONS_DB_LOCK_WAIT,
            "another gateway holds the operations database; waiting for it to end"
        )
    })
    .map_err(anyhow::Error::msg)?;
    tracing::info!(
        lock = %lock.path().display(),
        "this gateway is the only writer of the operations database"
    );
    Ok(Some(lock))
}

async fn serve(cli: Cli, lifecycle: GatewayLifecycle) -> Result<()> {
    let registry = load_registry(&cli)?;
    let store = load_store(&cli)?;
    let started = start_runner(&cli, lifecycle, store.clone()).await?;
    let mut handler = CodeSpace::from_cli(&cli, registry, store, started.runner.clone());
    if let Some(records) = &started.records {
        handler = handler.with_records(records.clone());
    }
    #[cfg(feature = "devguard")]
    handler.register_owner_now();
    let (result, ending) = match lifecycle {
        GatewayLifecycle::Managed => {
            let result = match cli.mode() {
                TransportMode::Stdio => stdio::serve_handler(handler).await,
                TransportMode::Http => {
                    let (bound, _cancel) = serve_http_handler(cli.http_config(), handler).await?;
                    tracing::info!(%bound, "http ready");
                    tokio::signal::ctrl_c().await?;
                    Ok(())
                }
            };
            (result, Ending::Shutdown)
        }
        GatewayLifecycle::Independent { .. } => serve_until_signal(&cli, handler).await?,
    };
    let stopped = started.stop(ending).await;
    result.and(stopped)
}

/// How the gateway ends (P1R-C01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// A normal shutdown: the end of stdin, SIGTERM or SIGINT.
    Shutdown,
    /// SIGUSR1: the gateway ends to be restarted, so it always detaches from an independent
    /// Runner.
    Restart,
}

/// Serve until the transport ends or a signal ends the gateway (P1R-C01): SIGTERM and SIGINT
/// shut it down as the end of stdin does, SIGUSR1 ends it for a restart.
async fn serve_until_signal(cli: &Cli, handler: CodeSpace) -> Result<(Result<()>, Ending)> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut restart = signal(SignalKind::user_defined1())?;
    let serving = async {
        match cli.mode() {
            TransportMode::Stdio => stdio::serve_handler(handler).await,
            TransportMode::Http => {
                let (bound, _cancel) = serve_http_handler(cli.http_config(), handler).await?;
                tracing::info!(%bound, "http ready");
                std::future::pending::<Result<()>>().await
            }
        }
    };
    Ok(tokio::select! {
        result = serving => (result, Ending::Shutdown),
        _ = terminate.recv() => (Ok(()), Ending::Shutdown),
        _ = interrupt.recv() => (Ok(()), Ending::Shutdown),
        _ = restart.recv() => (Ok(()), Ending::Restart),
    })
}

struct StartedRunner {
    runner: RuntimeBackend,
    process: Option<RuntimeProcess>,
    lifecycle: GatewayLifecycle,
    /// Set before the gateway ends its connection to an independent Runner itself.
    leaving: Arc<AtomicBool>,
    /// The records of the executions sent to an independent Runner (P1R-C02).
    records: Option<Arc<ProcessRecords>>,
}

impl StartedRunner {
    /// Gateway shutdown ends a worker this gateway started, and with it the worker's processes,
    /// before the gateway exits. An independent Runner is left or stopped as the gateway's exit
    /// intent says, and always left for a restart (P1R-C01).
    async fn stop(self, ending: Ending) -> Result<()> {
        let Self {
            runner,
            process,
            lifecycle,
            leaving,
            records: _,
        } = self;
        if let (GatewayLifecycle::Independent { exit_intent }, RuntimeBackend::Uds(runner)) =
            (lifecycle, &runner)
        {
            leaving.store(true, Ordering::SeqCst);
            let intent = match ending {
                Ending::Restart => RunnerExitIntent::Detach,
                Ending::Shutdown => exit_intent,
            };
            match intent {
                RunnerExitIntent::Detach => match runner.detach().await {
                    Ok(()) => tracing::info!(
                        "detached from the independent Runner; its processes keep running"
                    ),
                    Err(err) => tracing::warn!(
                        error = %err,
                        "the independent Runner did not confirm the detach; its processes keep \
                         running"
                    ),
                },
                RunnerExitIntent::Stop => match runner.stop_runner().await {
                    Ok(terminated) => {
                        tracing::info!(terminated, "stopped the independent Runner")
                    }
                    Err(err) => {
                        return Err(anyhow::anyhow!(
                            "the independent Runner did not confirm its stop ({err}); its \
                             processes may keep running, so stop it with SIGTERM"
                        ))
                    }
                },
            }
            return Ok(());
        }
        drop(runner);
        if let Some(mut process) = process {
            process.wait_exit().await;
        }
        Ok(())
    }
}

async fn start_runner(
    cli: &Cli,
    lifecycle: GatewayLifecycle,
    store: Arc<Store>,
) -> Result<StartedRunner> {
    let on_release: ShellRelease = {
        let store_for_lease = store.clone();
        Arc::new(move |process_id: &str| {
            store_for_lease.release_process(process_id);
        })
    };
    let leaving = Arc::new(AtomicBool::new(false));
    match cli.runner {
        RunnerMode::InProcess => Ok(StartedRunner {
            runner: RuntimeBackend::in_process(on_release),
            process: None,
            lifecycle,
            leaving,
            records: None,
        }),
        RunnerMode::Uds => {
            if let (GatewayLifecycle::Independent { .. }, Some(socket)) =
                (lifecycle, &cli.runner_socket)
            {
                // Each exit releases its lease and is recorded (P1R-C02).
                let (on_release, exits) = ProcessRecords::release_hook(store.clone());
                // The gateway only connects; the Runner registers itself, from its own
                // credential file, and nothing is handed to it (P1R-C01).
                let runner =
                    RuntimeProcess::connect_independent(socket, on_release, leaving.clone())
                        .await?;
                #[cfg(feature = "devguard")]
                if cli.devguard.worker().is_some() {
                    codespace_server::runtime::require_registration(&runner, socket)?;
                }
                let identity = runner
                    .hello()
                    .identity
                    .ok_or_else(|| anyhow::anyhow!("the independent Runner has no identity"))?;
                let runner = RuntimeBackend::Uds(runner);
                let records = ProcessRecords::start(store, &identity, runner.clone(), exits);
                return Ok(StartedRunner {
                    runner,
                    process: None,
                    lifecycle,
                    leaving,
                    records: Some(records),
                });
            }
            if let Some(bin) = &cli.runtime_bin {
                // The worker owns the executions, so it registers itself (CSRG-U2).
                #[cfg(feature = "devguard")]
                if let Some(worker) = cli.devguard.worker() {
                    let (process, runner) = RuntimeProcess::spawn_registered(
                        bin,
                        cli.runner_dir.as_deref(),
                        on_release,
                        store,
                        &worker,
                    )
                    .await?;
                    return Ok(StartedRunner {
                        runner: RuntimeBackend::Uds(runner),
                        process: Some(process),
                        lifecycle,
                        leaving,
                        records: None,
                    });
                }
                let (process, runner) =
                    RuntimeProcess::spawn(bin, cli.runner_dir.as_deref(), on_release, store)
                        .await?;
                return Ok(StartedRunner {
                    runner: RuntimeBackend::Uds(runner),
                    process: Some(process),
                    lifecycle,
                    leaving,
                    records: None,
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
                process: None,
                lifecycle,
                leaving,
                records: None,
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
