# Runner isolation

[English](runner-isolation.md) | [한국어](ko/runner-isolation.md)

There are three separate questions: which process runs the work, whether commands are sandboxed, and whether execution moves into a container. Today the first two are implemented; container dispatch is not.

<a id="later-process-split"></a>

## Host and worker execution

`in-process` runs the supervisor inside `codespace-mcp`. `uds` (Unix domain socket) runs that supervisor inside `codespace-codex-runtime` on the same host. The worker starts with Codex process hardening and binds a private Unix socket. Hardening the worker does not sandbox its commands.

The gateway creates a unique 0700 directory beneath its temporary directory or `CODESPACE_RUNNER_DIR`. The internal protocol is CodeSpace JSON with a u32 length prefix, handshake version 5, request IDs, and process-exit events. It is not Codex App Server RPC. Same-connection replay is not reconnect recovery.

Managed process lifetime is owned by the runner instance, not the MCP connection. Streamable HTTP or MCP client disconnect keeps processes running. Gateway/worker UDS disconnect or gateway shutdown (including stdio EOF) ends the owned worker and its children; process handles are lost. Restart does not restore `process_id`.

<a id="macos-no-docker"></a>

## Linux command sandbox

Both pipe and PTY execution use the same Linux helper path. On Linux, the runner probes `CODESPACE_LINUX_SANDBOX_BIN` or a sibling `codespace-linux-sandbox` binary. A successful probe enables preparation and sandboxed execution. The probe result is cached for that process.

```text
Runner → helper prepare (CodeSpace JSON, protocol 1)
       ← opaque plan pathname
Runner → managed helper run --plan
       → Codex sandbox setup → requested command
```

Codex permission translation and sandbox arguments stay inside the binary-only helper. The plan is a private 0600 file consumed by the helper. The runner depends on the small protocol crate, not the sandbox implementation library. Restricted execution uses self-exec; enabled execution keeps a helper-owned proxy while waiting for its sandbox child.

A failed initial probe permits unsandboxed host execution for `restricted`; the contract reports `command_sandbox: none` and no OS network enforcement. `enabled` requires the helper and fails without it. Once a probe succeeds, later prepare/protocol/spawn errors never fall back to unsandboxed execution. They report `PROCESS_SPAWN_FAILED`. Failure inside an already-started helper is observed as process termination through `process_status`. That wait status belongs to the managed child (helper argv) and is not documented as identical to the user argv.

## Network and filesystem scope

Restricted mode uses network namespace separation and restricted seccomp rules. Enabled mode uses an isolated network namespace with a managed HTTP proxy. Proxy bypass environment entries are cleared so loopback HTTP also follows the proxy. Direct host networking is not the fallback. Destination domain restrictions and compatibility with every network client are not implemented guarantees.

Read the effective `workspace_info.execution` fields. File tools stay workspace-scoped independently of command sandboxing. `codespace-fs` provides no-follow I/O behind logical path checks. The patch helper has its own path checks and Codex patch options; command sandboxing does not automatically wrap all file tools.

Sandboxed commands use a limited system PATH rather than mounting host toolchains from the user's home. Prepare the required compilers, package caches, and binaries in the execution environment. A command that works on the host may fail in the sandbox because its executable or dependency is unavailable.

<a id="what-the-compose-fixture-does"></a>

## Container fixture and verification

`deploy/compose.yml` runs a non-root sleeper with only the selected workspace mounted at `/workspace`. It does not install the server, launch a Runner, or receive `exec_command` calls. Never describe starting this fixture as activating the MCP command sandbox.

CI has a Linux isolation job that installs bubblewrap and requires helper availability for isolation tests. macOS checks cover host behavior, not Linux enforcement. Kernel escapes, Docker Desktop differences, and arbitrary remote deployments need separate assessment. See [operations](operations.md) for setup and [security model](security-model.md) for trust assumptions.
