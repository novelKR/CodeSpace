<a id="operations"></a>

# Installation and operations

[English](operations.md) | [한국어](ko/operations.md)

This guide sets up a personal, single-user server on macOS or Linux. For the tool-calling sequence, continue with [Agent Loop integration](agent-integration.md).

## Install

Install Git, Python 3 and a current Rust stable toolchain, matching CI. The root manifest declares Rust 1.88, but that is not a verified minimum for the full Codex adapter graph; the pinned upstream checkout selects Rust 1.95.0. On macOS, install the Xcode command-line tools. Linux command isolation additionally needs the sandbox helper, bubblewrap, and permission to create the required namespaces. Docker is only needed for the separate [container fixture](../deploy/README.md).

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# For an existing clone:
git submodule update --init --recursive
# Apply CodeSpace's recorded Codex patch; repeat after every submodule update:
python3 scripts/codex_patch.py apply
cargo build --locked -p codespace-server --bin codespace-mcp --release
cargo build --locked --manifest-path crates/patch/Cargo.toml --bin codespace-patch --release
mkdir -p dist
cp target/release/codespace-mcp dist/
cp crates/patch/target/release/codespace-patch dist/
```

Keep the two binaries together so the server can locate the patch helper. Alternatively, set `CODESPACE_PATCH_BIN` to its absolute path. The helper runs the pinned Codex patch library; the server alone cannot apply patches.

For Linux command isolation, build and place this helper alongside the server:

```bash
cargo build --locked --manifest-path crates/linux-sandbox/Cargo.toml --bin codespace-linux-sandbox --release
cp crates/linux-sandbox/target/release/codespace-linux-sandbox dist/
```

A successful build does not prove isolation is active. Check `workspace_info.execution.isolation.command_sandbox` after connecting. See [activation and failure conditions](runner-isolation.md).

<a id="workspace-registry"></a>

## Register a workspace

Create an existing project directory and a registry outside it. Replace `/absolute/path/to/project` with that directory; do not use this placeholder literally.

```json
{
  "workspaces": {
    "demo": {
      "root": "/absolute/path/to/project",
      "profile": "workspace-write",
      "network": "restricted",
      "approvals": "off"
    }
  }
}
```

Save this as `workspaces.json` in the CodeSpace checkout. `read-only` permits reads; `workspace-write` also permits patches and commands. Only the operator registers roots and chooses permissions. A tool's `workspace_id` selects a registration; it grants no permission by itself.

`network` defaults to `restricted`. `enabled` requires the Linux helper and routes supported HTTP traffic through its managed proxy; it does not grant unrestricted host networking. Without a working helper, `enabled` execution fails. With `restricted` and no helper, host execution is possible but network restrictions are not OS-enforced. Use the reported enforcement state when deciding whether an environment is suitable.

`approvals` defaults to `off`, which runs policy-allowed patches and commands immediately. `confirm` holds those tools before `begin()` or spawn and returns `APPROVAL_REQUIRED` with an `approval_id`. Retrying the same patch or exec reuses the active hold. It is operator JSON like `network`, not an MCP argument, and it does not escalate the profile. Confirmation rows share `CODESPACE_OPERATIONS_DB` in an `approvals` table, separate from the patch operations ledger. While a hold is `pending`, `granted`, `queued`, or `resuming`, that table stores the V4A patch or exec argv. After `denied` or `consumed`, the body is replaced with digest metadata (tool, workspace, fingerprint). The patch ledger keeps hashes, not patch text. See [Agent Loop integration](agent-integration.md) for resolve and resume.

<a id="run-the-gateway"></a>

## Start the server

From the CodeSpace checkout, configure absolute paths and persistent patch/coordination storage:

```bash
export CODESPACE_CONFIG="$PWD/workspaces.json"
mkdir -p data
export CODESPACE_OPERATIONS_DB="$PWD/data/operations.sqlite"
export CODESPACE_PATCH_BIN="$PWD/dist/codespace-patch"
# Use a suitable operator-selected limit for builds; the default is 30 seconds.
export CODESPACE_PROCESS_TIMEOUT_SECS=300
```

For stdio, configure your MCP client to launch the absolute path to `dist/codespace-mcp` with those environment variables. The server defaults to stdio. Running `./dist/codespace-mcp` in a terminal waits for MCP input; it does not open an interactive chat. Logs go to stderr, leaving stdout for MCP.

For Streamable HTTP, keep the process running:

```bash
export CODESPACE_HTTP_HOST=127.0.0.1
export CODESPACE_HTTP_PORT=8787
# Set CODESPACE_HTTP_TOKEN securely if your client will send a Bearer token.
./dist/codespace-mcp --http
```

Connect an MCP client to `http://127.0.0.1:8787/mcp`. The same listener exposes `/inbox` as a JSON API with the same optional Bearer authentication. It is unavailable in stdio-only mode. The server does not automatically load `.env.example`.

