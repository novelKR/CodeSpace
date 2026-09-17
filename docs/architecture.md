# Architecture

[English](architecture.md) | [한국어](ko/architecture.md)

CodeSpace is a personal **execution-tools MCP server**. An outer client
(ChatGPT, Cursor, or another MCP host) decides what to do. This process
never calls a model. It reads files, applies Codex V4A patches through a
pinned Rust engine, and runs managed commands in a registered workspace.

## Identity

| Is | Is not |
| --- | --- |
| Independent MCP server | Fork of CoS or cokacremote |
| Execution-only environment + contract | Codex agent / App Server wrapper |
| File read / patch / managed exec | Internal chat, Goal/Loop, multi-agent, Responses API |
| Gateway policy + (target) OS/container isolation | Kernel sandbox equivalent to Codex CLI |

Borrowed ideas (not code dumps):

- From cokacremote: headless server, Streamable HTTP, separating **request
  lifetime** from **process lifetime**.
- From CoS: approved workspaces, per-hunk path resolve before the engine,
  preflight, best-effort rollback, split tool surface.
- From Codex: original Rust `codex-apply-patch` parse / verify / apply,
  and later a cohesive **execution subgraph** (hardening, PTY, UDS,
  path, filesystem, Linux sandbox, network) isolated behind the
  Runner. Codex is an implementation dependency, not the control plane
  ([codex-reuse.md](codex-reuse.md)).

Not taken: Electron, Chrome extension, ChatGPT DOM, agents spawn, Desktop,
plugin marketplace, TypeScript `apply-patch` port, `git apply --unsafe-paths`,
wrapping the standalone `apply_patch` binary as the security boundary
(that path uses sandbox `None` and follows symlinks by default), a
TypeScript MCP gateway, the retired `native/patch-worker` tree, or any
outbound model client. Execution-only invariant:
[execution-substrate.md](execution-substrate.md).

## Current layout

MVP default is **one host process**: `codespace-mcp` plus in-process
`Runner`. `exec_command` is not dispatched into compose.
[`deploy/compose.yml`](../deploy/compose.yml) is an isolation **fixture**
only. Opt-in Unix-socket transport (`CODESPACE_RUNNER=uds`) talks
CodeSpace JSON to `codespace-codex-runtime`; that is not Linux
isolation and not the default.

```text
CURRENT

MCP Client
   │
   ▼
codespace-mcp  (host gateway)
   ├─ policy / store / coordination / operation persist
   ├─ structured logging (stderr tracing)
   │
   │  Runner execution DTO
   │  (command/exec shape; no work_id / operation_id / coordination)
   ▼
RuntimeBackend
   ├─ default: InProcessRunner
   └─ opt-in: UdsRunner (CODESPACE_RUNNER=uds)
          │ length-prefixed CodeSpace JSON (protocol 1, request_id rrpc-…)
          ▼
     codespace-codex-runtime
          ├─ codex-process-hardening
          ├─ codex-uds bind
          └─ InProcessRunner (same methods as default)
                 ├─ read / find / version (PathSandbox)
                 ├─ apply_patch (one transaction)
                 │      expected versions → preflight → snapshot
                 │      → helper apply → verify → rollback
                 │              │ JSON stdin/stdout
                 │              ▼
                 │         codespace-patch (host child)
                 │              └─ Codex Rust crate in-process
                 └─ exec / stdin / read / terminate
                        └─ host process (tokio::process::Command,
                           cwd = workspace root, env from runner-local defaults)

deploy/compose.yml
   └─ isolation fixture only; not connected to exec_command
```

```text
MCP JSON  →  domain params  →  gateway (policy/store)  →  Runner DTO  →  RuntimeBackend
                 │
                 └─ rmcp / JsonSchema stay on MCP types, not on runner DTOs
```

The gateway owns **who may do what in which workspace**. Tokens, server
config, workspace registry, environments, and the operations database
live here. It maps MCP params onto runner DTOs and does **not** pass
`ExecCommandParams` into the runner. Tools still have no
`environment_id`.

