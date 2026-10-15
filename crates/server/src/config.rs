use clap::{Parser, Subcommand, ValueEnum};

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

/// How a UDS Runner's processes relate to the gateway (P1R-C01).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum RunnerLifecycleMode {
    /// The worker belongs to the gateway, which starts it or connects to it through
    /// `--runner-socket`; its processes end with the gateway's connection.
    #[default]
    Managed,
    /// An independent Runner that an operator or a harness started: the gateway only connects
    /// to it through `--runner-socket`, and its processes outlive the gateway. Supported only on
    /// the combinations its qualification qualifies (P1R-C06).
    Independent,
}

/// What a gateway asks of its independent Runner when it shuts down normally (P1R-C01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunnerExitIntent {
    /// Leave the Runner, whose processes keep running.
    Detach,
    /// Stop the Runner: it ends its processes and exits.
    Stop,
}

/// The gateway's Runner lifecycle once its settings were checked together (P1R-C01).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GatewayLifecycle {
    /// The in-process runner, or a worker that belongs to the gateway.
    #[default]
    Managed,
    /// An independent Runner, and what a normal shutdown asks of it.
    Independent { exit_intent: RunnerExitIntent },
}

/// An operator's command instead of serving MCP (P1R-C05).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// The process records of a gateway with an independent Runner, in the database that
    /// --operations-db names.
    #[command(subcommand)]
    Records(RecordsCommand),
}

/// What an operator does with process records (P1R-C05, D8).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RecordsCommand {
    /// Print, as JSON, every record that may still hold a workspace: those that have not ended
    /// and were not released, with whether their root process still runs. Reads only, so a
    /// gateway may be running.
    List,
    /// Release the workspaces that these records hold after their Runner was lost, once nothing
    /// of their processes runs. Refused while a gateway records into the database, for a record
    /// whose root process still runs, and for one whose Runner may still run.
    Release {
        /// The process IDs of the records to release.
        #[arg(required = true)]
        process_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "codespace-mcp",
    version,
    about = "CodeSpace execution-tools MCP"
)]
pub struct Cli {
    /// An operator's command; without one, the gateway serves MCP.
    #[command(subcommand)]
    pub command: Option<Command>,

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
    /// Required with `--runner-lifecycle independent`, whose gateway records its executions there
    /// and holds the lock file beside it (P1R-C02), and with `records`.
    #[arg(long, env = "CODESPACE_OPERATIONS_DB", global = true)]
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

    /// The UDS Runner's lifecycle: `managed` (default), or `independent` for a Runner that an
    /// operator or a harness started, which the gateway only connects to through
    /// `--runner-socket` (P1R-C01). `independent` is supported only on the combinations its
    /// qualification qualifies (P1R-C06).
    #[arg(long, value_enum, default_value_t = RunnerLifecycleMode::Managed)]
    pub runner_lifecycle: RunnerLifecycleMode,

    /// Required with `--runner-lifecycle independent`, and only with it: what a normal
    /// shutdown (end of stdin, SIGTERM, SIGINT) asks of the Runner, `detach` or `stop`. SIGUSR1
    /// ends the gateway for a restart and always detaches.
    #[arg(long, value_enum)]
    pub runner_exit_intent: Option<RunnerExitIntent>,

    /// Opt-in DevGuard connection: status (CSRG-U1) or the execution owner's registration
    /// (CSRG-U2).
    #[cfg(feature = "devguard")]
    #[command(flatten)]
    pub devguard: crate::devguard::DevGuardArgs,
}

impl Cli {
    /// The gateway's Runner lifecycle, or why its settings do not fit together (P1R-C01).
    pub fn lifecycle(&self) -> Result<GatewayLifecycle, String> {
        let exit_intent = match (self.runner_lifecycle, self.runner_exit_intent) {
            (RunnerLifecycleMode::Managed, None) => return Ok(GatewayLifecycle::Managed),
            (RunnerLifecycleMode::Managed, Some(_)) => {
                return Err(
                    "--runner-exit-intent is only for --runner-lifecycle independent".into(),
                )
            }
            (RunnerLifecycleMode::Independent, None) => {
                return Err(
                    "--runner-lifecycle independent needs --runner-exit-intent detach \
                            or stop"
                        .into(),
                )
            }
            (RunnerLifecycleMode::Independent, Some(exit_intent)) => exit_intent,
        };
        if self.runner != RunnerMode::Uds {
            return Err("--runner-lifecycle independent needs --runner uds".into());
        }
        if self.runtime_bin.is_some() || self.runner_dir.is_some() {
            return Err(
                "--runner-lifecycle independent connects to a Runner that an operator \
                        or a harness started; --runtime-bin and --runner-dir are for a worker \
                        the gateway starts"
                    .into(),
            );
        }
        if self.runner_socket.is_none() {
            return Err("--runner-lifecycle independent needs --runner-socket".into());
        }
        // Its executions outlive the gateway, so their records must too (P1R-C02).
        if self.operations_db.is_none() {
            return Err("--runner-lifecycle independent needs --operations-db".into());
        }
        Ok(GatewayLifecycle::Independent { exit_intent })
    }

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