Public HTTPS and reverse-proxy deployment need separate verification. The current HTTP Host allowlist is constructed from loopback and bind-host values; there is no separate public-host configuration option. A public hostname may therefore be rejected. Do not assume that binding to `0.0.0.0` configures an external hostname.

<a id="reproduce-the-mvp-flow"></a>

## Verify the first connection

Complete MCP initialization, list tools, and call `workspace_info` with `{"workspace_id":"demo"}`. Confirm that file and process availability match your intended profile. Then read a known project file and follow the [small patch and process example](agent-integration.md).

`available` combines permission and backend support, not current occupancy. A later patch or command may still return `WORKSPACE_BUSY`. A registered `linux-container` environment has no implemented file or exec backend.

## Optional Unix-socket worker

Build the worker and select it before starting the gateway:

```bash
cargo build --locked --manifest-path crates/codex-runtime/Cargo.toml --bin codespace-codex-runtime --release
cp crates/codex-runtime/target/release/codespace-codex-runtime dist/
export CODESPACE_RUNNER=uds
export CODESPACE_RUNTIME_BIN="$PWD/dist/codespace-codex-runtime"
```

The worker runs on the same host and is not a container. The gateway creates a private socket directory and owns the child. Worker connection loss or gateway shutdown ends that worker's processes; reconnect and process recovery are not supported. MCP/HTTP client disconnect does not kill those processes. The default remains `in-process`. An [independent Runner](runner-isolation.md#independent-runner), whose processes outlive the gateway, whose gateway records them in the operations database, and which a gateway claims again after a lost connection and then reconciles with its records, is not a supported mode yet; when such a Runner itself is lost, an operator settles what it ran with `codespace-mcp records` ([Runner loss](#runner-loss)).

<a id="logs"></a>
<a id="recovery-after-disconnect-or-restart"></a>

## Logs, limits, and recovery

| Setting or limit | Behavior |
| --- | --- |
| `RUST_LOG` | stderr tracing level; default `info` |
| `CODESPACE_OPERATIONS_DB` unset | In-memory patch operations and instruction queue; lost on restart. A gateway with an independent Runner does not start without it |
| `CODESPACE_PROCESS_TIMEOUT_SECS` | Positive integer; default 30 seconds; set in the runner environment |
| `CODESPACE_MAX_PROCESSES` | Default 8 live processes across the runner; a request over it is refused before any process is created; workspace occupancy still applies |
| Process output | Last 256 KiB retained; stdout/stderr combined; `read_process` reports `output_lost` and `retained_from`, and `lost_reason` says why output is missing; `process_status` reports termination; `process_resize` resizes a running PTY |
| Completed handles | Default retention up to 15 minutes and 64 completed entries; not durable. An independent Runner keeps an ended process's status and output until its gateway acknowledges them, at most 64 once their handles are released ([ended processes](runner-isolation.md#reconciliation)); after that, its gateway answers from the process record, without output, with `lost_reason` `retention_expired` |

Store logs and the database outside the managed workspace, with tokens and gateway configuration. Rotate stderr capture yourself. Do not log Bearer tokens or commit real credentials. Deleting the database also deletes patch idempotency records and confirmation-hold rows.

<a id="operations-database"></a>