`crates/runner` owns the `Runner` trait, execution DTOs,
`InProcessRunner` (filesystem, one `apply_patch` transaction, host
process supervisor), `UdsRunner` (Unix-socket client), and
compose-fixture checks. Default backend is in-process. The worker
binary is isolated `crates/codex-runtime` / `codespace-codex-runtime`.

`codespace-patch` is a product helper process, not the upstream
standalone `apply_patch` binary and not `native/patch-worker`.
Runner ↔ helper is JSON stdin/stdout. Codex itself runs in-process
**inside that helper**.

Single instance is enough for MVP. SQLite stores **patch operations**
plus works/intents. Process handles and resource locks (workspace
exclusive write / shell occupancy) are in-memory. No message broker.

Gateway unit tests may run on the macOS development host. A Linux
container is the **target** isolation OS, not the current exec boundary.
Registered `linux-container` environments fail closed (`UNAUTHORIZED`).

## Target layout

A later runner split stays Rust on both sides. Do not reintroduce a
TypeScript gateway or `native/patch-worker` just to call Codex.

```text
TARGET

MCP Client
   │
   ▼
Gateway
   │ authorized RunnerRequest
   │ (policy, operation_key replay, write lock,
   │  dispatch, result persistence)
   ▼
Runner process boundary
   │
   ├─ filesystem
   ├─ patch transaction (one runner-side operation)
   └─ process supervisor
          │
          ▼
   isolated Linux workspace
```

Target **domain** (not live MCP fields): Environment (where), Workspace
(what), PermissionProfile (may), Operation (this RPC). Do not add
`environment_id` to tools. Operator config may register environments.
See [execution-substrate.md](execution-substrate.md).

Unix-socket **transport** (`UdsRunner`) exists behind the existing
`Runner` / `InProcessRunner` types as an opt-in. Host + UDS is the same
host; it does not claim Linux isolation. `LinuxContainer` stays
fail-closed (`UNAUTHORIZED`, no `operation_id`). It must **not** split
patch apply into multiple gateway-driven RPCs:

```text
Runner.apply_patch(request)
  expected versions → path policy → preflight → snapshot
  → Codex apply → after-version verify → rollback on failure
```

Gateway keeps authorization, `operation_key` replay, the write lock,
dispatch, and persistence. Default remains `InProcessRunner`. Opt-in
`CODESPACE_RUNNER=uds` uses a private Unix socket, not the compose
fixture.

Sandbox, PTY, and network isolation are **not** “reimplement Codex OS
engineering by default.” Prefer a cohesive execution subgraph isolated
behind the Runner, same pattern as `crates/patch` and
`crates/codex-runtime` / `codespace-codex-runtime`. This WP takes
`codex-process-hardening` and `codex-uds`. Codex types stay in the
adapter. Do not embed App Server or `codex-exec`. `codex-exec-server`
is a future measurement, not a current backend.

## Protocol compatibility

Core execution is **MCP 2025-11-25** over **stdio** and **Streamable HTTP
`/mcp`**. Required primitives are `initialize`, `tools/list`, and
`tools/call`. HTTP's spec floor is 2025-03-26 (Streamable HTTP exists);
CI pins 2025-11-25. **2026-07-28 is progressive enhancement only.**

Core must not require MRTR, Tasks, subscriptions, `Mcp-Name` routing, or
0728 stateless lifecycle as application state. Auth stays optional Bearer
middleware; dispatch is `/mcp` → rmcp tools. `ProtocolVersion` and
`NegotiatedFeatures` live in `crates/server` only.

2024-11-05 HTTP+SSE is not a goal. Full matrix:
[protocol-compatibility.md](protocol-compatibility.md).

## MVP tools

Execution tools:

| Tool | Role |
| --- | --- |
| `workspace_info` | Selector metadata, profile, roots (not a credential) |
| `read` | File contents + version |
| `find` | Relative-path search |
| `apply_patch` | Codex V4A only |
| `exec_command` | Start a managed **host** process |
| `write_stdin` | Write to a managed process |
| `read_process` | Cursor-based output |
| `terminate_process` | Kill a server-issued handle |
| `operation_status` | Recover by `operation_id` **or** `operation_key` |

Coordination tools (ordinary `tools/call`, MCP 2025-11-25 first-class):

