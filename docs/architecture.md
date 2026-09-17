# Architecture

CodeSpace is a personal **execution-tools MCP server**. An outer client
(ChatGPT, Cursor, or another MCP host) decides what to do. This process
never calls a model. It reads files, applies Codex V4A patches through a
pinned Rust engine, and runs commands in an isolated Linux environment.

## Identity

| Is | Is not |
| --- | --- |
| Independent MCP server | Fork of CoS or cokacremote |
| Execution environment + contract | Codex agent wrapper |
| File read / patch / isolated exec | Internal chat, Goal/Loop, multi-agent |
| Gateway policy + OS/container isolation | Kernel sandbox equivalent to Codex CLI |

Borrowed ideas (not code dumps):

- From cokacremote: headless server, Streamable HTTP, separating **request
  lifetime** from **process lifetime**.
- From CoS: approved workspaces, per-hunk path resolve before the engine,
  preflight, best-effort rollback, split tool surface.
- From Codex: original Rust `codex-apply-patch` parse / verify / apply.

Not taken: Electron, Chrome extension, ChatGPT DOM, agents spawn, Desktop,
plugin marketplace, TypeScript `apply-patch` port, `git apply --unsafe-paths`,
wrapping the standalone `apply_patch` binary as the security boundary
(that path uses sandbox `None` and follows symlinks by default), a
TypeScript MCP gateway, or native/patch-worker JSON IPC.

## Control plane vs execution plane

Both planes are **Rust**. Language is not a security boundary. Isolation is
gateway policy plus the Linux runner process (in-process for MVP; Unix
socket later if a split is required).

```text
ChatGPT / other MCP client
             │
             │ stdio  or  HTTPS + optional Bearer
             ▼
┌──────────────────────────────────────────┐
│ MCP Gateway — Rust (rmcp, crates/server) │
│  tool schemas, auth check, policy        │
│  operation store, audit, result limits   │
└───────────────────┬──────────────────────┘
                    │ in-process now;
                    │ Unix socket later
                    ▼
┌──────────────────────────────────────────┐
│ Workspace Runner — isolated Linux, Rust  │
│  filesystem + process supervisor         │
│  crates/patch calls Codex in-process     │
│  reachable tree: /workspace              │
└──────────────────────────────────────────┘
```

```text
MCP JSON  →  domain request  →  PatchService / ProcessService  →  domain result  →  MCP
                 │
                 └─ rmcp types stay inside crates/server
```

The gateway owns **who may do what in which workspace**. Tokens, server
config, workspace registry, and the operations database live here.

The runner owns **performing an allowed action**. Model-authored shell and
patched project files run here. Gateway secrets, `.env`, SQLite, and SSH
must not be mounted into the runner.

MVP is **one process**: `codespace-mcp`. If isolation needs a process
boundary later, `crates/server` and `crates/runner` both build from this
Cargo workspace. Do not reintroduce a TypeScript gateway or a separate
patch-worker IPC just to call Codex.

Single instance is enough for MVP. SQLite for operations is allowed. No
message broker.

Gateway unit tests may run on the macOS development host. **Execution
isolation OS is a Linux container.**

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

## MVP tools (9)

| Tool | Role |
| --- | --- |
| `workspace_info` | Selector metadata, profile, roots (not a credential) |
| `read` | File contents + version |
| `find` | Relative-path search |
| `apply_patch` | Codex V4A only |
| `exec_command` | Start a managed process |
| `write_stdin` | Write to a managed process |
| `read_process` | Cursor-based output |
| `terminate_process` | Kill a server-issued handle |
| `operation_status` | Recover after disconnect; do not re-run blindly |

No internal model-calling tool exists. `git_apply_patch` is out of MVP.
Error codes and transport-vs-execution rules: [error-codes.md](error-codes.md).
Linux runner isolation: [runner-isolation.md](runner-isolation.md).

## IDs

HTTP/JSON-RPC request id, `operation_id`, and `process_id` are three
different identifiers. A lost HTTP response is not an execution failure.
Clients call `operation_status` instead of replaying a mutating tool.

`workspace_id` is a **selector**, never proof of authorization. Client
arguments `approved: true` and `user_id` are ignored.

## Patch apply pipeline

```text
validate request
  → auth + workspace policy
  → operation_key replay / conflict
  → workspace write lock
  → original Codex parser (`parse_patch`)
  → every source and destination path
  → expected_versions
  → full preflight (no writes)
  → save rollback snapshot
  → original engine apply (`apply_patch_with_options`)
  → verify disk == claimed result
  → persist operation status
  → MCP response
```

`crates/patch` calls `parse_patch`, then product policy, then
`apply_patch_with_options` **in-process**. It does not wrap the standalone
`apply_patch` binary and does not reimplement the parser.

`apply_patch` never silently falls back to `git apply`. Status values are
`applied` / `rejected` / `failed_rolled_back` / `failed_partial` /
`unknown`. Success copy without disk verification is forbidden.

Rollback must not use `git reset --hard` and must not overwrite a whole
directory tree as a substitute for per-file restore.

## Repository layout

```text
Cargo.toml              workspace root
crates/server/          bin codespace-mcp: rmcp stdio + Streamable HTTP
crates/domain/          workspace, capabilities, operation, errors (no rmcp)
crates/policy/          registry and path policy
crates/patch/           Codex adapter + rollback (W06)
crates/fs/              read / find / versions
crates/exec/            process supervisor
crates/store/           SQLite operations
crates/runner/          later process split; MVP called in-process
third_party/codex/      git submodule, pinned revision (W06)
tests/{contract,parity,security,recovery,e2e}/
docs/                  including operations.md (W12)
deploy/                 unprivileged Linux runner example
```

## Out of scope (initial)

Browser extension, ChatGPT DOM automation, Goal/Loop, multi-agent,
Desktop control, plugin marketplace, a full OAuth server, automatic
unified-diff conversion, forwarding the entire Codex App Server RPC,
internal model calls, TypeScript MCP SDK, native/patch-worker IPC.
