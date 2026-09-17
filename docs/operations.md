# Operations

[English](operations.md) | [한국어](ko/operations.md)

Reproduce a local CodeSpace from a clean clone: install, start the
gateway, then `workspace_info` → `read` → `apply_patch` → `exec_command`.
This is a **personal single-user** layout. It is not a multi-tenant SaaS
and not a full OAuth server.

ChatGPT Custom Connector against a public hostname is **not verified**.
See [chatgpt-connector.md](chatgpt-connector.md).

## Install

Needs Rust 1.88+, Git, and (for the Linux isolation fixture) Docker.

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# If you already cloned without submodules:
# git submodule update --init --recursive
```

The Codex pin is `third_party/codex` at the commit in
[upstream-lock.md](upstream-lock.md). Do not `git submodule update --remote`
to Codex `main`. Product runtime stays out of the gateway; see
[codex-reuse.md](codex-reuse.md) and
[execution-substrate.md](execution-substrate.md).

Build the gateway and patch helper into the **same** directory so the
gateway can find the helper next to itself (or set `CODESPACE_PATCH_BIN`).
The UDS worker is optional (`CODESPACE_RUNTIME_BIN`).

```bash
cargo build -p codespace-server --bin codespace-mcp --release
cargo build --manifest-path crates/patch/Cargo.toml --bin codespace-patch --release
mkdir -p dist
cp target/release/codespace-mcp dist/
cp crates/patch/target/release/codespace-patch dist/
# Optional Unix-socket worker (not the default exec path):
cargo build --manifest-path crates/codex-runtime/Cargo.toml --bin codespace-codex-runtime --release
cp crates/codex-runtime/target/release/codespace-codex-runtime dist/
```

`codespace-patch` is a host child process. It hosts the pinned Codex
crate **in-process**. It is not the upstream standalone `apply_patch`
binary and not the retired `native/patch-worker`. `codespace-codex-runtime`
binds a private Unix socket with `codex-process-hardening` and
`codex-uds`, then runs `InProcessRunner`. Hardening is **worker/helper
process** hardening (`pre_main_hardening()` as the first line of
`main`; no `ctor`), not a command sandbox. Default `exec_command` still
uses in-process host spawn. `exec_command.tty` defaults to false (pipes).
`tty: true` attaches a PTY at 24x80. Exec DTO cwd is `WorkspaceRoot`; `PATH` /
`HOME` / `LANG` are applied inside the runner process (`TERM=xterm` for PTY).

## Workspace registry

Copy [workspaces.example.json](workspaces.example.json) and point `root`
at a **real directory you registered**. Models cannot add workspaces.

```json
{
  "workspaces": {
    "demo": {
      "root": "/absolute/path/to/your/project",
      "profile": "workspace-write"
    }
  }
}
```

Profiles: `read-only` (default intent) or `workspace-write`. `host-admin`
is not a product profile. Optional operator `environments` may register
`host` or `linux-container`. Omitted environment is implicit local host.
`linux-container` is not an exec path. Tools and `workspace_info` have
no `environment_id`.

## Run the gateway

stdio (Cursor / local MCP host):

```bash
export CODESPACE_CONFIG="$PWD/docs/workspaces.example.json"
export CODESPACE_OPERATIONS_DB="$PWD/data/operations.sqlite"
mkdir -p data
./dist/codespace-mcp
```

Streamable HTTP (optional static Bearer — experiment only, never log it):

```bash
export CODESPACE_HTTP_HOST=127.0.0.1
export CODESPACE_HTTP_PORT=8787
# export CODESPACE_HTTP_TOKEN="replace-me"
export CODESPACE_CONFIG="$PWD/docs/workspaces.example.json"
export CODESPACE_OPERATIONS_DB="$PWD/data/operations.sqlite"
./dist/codespace-mcp --http
```

Endpoint: `http://127.0.0.1:8787/mcp`. User Inbox JSON is
`http://127.0.0.1:8787/inbox` on the **same** HTTP listener and Bearer.
stdio-only mode does not expose `/inbox`. Drafts stay off the model
until `POST /inbox/intents/{id}/queue`. Bind address is not the same as a
public `Host` header. For a reverse proxy, allow the external hostname
separately; do not treat `0.0.0.0` as that name.