**Schema version and earlier versions.** The operations database states its schema version. CodeSpace refuses a database with a newer schema version than its own before writing anything, and leaves it as it is: open it with the CodeSpace version that wrote it. A gateway with an [independent Runner](runner-isolation.md#process-records) also records its executions in the database, keeps there the random identity of the database that it names when it [claims the Runner](runner-isolation.md#claim), and holds the lock file `<database>.gateway.lock` beside it while it runs. A CodeSpace version from before these records still opens such a database and leaves the records alone, but it neither checks the schema version nor takes the lock, so do not run one on a database that a gateway with an independent Runner uses. To go back from the independent mode, stop using it and keep the database: restoring an earlier copy would drop the patch idempotency records, confirmation holds and process records written since.

After losing a patch response, query `operation_status` with exactly one of `operation_id` or `operation_key`. An unfinished record becomes `unknown` after restart; inspect files before deciding what to do. Processes use `process_id` and cannot be recovered through `operation_status`. A confirmation hold that was `queued` when the process died can be resumed and will reacquire. A hold that was `resuming` may recover a patch from that ledger, replay a stored terminal result, or return `APPROVAL_AMBIGUOUS`; exec is not respawned. A gateway with an independent Runner settles an exec hold left `resuming` when it [reconciles](runner-isolation.md#reconciliation): consumed with the process that the Runner created for it, or granted again when the Runner proves that it created none. See [retry and recovery rules](agent-integration.md).

<a id="runner-loss"></a>

## Runner loss

This applies to a gateway with an [independent Runner](runner-isolation.md#independent-runner), which is not a supported mode yet. When that Runner is killed, crashes or stops, or the host restarts, what it was running can no longer be learned from it, and its processes may outlive it ([Runner loss](runner-isolation.md#runner-loss)). The gateway answers for them from their records, as `unknown` with a `lost_reason`, and each workspace they held stays refused, with `RECOVERY_PENDING` in the next gateway, until you confirm that nothing of them runs and release their records. CodeSpace ends none of these processes, starts nothing again, and asks DevGuard nothing about them.

1. **List the records** that may still hold a workspace. This only reads the database, so it also works while a gateway runs:

   ```bash
   ./dist/codespace-mcp records list --operations-db "$CODESPACE_OPERATIONS_DB"
   ```

   It prints JSON. Each record has its `process_id` and `workspace_id`; its `phase`, `dispatching`, `running` or `unknown`; `lost`, which is `runner_lost` or `host_rebooted` once a gateway or a release marked it lost, and null before; its `participation` and DevGuard `attempt_id`; the `approval_id` it resumed; `tty`; its `runner`, with the Runner's PID and boot, `this_boot`, whether it ran on this boot of the host, and `pid_running`, whether a process with that PID runs now, which may be another process that took the PID; and its `root` process, with `running`, whether that very process still runs, or null when the record cannot tell.

2. **Find what still runs** of each record. After a restart of the host, `host_rebooted` or `this_boot` false, nothing does. Otherwise:
   - The root process runs when `running` is true. `ps -o pid,pgid,sid,lstart,command -p <pid>` shows a process with its process group and session.
   - A pipe process, and what it starts, stays in the process group it started in unless it leaves it: the Runner's own, or one that the root leads, as a sandbox helper with network enabled and DevGuard's launch helper do. `pgrep -g <pgid>` lists a group, `pgrep -P <pid>` a process's children, and `lsof -d cwd` shows which processes work in the workspace's directories. One that wrote to its output since the Runner's end most likely ended with SIGPIPE, unless it ignores that signal.
   - A `tty: true` process leads its own session, which `pgrep -s <root pid>` lists, and outlives the Runner only if it ignored the SIGHUP that the Runner's end most likely sent it.
   - A record without a root, still `dispatching` when the Runner was lost, may or may not have started its process: look for it as above.
   - Under the Linux command sandbox, commands in a workspace without network end with the Runner; with network enabled, they do not. A service manager may have ended the rest when the Runner's service stopped, as systemd does for a service's control group with its default `KillMode=control-group`, and launchd for a job's process group unless the job sets `AbandonProcessGroup`. Check rather than assume.

3. **End what still runs**, once you decide that it should not finish: `kill -TERM <pid>`, then `kill -KILL <pid>` if it does not exit. No deadline ends it any more, since the Runner that enforced the deadline is gone.

4. **For a `required` record, wait until DevGuard no longer charges its attempt.** The lost Runner was DevGuard's execution owner, and DevGuard keeps the attempt charged until it observes that the attempt's processes ended, or until the host restarts. On macOS, `devguard admission --open` prints DevGuard's `quiescence`: `charged_attempts`, how many attempts it charges, and `attempts`, up to 16 of their keys. While a key there has the record's `attempt_id`, or `charged_attempts` is larger than the number of keys listed, DevGuard may still charge it. That command also reopens admission when it is closed, as an interrupted DevGuard upgrade leaves it, so complete such an upgrade first as [DevGuard's operations guide](https://github.com/novelKR/DevGuard/blob/7cd86e4c56cdd6c6067ceda183b8f3f82075fcaa/docs/operations.md#available-commands) says. DevGuard has no command that only reads its quiescence. When it prints `quiescence` as null, the serving DevGuard release does not report it, and nothing tells whether DevGuard still charges the attempt: keep the record until the host restarts, which ends the attempt in DevGuard too. When DevGuard lost track of an attempt's processes, as its [reconciliation contract](https://github.com/novelKR/DevGuard/blob/7cd86e4c56cdd6c6067ceda183b8f3f82075fcaa/docs/contracts.md#reconciliation) describes, it keeps the attempt charged until the host restarts, even after they all ended. Do not release a `required` record while DevGuard charges its attempt.

5. **Stop the gateway and release the records:**

   ```bash
   ./dist/codespace-mcp records release --operations-db "$CODESPACE_OPERATIONS_DB" <process_id>...
   ```

   It changes records only while no gateway runs on the database, since a running gateway holds its lock file. It releases each record whose Runner was lost once the record's root process no longer runs, prints JSON with `released`, `already_released` and `refused`, each refusal with its reason, and exits with status 1 when it refused any. It refuses a record that ended, which holds no workspace, one whose root process still runs, and one whose Runner may still run: a Runner that stopped while no gateway ran left its records `running`, and they are marked lost, with every other unended record of that Runner, only once no process has that Runner's PID. It ends no process. A released record stays `unknown`, and `operation_resume` answers an approval it resumed with `APPROVAL_AMBIGUOUS`: that command never runs again.

6. **Start a Runner and the gateway again.** The gateway reconciles each workspace, and one whose records all ended or were released takes new work again. With DevGuard, the new Runner registers as the `codespace` consumer's one instance, which DevGuard allows only once it has retired the lost Runner's instance, when none of that Runner's attempts is charged any more. Until then `workspace_info` reports `resource_authority.registration.state` as `refused`, and `required` workspaces do not run; `off` workspaces are not affected. DevGuard does not let a consumer's instance limit change while one of its instances is not retired.

A workspace where `RECOVERY_PENDING` says that the records, the Runner and DevGuard do not agree is not settled by `records release` while its Runner runs. Stopping that Runner makes its unended records records of a lost Runner, in every workspace it held, and the steps above then apply.

<a id="linux-isolation-fixture"></a>
<a id="what-this-document-does-not-verify"></a>

## Troubleshooting

| Symptom | First check |
| --- | --- |
| `WORKSPACE_NOT_FOUND` | Registry path, registered ID, and process environment |
| Patch helper cannot start | Build `codespace-patch` and check its absolute path |
| `WORKSPACE_BUSY` | Finish or terminate the existing command before patching |
| `RESOURCE_QUEUE_FULL` | Too many waiters; retry later |
| Command times out | Operator timeout; do not assume a long build completed |
| Linux reports no sandbox | Helper location, bubblewrap, namespace support; see isolation guide |
| `enabled` command fails before starting | A working Linux sandbox helper is required |
| HTTP rejects the request | `/mcp` path, Bearer header, and Host validation |
| `RUNNER_DISCONNECTED` saying that the gateway lost its Runner, or `RECOVERY_PENDING` naming `codespace-mcp records` | Follow [Runner loss](#runner-loss) |

Local transport tests do not establish a live ChatGPT connection, compatibility with every package manager through the proxy, or protection against kernel/container escapes.
