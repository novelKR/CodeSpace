<a id="operations"></a>

# Installation and operations

[English](operations.md) | [한국어](ko/operations.md)

This guide sets up a personal, single-user server on macOS or Linux. For the tool-calling sequence, continue with [Agent Loop integration](agent-integration.md).

## Install

Install Git and a current Rust stable toolchain, matching CI. The root manifest declares Rust 1.88, but that is not a verified minimum for the full Codex adapter graph; the pinned upstream checkout selects Rust 1.95.0. On macOS, install the Xcode command-line tools. Linux command isolation additionally needs the sandbox helper, bubblewrap, and permission to create the required namespaces. Docker is only needed for the separate [container fixture](../deploy/README.md).

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# For an existing clone:
git submodule update --init --recursive
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

The worker runs on the same host and is not a container. The gateway creates a private socket directory and owns the child. Worker connection loss or gateway shutdown ends that worker's processes; reconnect and process recovery are not supported. MCP/HTTP client disconnect does not kill those processes. The default remains `in-process`.

<a id="logs"></a>
<a id="recovery-after-disconnect-or-restart"></a>

## Logs, limits, and recovery

| Setting or limit | Behavior |
| --- | --- |
| `RUST_LOG` | stderr tracing level; default `info` |
| `CODESPACE_OPERATIONS_DB` unset | In-memory patch operations and instruction queue; lost on restart |
| `CODESPACE_PROCESS_TIMEOUT_SECS` | Positive integer; default 30 seconds; set in the runner environment |
| `CODESPACE_MAX_PROCESSES` | Default 8 live processes across the runner; workspace occupancy still applies |
| Process output | Last 256 KiB retained; stdout/stderr combined; `read_process` reports `output_lost` and `retained_from`; `process_status` reports termination; `process_resize` resizes a running PTY |
| Completed handles | Default retention up to 15 minutes and 64 completed entries; not durable |

Store logs and the database outside the managed workspace, with tokens and gateway configuration. Rotate stderr capture yourself. Do not log Bearer tokens or commit real credentials. Deleting the database also deletes patch idempotency records and confirmation-hold rows.

After losing a patch response, query `operation_status` with exactly one of `operation_id` or `operation_key`. An unfinished record becomes `unknown` after restart; inspect files before deciding what to do. Processes use `process_id` and cannot be recovered through `operation_status`. A confirmation hold that was `queued` when the process died can be resumed and will reacquire. A hold that was `resuming` may recover a patch from that ledger, replay a stored terminal result, or return `APPROVAL_AMBIGUOUS`; exec is not respawned. See [retry and recovery rules](agent-integration.md).

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

Local transport tests do not establish a live ChatGPT connection, compatibility with every package manager through the proxy, or protection against kernel/container escapes.