If `CODESPACE_OPERATIONS_DB` is unset, operations and the intent queue
live in memory and **do not survive restart**. Process handles never
survive restart.

Opt-in runner worker (still host exec, not Linux isolation). UDS is
1:1: the gateway owns `RuntimeProcess` (child, private 0700 directory,
`$dir/runner.sock`). There is no reconnect. Allowed `--runner` /
`CODESPACE_RUNNER` values are `in-process` and `uds` only. `--runner-dir` /
`CODESPACE_RUNNER_DIR` may name a parent for that unique leaf; `/`,
`/tmp`, `/var/tmp`, and `$HOME` are rejected as the directory itself.
`--runner-socket` is only for connecting to an already-running worker
and does not chmod the parent path.

```bash
export CODESPACE_RUNNER=uds
export CODESPACE_RUNNER_DIR="$PWD/data/runner"
export CODESPACE_RUNTIME_BIN="$PWD/dist/codespace-codex-runtime"
./dist/codespace-mcp
```

## Reproduce the MVP flow

Automated coverage (no ChatGPT account required):

```bash
cargo test -p codespace-server --test apply
cargo test -p codespace-server --test process
cargo test -p codespace-server --test protocol_compat
cargo test -p codespace-server --test inbox
cargo test -p codespace-server --test e2e
```

Those tests cover patch apply/disk confirmation and managed processes
(`exec_command` / `read_process` / `WORKSPACE_BUSY`).

Manual stdio: connect an MCP client to `./dist/codespace-mcp` with the
env vars above, then call those tools against `workspace_id: "demo"`
(after the JSON `root` exists and the profile allows writes).

## Linux isolation fixture

[`deploy/compose.yml`](../deploy/compose.yml) is a sleeper fixture. It
bind-mounts **only** the project at `/workspace` as uid `10001`. It does
not mount host `$HOME`, SSH agent sockets, `/var/run/docker.sock`,
gateway `.env`, Bearer files, or the operations SQLite file. It does not
run `codespace-mcp` and is **not** connected to `exec_command`.

```bash
export CODESPACE_WORKSPACE=/absolute/path/to/your/project
docker compose -f deploy/compose.yml up --build
```

The gateway still runs on the host. `exec_command` is a host process
(pipes by default; PTY when `tty` is true) with the workspace as cwd.

## Logs

- Gateway logs go to **stderr** (`RUST_LOG` / `tracing`, default `info`).
- Secret keys (`authorization`, `token`, `bearer`, …) are redacted in
  structured sanitizers. Do not print `CODESPACE_HTTP_TOKEN` in shell
  history docs you share.
- Rotate or truncate stderr capture yourself. There is no log SaaS.
- The operations SQLite file grows with **patch operation** rows plus
  works/intents. Process handles and resource locks are volatile
  memory. Keep the database off any future runner mount. Deleting it
  forgets idempotency keys.

## Recovery after disconnect or restart

HTTP/JSON-RPC request id ≠ `operation_id` ≠ `process_id` ≠ `work_id`. A lost HTTP
response is not an execution failure.

- Call `operation_status` with **exactly one** of the server-minted
  `operation_id` or the client `operation_key` instead of blindly
  re-running `apply_patch`. Providing both or neither is an error.
- After a crash, unfinished rows are `unknown`. The server does **not**
  auto-replay them. A lost UDS `apply_patch` response is stored as
  `unknown`, never a disk-contradicting `rejected`. Inspect the
  workspace, then start a **new** `operation_key` if you still want the
  change.
- A live `exec_command` process can outlive the MCP request. Use
  `read_process` / `terminate_process` with the issued `process_id`.
  Ambiguous transport keeps the process-owned lease. `ProcessExited`
  from the worker calls `release_process` so `WORKSPACE_BUSY` does not
  stick forever. UDS disconnect or gateway shutdown **kills the worker**
  (host children die with it). Confirmed worker death releases
  process-owned leases; `process_id` does not survive. Runner `Replay`
  is a same-connection primitive, not disconnect recovery. After
  gateway restart, old OS PIDs are not reused as CodeSpace handles.

## What this document does not verify

- Installing on someone else's laptop or a cloud VM from this session
- ChatGPT Custom Connector OAuth / public HTTPS `Host` headers
- Kernel escape of the Docker example
