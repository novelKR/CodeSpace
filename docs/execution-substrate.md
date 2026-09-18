# Execution substrate

[English](execution-substrate.md) | [한국어](ko/execution-substrate.md)

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

Authorization (**WHO MAY**) stays the Gateway. Safe execution (**HOW
SAFE**) is supplied by a pinned Codex **execution subgraph** behind
the Runner (implementation dependency, not an architectural one).
Core crates must not import Codex types, including `codex-protocol`.
The adapter may. Take/leave table:
[codex-reuse.md](codex-reuse.md). Pin:
[upstream-lock.md](upstream-lock.md) (`6b9826e`, `rust-v0.154.0`).
Survey notes that mention Codex `main` `4701aa4b` are **not** a pin
bump. Re-check graphs after a deliberate W13 update.

CI: `policy-scan` job runs `scripts/check-no-model-deps.sh` **without**
submodules, in parallel with `rust`. Core manifests may not declare
`codex-*` deps. Core sources keep the agent/model grep. Isolated
adapter manifests (`crates/patch`, `crates/codex-runtime`, `crates/pty`,
`crates/file-system`) use an
**allowlist**; `third_party/codex` sources are never scanned.
`SCAN_BASE` limits the tree to the update range; unknown range scans
all core crates and adapter manifests. Clippy/tests still always run.
The rust job checks the pin SHA (`PIN_ONLY=1`) **before** fmt/clippy.

## Invariant

| Must | Must not |
| --- | --- |
| MCP tools for read, patch, exec, process, operations | Responses / Chat Completions client |
| Gateway as the only allow path | Codex session `permissionProfile` as allow |
| Workspace-relative MCP paths | Absolute paths on the wire |
| Process handles that outlive an MCP connection | Copy App Server “kill on connection close” |
| Isolated `crates/patch` → `codex-apply-patch` | Embed `codex-app-server` / `codex-exec` / `codex-core` |

`read-only` / `workspace-write` stay the live MCP profiles. Richer
filesystem glob + network axes live in `crates/policy` as
`PermissionProfile`, mapped from those profiles. `process_exec` is the
Exec axis (`read-only` denies, `workspace-write` allows). Path globs are
**domain only**; live enforcement stays coarse `allow(Write|Exec)` plus
PathSandbox. The network axis is recorded only; it does not grant. This
is not an import of Codex user config.

## Four axes (target domain)

Not all of these are MCP fields today. **Do not add `environment_id`
to live tools.** Operator JSON may register environments. Omitted
environment is the implicit local host. Unknown environment ids fail
config load. `linux-container` loads but exec/patch fail closed.

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
  special roots; `process_exec`; separate network axis) may follow App
  Server. Globs are expressed, not live-enforced. The **engine that
  grants** is CodeSpace policy.
- **Operation** is already `operation_id` / `operation_key` /
  `operation_status`. Diff/audit ledger is P1, not conversation
  history.

```text
MCP virtual path
      ↓
WorkspaceResolver / CodeSpace path scope
      ↓
absolute path (internal; later Codex AbsolutePath / PathUri in the adapter)
      ↓
Runner / patch helper
```

## PathSandbox vs codespace-fs

`PathSandbox` is logical authorization and workspace selection
(relative path, stay inside the root, reject `..`, reject leaf and
ancestor symlinks and special files). Its pre-check `symlink_metadata`
is **not** the I/O safety boundary.

The shared I/O primitive is Codex `LOCAL_FS` with
`follow_symlinks: false`. `codespace-fs` is the Runner adapter for
read, find, version, mkdir, chmod, remove, and rollback. `apply_patch`
mutation goes `crates/patch` → `apply_patch_with_options` on the same
`LOCAL_FS` pin (`sandbox: None`). Helper preflight and post-hash may
still use `std::fs`.

`sandbox: None` means OS command sandbox is not reused as a file-tool
authorizer. It does **not** mean unbounded I/O. File-tool workspace
scope, command sandbox, and network enforcement stay separate axes.

Live `exec_command` may run while `read` / `find` are allowed
(`read_while_process_live`, `find_while_process_live`). A process can
replace a directory with a symlink between PathSandbox's lstat and the
open. Safety at that moment is no-follow `LOCAL_FS` I/O, not the
earlier lstat.

`find` walks with upstream caps (depth 64, 10,000 directories, 50,000
entries), then applies CodeSpace glob and the user limit. `truncated`
is true if the walk was cut or the filtered list exceeds the limit.
Hidden directories are not pruned (`prune_hidden_directories: false`).
Stopping the walk at the user limit when there is no glob is a P1
optimization; P0 keeps the bounded full walk.

Operator-registered `workspace.root` is the trust anchor. `find` and
ancestor checks may `canonicalize` that root. Descendants under it are
never followed.

`codespace-fs` `FsError` maps 1:1 onto product codes. `NotFound` is
`FILE_NOT_FOUND`, `NotDirectory` is `PATH_NOT_DIRECTORY`, generic `Io`
is `FILE_OPERATION_FAILED`, `SymlinkRejected` is `SYMLINK_REJECTED`,
and `NotRegularFile` is `SPECIAL_FILE_REJECTED`. `PATH_ESCAPE` is a
workspace/path containment violation: a `../` or absolute request, or a
walk result that cannot `strip_prefix` the workspace root. `find` root
canonicalize failure is `FILE_OPERATION_FAILED` (containment held; the
operation failed). Rollback filesystem failures use the same mapping;
they are not `INVALID_PATCH`.

## `command/exec`: shape vs crates

