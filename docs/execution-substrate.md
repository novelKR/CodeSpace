# Execution substrate

CodeSpace is an **execution-only MCP**. ChatGPT (or another MCP host)
plans and writes code. This process never calls a model, never calls
the OpenAI Responses API, and never runs an agent loop.

```text
ChatGPT
  판단 / 계획 / 코드 생성
        │ MCP tools/call
        ▼
Headless execution MCP
  no model, no Responses API, no agent loop
        │
  patch / exec / fs / sandbox / permissions / audit
        ▼
Filesystem / OS / container / (later) remote runner
```

Take a Codex App Server idea only if it answers **yes**:

> Can this provide execution, permission, state, or observation
> **deterministically, with no model**?

If it needs prompt, context, turn, reasoning, review, or a model
catalog, leave it out. Do not translate App Server JSON-RPC into MCP.
Extract substrate; re-expose it as CodeSpace tools.

Crate-level take/leave: [codex-reuse.md](codex-reuse.md). Pin:
[upstream-lock.md](upstream-lock.md) (`6b9826e`, `rust-v0.154.0`).
Survey notes that mention Codex `main` `4701aa4b` are **not** a pin
bump. Re-check graphs after a deliberate W13 update.

CI: `scripts/check-no-model-deps.sh` (root workspace crates only).

## Invariant

| Must | Must not |
| --- | --- |
| MCP tools for read, patch, exec, process, operations | Responses / Chat Completions client |
| Gateway as the only allow path | Codex session `permissionProfile` as allow |
| Workspace-relative MCP paths | Absolute paths on the wire |
| Process handles that outlive an MCP connection | Copy App Server “kill on connection close” |
| Isolated `crates/patch` → `codex-apply-patch` | Embed `codex-app-server` / `codex-exec` / `codex-core` |

`read-only` / `workspace-write` stay the live profiles. Richer
filesystem glob + network axes are a **future `crates/policy` type**,
not an import of Codex user config.

## Four axes (target domain)

Not all of these are MCP fields today. **Do not add `environment_id`
to live tools in this work package.**

```text
Environment   where command and filesystem ops run
Workspace     which tree inside that environment is in scope
PermissionProfile  what that pair may do (gateway-owned)
Operation     this mutating RPC (id, key, persist, audit)
      ↓
Process / Patch / FS
```

- **Environment** is not an agent. Local host, a Linux container, or a
  later remote runner are environments. Registration is a control-plane
  / operator action. The model must not supply `execServerUrl`.
- **Workspace** stays the selector on MCP (`workspace_id` + relative
  path). Internally the runner may resolve to an absolute path.
- **PermissionProfile** shape (Read / Write / Deny on path, glob, or
  special roots; separate network axis) may follow App Server. The
  **engine that grants** is CodeSpace policy.
- **Operation** is already `operation_id` / `operation_key` /
  `operation_status`. Diff/audit ledger is P1, not conversation
  history.

```text
MCP virtual path
      ↓
WorkspaceResolver
      ↓
absolute path (internal)
      ↓
Runner / patch helper
```

## `command/exec`: shape vs crates

App Server `command/exec` at the **pin**
([`command_exec.rs`](../third_party/codex/codex-rs/app-server-protocol/src/protocol/v2/command_exec.rs))
is a **standalone** argv API: no thread, no turn. Fields include argv,
optional process id, tty, stdin/stdout streaming, output cap, timeout,
cwd, env, PTY size, `sandboxPolicy` / `permissionProfile`. Follow-ups:
write, resize, terminate. Streaming is `outputDelta`.

That **shape** is the long-term Runner DTO target (plus
`process_resize` when PTY exists). Live MCP remains:

```text
exec_command / write_stdin / read_process / terminate_process
```

Do **not** take `codex-exec` or `codex-exec-server` (product runtime;
see [codex-reuse.md](codex-reuse.md)). Do **not** default sandbox
policy from “the Codex user’s config.” Gateway maps an already-allowed
request onto runner DTOs. PTY helper candidate remains
`codex-utils-pty`.