| Tool | Role |
| --- | --- |
| `work_open` | Mint a `work_id` for one logical job |
| `steer_status` | Counts only; no intent bodies |
| `steer_claim_next` | Atomically claim one queued item |
| `steer_complete` | Mark claimed intent done or blocked |
| `work_finish` | Close only if the queue is drained |

Users edit drafts and reorder queued items on HTTP `/inbox`, not MCP.
Intent bodies are instructions, never capabilities.

No internal model-calling tool exists. `git_apply_patch` is out of MVP.
Error codes and transport-vs-execution rules: [error-codes.md](error-codes.md).
Linux isolation fixture: [runner-isolation.md](runner-isolation.md).
Codex product vs primitive: [codex-reuse.md](codex-reuse.md).
Execution-only substrate: [execution-substrate.md](execution-substrate.md).

## IDs

HTTP/JSON-RPC request id, `operation_id`, `operation_key`, `process_id`,
`work_id`, `intent_id`, and runner `request_id` (`rrpc-…`) are different
identifiers. A lost HTTP response is not an execution failure. A lost
UDS `apply_patch` response is recorded as `unknown`, not `rejected`.
Clients call `operation_status` instead of replaying a mutating tool.

```text
Workspace (workspace_id)
  └── Work (work_id)
        ├── Operation (operation_id)
        ├── Process (process_id)
        └── User Intent Queue (intent_id)
```

`workspace_id` and `work_id` are **selectors**, never proof of
authorization. Client arguments `approved: true` and `user_id` are
ignored. User-intent text does not raise the permission profile.

## Patch apply pipeline

Gateway keeps authorization, `operation_key` replay, the write lock,
and persistence. `InProcessRunner.apply_patch` runs the execution
transaction as a single call. Gateway does not split it into preflight /
snapshot / apply RPCs.

```text
validate request
  → auth + workspace policy
  → operation_key replay / conflict
  → workspace write lock
  → Runner.apply_patch
        expected_versions
        → full preflight (no writes)
        → save rollback snapshot
        → original engine apply (`apply_patch_with_options`)
        → verify disk hash == helper claimed after_version
        → rollback on failure
  → persist operation status (gateway fills operation_id)
  → MCP response
```

`crates/patch` (inside `codespace-patch`) calls `parse_patch`, then
product policy, then `apply_patch_with_options` **in-process**. It does
not wrap the standalone `apply_patch` binary and does not reimplement
the parser.

`apply_patch` never silently falls back to `git apply`. Status values
are `applied` / `checked` / `rejected` / `failed_rolled_back` /
`failed_partial` / `unknown`. `checked` is a successful `check_only`
preview (no writes). `rejected` is an actual refusal. Transport
ambiguity (socket drop after dispatch) finishes `unknown` and must not
be stored as `rejected`. Success copy without after-version verification
is forbidden.

Rollback must not use `git reset --hard` and must not overwrite a whole
directory tree as a substitute for per-file restore.

## Repository layout

```text
Cargo.toml              workspace root
crates/server/          bin codespace-mcp: rmcp stdio + Streamable HTTP + /inbox
crates/domain/          workspace, capabilities, operation, errors (no rmcp)
crates/policy/          registry, PermissionProfile, Environment
crates/patch/           Codex adapter + codespace-patch helper (own workspace)
crates/codex-runtime/   isolated worker: hardening + UDS + InProcessRunner
crates/store/           SQLite operations, works, intents; in-memory resource locks
crates/runner/          Runner trait + execution DTOs, PathSandbox, patch transaction, host supervisor, UdsRunner, fixture checks
third_party/codex/      git submodule, pinned revision (W06)
tests/{security,recovery,e2e}/
docs/                   including operations.md (W12), codex-reuse.md,
                        execution-substrate.md (W17)
deploy/                 unprivileged Linux isolation fixture
```

## Out of scope (initial)

Browser extension, ChatGPT DOM automation, Goal/Loop, multi-agent,
Desktop control, plugin marketplace, a full OAuth server, automatic
unified-diff conversion, forwarding the entire Codex App Server RPC,
internal model calls, TypeScript MCP SDK, `native/patch-worker`.