    /// An independent Runner is only connected to, through `--runner-socket`, and a normal
    /// shutdown's intent toward it is always stated; a managed one takes no intent (P1R-C01).
    #[test]
    fn only_an_independent_runner_with_an_exit_intent_is_accepted() {
        let lifecycle = |args: &[&str]| {
            let mut argv = vec!["codespace-mcp"];
            argv.extend_from_slice(args);
            let mut cli = Cli::try_parse_from(argv).expect("parses");
            // Runner settings that the environment of the test run may give.
            if !args.contains(&"--runner") {
                cli.runner = RunnerMode::InProcess;
            }
            if !args.contains(&"--runner-socket") {
                cli.runner_socket = None;
            }
            if !args.contains(&"--runner-dir") {
                cli.runner_dir = None;
            }
            if !args.contains(&"--runtime-bin") {
                cli.runtime_bin = None;
            }
            if !args.contains(&"--operations-db") {
                cli.operations_db = None;
            }
            cli.lifecycle()
        };
        let socket = [
            "--runner",
            "uds",
            "--runner-socket",
            "/run/cs/runner.sock",
            "--operations-db",
            "/var/lib/cs/operations.sqlite",
        ];
        let independent = ["--runner-lifecycle", "independent"];
        assert_eq!(lifecycle(&[]), Ok(GatewayLifecycle::Managed));
        assert_eq!(lifecycle(&socket), Ok(GatewayLifecycle::Managed));
        for (intent, exit_intent) in [
            ("detach", RunnerExitIntent::Detach),
            ("stop", RunnerExitIntent::Stop),
        ] {
            assert_eq!(
                lifecycle(&[&socket[..], &independent, &["--runner-exit-intent", intent]].concat()),
                Ok(GatewayLifecycle::Independent { exit_intent })
            );
        }
        let detach = ["--runner-exit-intent", "detach"];
        for (args, problem) in [
            ([&socket[..], &detach].concat(), "only for"),
            (
                [&socket[..], &independent].concat(),
                "needs --runner-exit-intent",
            ),
            ([&independent[..], &detach].concat(), "needs --runner uds"),
            (
                [&["--runner", "uds"][..], &independent, &detach].concat(),
                "needs --runner-socket",
            ),
            (
                [&socket[..4], &independent, &detach].concat(),
                "needs --operations-db",
            ),
            (
                [
                    &socket[..],
                    &independent,
                    &detach,
                    &["--runtime-bin", "/usr/local/bin/codespace-codex-runtime"],
                ]
                .concat(),
                "--runtime-bin",
            ),
            (
                [
                    &socket[..],
                    &independent,
                    &detach,
                    &["--runner-dir", "/run/cs"],
                ]
                .concat(),
                "--runner-dir",
            ),
        ] {
            let err = lifecycle(&args).expect_err(problem);
            assert!(err.contains(problem), "{args:?}: {err}");
        }
        let err = Cli::try_parse_from(["codespace-mcp", "--runner-lifecycle", "detached"])
            .expect_err("an unknown lifecycle");
        assert!(err.to_string().contains("detached"), "{err}");
    }

    /// The operator's records commands name the operations database, before or after the
    /// command, and release at least one record (P1R-C05).
    #[test]
    fn the_records_commands_take_the_operations_database() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["codespace-mcp"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv)
        };
        let cli = parse(&["records", "list", "--operations-db", "/var/cs/ops.sqlite"]).unwrap();
        assert_eq!(cli.command, Some(Command::Records(RecordsCommand::List)));
        assert_eq!(
            cli.operations_db.as_deref(),
            Some(std::path::Path::new("/var/cs/ops.sqlite"))
        );
        let cli = parse(&[
            "--operations-db",
            "/var/cs/ops.sqlite",
            "records",
            "release",
            "proc-1",
            "proc-2",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Records(RecordsCommand::Release {
                process_ids: vec!["proc-1".into(), "proc-2".into()]
            }))
        );
        assert!(parse(&["records", "release"]).is_err());
        assert!(parse(&["records"]).is_err());
        assert_eq!(parse(&[]).unwrap().command, None);
    }

    /// Without the `devguard` feature the gateway has no DevGuard setting at all.
    #[cfg(not(feature = "devguard"))]
    #[test]
    fn devguard_settings_need_the_devguard_feature() {
        for flag in [
            "--devguard",
            "--devguard-socket",
            "--devguard-credential-file",
        ] {
            let err = Cli::try_parse_from(["codespace-mcp", flag, "status"]).expect_err(flag);
            assert!(err.to_string().contains(flag), "{err}");
        }
    }
}
