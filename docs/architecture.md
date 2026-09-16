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
(that path uses sandbox `None` and follows symlinks by default).

## Control plane vs execution plane

```text
ChatGPT / other MCP client
             │
             │ stdio  or  HTTPS + optional Bearer
             ▼
┌──────────────────────────────────────────┐
│ MCP Gateway — TypeScript                 │
│  tool schemas, auth check, policy        │
│  operation store, audit, result limits   │
└───────────────────┬──────────────────────┘
                    │ internal RPC
                    ▼
┌──────────────────────────────────────────┐
│ Workspace Runner — isolated Linux        │
│  filesystem + process supervisor         │
│  Rust patch worker (pinned Codex crate)  │
│  reachable tree: /workspace              │
└──────────────────────────────────────────┘
```

The gateway owns **who may do what in which workspace**. Tokens, server
config, workspace registry, and the operations database live here.

The runner owns **performing an allowed action**. Model-authored shell and
patched project files run here. Gateway secrets, `.env`, SQLite, and SSH
must not be mounted into the runner.

Single instance is enough for MVP. SQLite for operations is allowed. No
message broker.

Gateway unit tests may run on the macOS development host. **Execution
isolation OS is a Linux container.**

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
  → original Codex parser
  → every source and destination path
  → expected_versions
  → full preflight (no writes)
  → save rollback snapshot
  → original engine apply
  → verify disk == claimed result
  → persist operation status
  → MCP response
```

`apply_patch` never silently falls back to `git apply`. Status values are
`applied` / `rejected` / `failed_rolled_back` / `failed_partial` /
`unknown`. Success copy without disk verification is forbidden.

Rollback must not use `git reset --hard` and must not overwrite a whole
directory tree as a substitute for per-file restore.

## Repository layout

```text
server/                 TypeScript MCP gateway
native/patch-worker/    thin Rust adapter (no parser rewrite)
runner/                 filesystem + process supervisor
third_party/codex/      git submodule, pinned revision (W06)
tests/{contract,parity,security,recovery,e2e}/
docs/
deploy/
```

## Out of scope (initial)

Browser extension, ChatGPT DOM automation, Goal/Loop, multi-agent,
Desktop control, plugin marketplace, a full OAuth server, automatic
unified-diff conversion, forwarding the entire Codex App Server RPC,
internal model calls.