App Server streaming processes are connection-scoped and die when that
connection closes. CodeSpace keeps **request lifetime ≠ process
lifetime**. `process_id` is server-minted and stored as application
state. Later disconnect policy may be continue / terminate /
grace-period — not “socket closed ⇒ kill.”

## Approval and MCP revision

Insufficient permission is a **policy refusal** today (`ErrorBody`),
not a silent grant from `{ "network": true }` in tool args.

Core protocol stays **MCP 2025-11-25** `tools/call`
([protocol-compatibility.md](protocol-compatibility.md)). MRTR and
Tasks are 2026-07-28 progressive enhancement. They must not become
required for exec or patch.

When extra permission is designed later:

1. Prefer explicit tools (`approval_create` / `approval_resolve` /
   `operation_resume`) so 2025-11-25 clients work.
2. Optionally map the same state onto MRTR `input_required` for 0728
   hosts.

Human / configured policy sits between the model request and OS exec.
No model is invoked to decide the grant.

Long-running **non-interactive** jobs may later use MCP Tasks;
**interactive** jobs keep `process_id`. Tasks must not replace process
handles.

## Scheduler (after the single write lock)

Today one workspace write lock plus shell occupancy is enough. App
Server serializes by resource (exclusive vs shared read). The target
scopes are Environment, Workspace, Path, Process, Operation, Watch —
not Thread. Do not change `crates/store` in this work package.

## `fs/watch` and search

Do not expose `fs/watch` as a model tool. Use it internally so an
external editor bump invalidates versions and `apply_patch` can fail
`expected_versions` / a future `STALE_READ`.

Fuzzy search **sessions** are TUI typing UX. Keep MCP as `find` /
later `find_files(query, workspace_id, limit)`. Reuse an engine later
if the crate graph is as narrow as apply-patch; drop the session
protocol.

## Hooks and skills

Hooks are allowed only if they are local, deterministic, and cannot
call a model (`before_patch` policy, `after_patch` fmt, audit). A hook
that reviews code via Responses API is forbidden.

Skills are not auto-injected into a hidden agent. If added, they are
MCP resources or prompts the **host** chooses to read.

Downstream MCP federation (this server as MCP client) is P3: no model,
but auth and tool-name collision are expensive.

## Take / leave (concepts)

| Take (substrate) | Leave (agent runtime) |
| --- | --- |
| V4A parse/verify/apply | `thread/*`, `turn/*`, steer-as-turn |
| Standalone command/exec **shape** | `codex-exec` crate, App Server embed |
| Process manager / PTY helper | Connection-scoped process death |
| Sandbox **policy object** (gateway fills) | User Codex config as default allow |
| Permission profile **shape** in `crates/policy` | `permissionProfile` from the model or Codex session |
| Environment as exec location | Agent / account / model provider |
| Resource serialization | Thread-keyed queues |
| Internal fs/watch | Watch as an MCP tool |
| Search engine, not session RPC | Absolute-path `fs/writeFile` on the wire |
| Deterministic hooks | Hook → model |
| Operation / diff / audit | Conversation compaction, memory, review, Guardian, multi-agent, Goal |

Concept maps (do not import the types): Thread → workspace/operation
history; Turn → Operation; Interrupt → cancel; Turn diff →
`operation_diff`; Approval → policy + human; Attachment → artifact
resource.

## Roadmap (implementation later)

This work package is documentation and a dependency check only.

**P0** — already or next code WPs: `codex-apply-patch` (done), exec
runtime **shape** on Runner DTOs, PermissionProfile domain in
`crates/policy`, Environment domain (operator-registered; not a tool
arg yet), resource serializer, sandbox crate **judgment** then
transport (`ContainerRunner`).

**P1** — operation state machine / diff ledger, approval fallback
tools, internal watch, richer process handles (resize, caps),
disconnect policy.

**P2** — `find` quality / index, deterministic hooks, skills as
resources or prompts.

**P3** — remote environment, MCP federation, artifact registry.

The next **code** WP remains Runner **transport** behind the existing
trait, without splitting `apply_patch` into gateway RPCs. Sandbox / PTY
/ network are not a default homegrown OS stack
([codex-reuse.md](codex-reuse.md)).
