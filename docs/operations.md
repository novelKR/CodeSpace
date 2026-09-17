# Operations (W12)

Reproduce a local CodeSpace from a clean clone: install, start the
gateway, then `workspace_info` → `read` → `apply_patch` → `exec_command`.
This is a **personal single-user** layout. It is not a multi-tenant SaaS
and not a full OAuth server.

ChatGPT Custom Connector against a public hostname is **not verified**.
See [chatgpt-connector.md](chatgpt-connector.md).

## Install

Needs Rust 1.88+, Git, and (for the Linux runner example) Docker.

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# If you already cloned without submodules:
# git submodule update --init --recursive
```

The Codex pin is `third_party/codex` at the commit in
[upstream-lock.md](upstream-lock.md). Do not `git submodule update --remote`
to Codex `main`.

Build both binaries into the **same** directory so the gateway can find
the patch helper next to itself (or set `CODESPACE_PATCH_BIN`):

```bash
cargo build -p codespace-server --bin codespace-mcp --release
cargo build --manifest-path crates/patch/Cargo.toml --bin codespace-patch --release
mkdir -p dist
cp target/release/codespace-mcp dist/
cp crates/patch/target/release/codespace-patch dist/
```

`codespace-patch` hosts the pinned Codex crate **in-process**. It is not
the upstream standalone `apply_patch` binary.

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
is not a product profile.

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

Endpoint: `http://127.0.0.1:8787/mcp`. Bind address is not the same as a
public `Host` header. For a reverse proxy, allow the external hostname
separately; do not treat `0.0.0.0` as that name.

If `CODESPACE_OPERATIONS_DB` is unset, operations live in memory and
**do not survive restart**.

## Reproduce the MVP flow

Automated coverage (no ChatGPT account required):

```bash
cargo test -p codespace-server --test apply
cargo test -p codespace-server --test process
# When this revision includes tests/e2e (W11):
# cargo test -p codespace-server --test e2e
```

Those tests cover patch apply/disk confirmation and managed processes
(`exec_command` / `read_process` / `WORKSPACE_BUSY`).

Manual stdio: connect an MCP client to `./dist/codespace-mcp` with the
env vars above, then call those tools against `workspace_id: "demo"`
(after the JSON `root` exists and the profile allows writes).

## Linux runner example

[`deploy/compose.yml`](../deploy/compose.yml) bind-mounts **only** the
project at `/workspace` as uid `10001`. It does not mount host `$HOME`,
SSH agent sockets, `/var/run/docker.sock`, gateway `.env`, Bearer files,
or the operations SQLite file.

```bash
export CODESPACE_WORKSPACE=/absolute/path/to/your/project
docker compose -f deploy/compose.yml up --build
```

The gateway still runs on the host in MVP. The compose file is the
**execution isolation example**, not a claim that ChatGPT was connected
through it.

## Logs

- Gateway logs go to **stderr** (`RUST_LOG` / `tracing`, default `info`).
- Secret keys (`authorization`, `token`, `bearer`, …) are redacted in
  structured sanitizers. Do not print `CODESPACE_HTTP_TOKEN` in shell
  history docs you share.
- Rotate or truncate stderr capture yourself. There is no log SaaS.
- The operations SQLite file grows with apply/exec records. Keep it off
  the runner mount. Deleting it forgets idempotency keys.

## Recovery after disconnect or restart

HTTP/JSON-RPC request id ≠ `operation_id` ≠ `process_id`. A lost HTTP
response is not an execution failure.

- Call `operation_status` with the server-minted id or `operation_key`
  instead of blindly re-running `apply_patch`.
- After a crash, unfinished rows are `unknown`. The server does **not**
  auto-replay them. Inspect the workspace, then start a **new**
  `operation_key` if you still want the change.
- A live `exec_command` process can outlive the MCP request. Use
  `read_process` / `terminate_process` with the issued `process_id`.
  After gateway restart, old OS PIDs are not reused as CodeSpace
  handles.

## What this document does not verify

- Installing on someone else's laptop or a cloud VM from this session
- ChatGPT Custom Connector OAuth / public HTTPS `Host` headers
- Kernel escape of the Docker example