App Server `command/exec` at the **pin**
([`command_exec.rs`](../third_party/codex/codex-rs/app-server-protocol/src/protocol/v2/command_exec.rs))
is a **standalone** argv API: no thread, no turn. Fields include argv,
optional process id, tty, stdin/stdout streaming, output cap, timeout,
cwd, env, PTY size, `sandboxPolicy` / `permissionProfile`. Follow-ups:
write, resize, terminate. Streaming is `outputDelta`.

That **shape** is on Runner DTOs today. PTY spawn is wired behind the
same `process_id` (`exec_command.tty`, default false; adapter size
24x80). `process_resize` / `tty_size` stay **P1**. Gateway fills
`cwd: WorkspaceRoot`, runner-local env defaults
(`PATH` / `HOME` / `LANG` applied in the runner process; `TERM=xterm`
for PTY only; not the gateway `PATH` or a host absolute cwd), timeout,
output cap, and a policy summary. Live MCP remains:

```text
exec_command / write_stdin / read_process / terminate_process
```

The model learns this from MCP, not from adapter topology.
`initialize.instructions` holds global invariants. `workspace_info.execution`
(when a workspace is selected) holds effective capabilities. `exec_command`
results carry `dispatch_status`. Do not inject architecture manuals,
Codex crate graphs, or UDS wire details into the client contract.

`output_combined=true` means `read_process` exposes one combined output
stream. stdout/stderr identity is not preserved. Pipe-backed processes
pump stdout and stderr independently, so relative ordering between them
is not guaranteed. PTY output is the terminal master stream.

Do **not** take `codex-exec` (product exec flow) or embed App Server.
`codex-exec-server-protocol` is an internal worker-DTO candidate;
`codex-exec-server` is an experimental backend (`codex-api` /
`codex-config`) — not a forever reject
([codex-reuse.md](codex-reuse.md)). Do **not** default sandbox policy
from “the Codex user’s config.” Gateway maps an already-allowed
request onto runner DTOs. Prefer `codex-process-hardening`,
`codex-utils-pty`, `codex-uds` (transport primitive; RPC stays
CodeSpace), and `codex-file-system` under PathSandbox scope
(`crates/file-system` → `LOCAL_FS`, no-follow I/O and bounded walk).
Then `codex-linux-sandbox` (dev-dep includes `codex-core`;
keep that out of the product graph) plus `codex-network-proxy` when a
network axis exists. A container does not replace that subgraph.

App Server streaming processes are connection-scoped and die when that
connection closes. CodeSpace keeps **MCP request lifetime ≠ process
lifetime**. `process_id` is server-minted and stored as application
state. Ending an MCP request does not kill a live process. The opt-in
UDS path is different: it is 1:1 Gateway ↔ worker. UDS disconnect or
gateway shutdown kills the worker (host children die). `process_id`
does not survive worker death. Runner `Replay` is a same-connection
primitive, not disconnect recovery.

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
not Thread. `crates/store` now uses an in-memory resource serializer
for those keys. SQLite schema is unchanged. MVP takes request-owned
exclusive for `apply_patch` and process-owned exclusive for live shells
(`WORKSPACE_BUSY`); `ProcessExited` (or in-process exit) calls
`release_process`. Confirmed UDS worker death releases **all**
process-owned leases; a lost/ambiguous response by itself does not.
`read` / `find` stay unlocked. Shared-read is typed
only.

## `fs/watch` and search

Do not expose `fs/watch` as a model tool. Use it internally so an
external editor bump invalidates versions and `apply_patch` can fail
`expected_versions` / a future `STALE_READ`.

Fuzzy search **sessions** are TUI typing UX. Keep MCP as `find` /
later `find_files(query, workspace_id, limit)`. Prefer
`codex-file-search` as the engine behind that contract; drop the
session protocol.

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
| PTY / UDS / linux-sandbox / hardening subgraph | Homegrown Landlock/seccomp/PTY/UDS by default |
| Filesystem mechanics under PathSandbox scope | Replacing PathSandbox wholesale; `codex-protocol` in core |
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

P0 code for this substrate is in: Runner exec DTO **shape**
(`RunnerCwd::WorkspaceRoot`, runner-local env defaults),
`PermissionProfile` (`process_exec`) and Environment in `crates/policy`,
resource serializer (request vs process owners), opt-in `UdsRunner` +
`codespace-codex-runtime` (process-hardening + UDS), isolated
`crates/pty` → `codex-utils-pty`, isolated `crates/file-system` →
`LOCAL_FS`. Live MCP tool **names** stay frozen;
`exec_command` has optional `tty` (default false).

**P0** — landed or next subgraph WPs: `codex-apply-patch` (done), exec
runtime **shape** on Runner DTOs (done), PermissionProfile domain in
`crates/policy` (done), Environment domain (operator-registered; not a
tool arg) (done), resource serializer (done), transport
(`UdsRunner`) with process-hardening + UDS (done, opt-in), PTY I/O
backend (done), filesystem mechanics under PathSandbox (done). Still
out: linux-sandbox → network.

**P1** — operation state machine / diff ledger, approval fallback
tools, internal watch, richer process handles (resize, caps),
disconnect policy.

**P2** — `find` quality via `codex-file-search` behind the existing
MCP contract, deterministic hooks, skills as resources or prompts.

**P3** — remote environment, MCP federation, artifact registry.

The next **code** WPs are remaining execution subgraph crates behind
the existing trait, without splitting `apply_patch` into gateway RPCs.
Start at linux-sandbox. Sandbox / network are not a default homegrown OS
stack ([codex-reuse.md](codex-reuse.md)).
