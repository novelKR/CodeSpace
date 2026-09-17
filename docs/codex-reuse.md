# Codex reuse: product vs primitive

CodeSpace does **not** embed the Codex agent. It also does **not**
reimplement every execution mechanism from scratch.

> Prefer upstream Codex **execution** implementations when the required
> dependency subgraph is execution-focused and can be isolated behind
> the Runner. A primitive need not be a single narrow crate; a cohesive
> execution subgraph is acceptable.

This process is execution-only: no model, no Responses API
([execution-substrate.md](execution-substrate.md)). App Server
**protocol** is not an MCP translation target. `command/exec` **shape**
(standalone argv, handles, later TTY) may land on Runner DTOs.
`codex-exec`, `codex-core`, and App Server stay forbidden.

```text
WHO MAY  → CodeSpace Gateway
           MCP contract, workspace, profile meaning,
           operation/idempotency, audit, Runner trait

HOW SAFE → Codex execution subgraph
           patch engine, PTY, spawn/reap, Landlock/seccomp,
           process hardening, network enforcement
```

Reusing a subgraph does **not** make Codex the authorizer. Gateway still
allows; Runner still executes.

```text
ChatGPT / Cursor / other MCP host
   │ MCP
   ▼
CodeSpace Gateway     ← only authorization authority
   ├─ workspace registry / profile meaning
   ├─ operation_key / operation_id / persist
   └─ Runner contract (execution DTOs)
          │
          ▼
   isolated adapter workspace
          │  crates/patch today
          │  crates/codex-runtime later (not created in this WP)
          ▼
   Codex execution subgraph (pinned)
```

## Unit of reuse is a subgraph

Do not require “as narrow as `codex-apply-patch`.” Ask:

1. Is this subgraph **cohesive execution** (PTY, sandbox, hardening)?
2. Do **agent / model / product** types cross the Runner boundary?
3. Can it **bypass Gateway allow**?

A wide Cargo graph is not a reject by itself. `codex-linux-sandbox`
pulls process-hardening, network-proxy, and protocol types because
those are part of a tested Linux sandbox, not because it is an agent.

Reimplementing PTY fd handling, signal races, Landlock, or mount
escapes means Codex bugfixes never arrive except by hand. Prefer:

```text
Codex bugfix → candidate pin → adapter compile/test → promotion
```

That is **not** “track `main`.” Release acceptance stays in
[upstream-update.md](upstream-update.md).

Container isolation and host sandbox are **not substitutes**. A
container plus no-new-privs / seccomp / Landlock / network limits is
defense-in-depth, and later Environments (local container, remote
Linux, bare Linux) may share the same Linux sandbox subgraph.

## Policy vs mechanism

| CodeSpace owns (policy) | Prefer Codex (mechanism) |
| --- | --- |
| workspace / profile allow | PTY |
| path permission meaning | process spawn / reap / signals |
| network permission meaning | seccomp / Landlock / hardening |
| operation approval | network enforcement (when needed) |

`codex-execpolicy` is policy, not mechanism. Do not use it as the
final `allow(command)`.

## The `apply_patch` pattern (isolation, not crate width)

Reuse the engine. Own the service around it. Same pattern for a later
runtime adapter:

- Pin stays [upstream-lock.md](upstream-lock.md) (`6b9826e3aa83b1a5947db50f4332cb9c65f1b340`).
- Path dependency from an **isolated** Cargo workspace, not the repo
  root. Today: `crates/patch`. Later: `crates/codex-runtime` (documented
  only; **not created in this work package**).
- NOTICE + Apache-2.0 attribution.
- Product policy stays in front of and behind the subgraph.
- Do not file-copy a crate out of the Codex workspace.

```text
Gateway → Runner trait → (later) ContainerRunner
       → codespace-runtime helper → Codex execution crates
```

Root workspace must not grow a Codex path dependency.
`scripts/check-no-model-deps.sh` scans
`crates/{domain,policy,runner,store,server}` only.

Do **not** wrap the standalone `apply_patch` binary as a security
boundary. Do **not** wrap Codex App Server as an internal backend.

## Why supervisor code still exists

`process_id`, stdin, terminate, and timeout converge because request
lifetime is not process lifetime. The in-process supervisor stays
CodeSpace until a Runner transport exists. `operation_key` /
`operation_status` recover a lost **remote MCP mutating RPC**, not a
Codex thread.

Pulling `codex-core` / `codex-exec` / App Server to get spawn+PTY also
pulls login, models, plugins, and rollout. That blast radius is still
rejected. A **linux-sandbox + pty + hardening** subgraph is not that.

## Candidates at pin `6b9826e`

Judged from the pin’s `Cargo.toml` files, not from Codex `main`.
**No crate is added in this work package.**

### Reuse now (in code)

**`codex-apply-patch`** via `crates/patch`. Parse, hunk verify, apply,
parity subset.

### Prefer reuse (when that WP)

**`codex-utils-pty`**
([`codex-rs/utils/pty/Cargo.toml`](../third_party/codex/codex-rs/utils/pty/Cargo.toml))

Unix: `portable-pty`, `tokio`, `libc`, `anyhow`. Prefer upstream over a
CodeSpace PTY. Wiring it does **not** add a PTY MCP tool. Gateway still
mints handles.

**`codex-process-hardening`** — prefer with the Linux sandbox subgraph
rather than reimplementing no-new-privs / similar.

**`codex-utils-absolute-path` / `codex-utils-path-uri`** — prefer where
the adapter already needs them (patch does today). MCP still exposes
workspace-relative paths only.

### Active evaluation / likely reuse

**`codex-linux-sandbox`**
([`codex-rs/linux-sandbox/Cargo.toml`](../third_party/codex/codex-rs/linux-sandbox/Cargo.toml))

Landlock, seccomp, process-hardening. Also depends on
`codex-protocol`, `codex-network-proxy`, `codex-sandboxing`,
`codex-install-context`. That width is expected for a cohesive Linux
sandbox. A **container** does not make this crate unnecessary; evaluate
it as defense-in-depth and for non-container Environments.

**Transitive (allowed when the sandbox subgraph is taken):**
`codex-sandboxing`, `codex-network-proxy`. Direct use is allowed if the
adapter needs it. They are not root-workspace deps and not an allow
engine.

### Reference / future backend (not now)

**`codex-exec-server`**
([`codex-rs/exec-server/Cargo.toml`](../third_party/codex/codex-rs/exec-server/Cargo.toml))

HTTP/WS plus config, OTel, protocol, sandboxing, PTY. Too heavy as
today’s Runner backend. Not a forever reject. Later compare:

- A: ContainerRunner + low-level Codex crates
- B: Gateway adapter → exec-server

Measure compile graph and upgrade cost before choosing B.

### Reject

**`codex-exec`** — App Server client, `codex-core`, login, config,
rollout, history, worktree. Product exec flow, not `spawn`.

**`codex-core`** — agent loop, tools, session.

**App Server embed** (`ChatGPT → MCP adapter → Codex App Server`) —
re-imports agent infrastructure and splits authorization.

**Codex session `permissionProfile` / user sandbox config as allow** —
second authorizer.

### Policy stays CodeSpace

**`codex-execpolicy`** — Starlark prefix rules. Small graph, but command
allow/deny is Gateway authority. Reference only.

## What stays CodeSpace

- MCP tool schemas and domain types (no `rmcp` in runner/domain).
- Workspace registry, **meaning** of profiles, path policy.
- Write lock, shell occupancy, `WORKSPACE_BUSY` (until a scheduler WP).
- `operation_key` replay, `operation_id`, `operation_status`.
- Host/in-process process supervisor until a Runner transport exists.
- Container lifecycle and workspace bind-mount **policy**.
- Isolated adapter workspaces (`crates/patch`, later
  `crates/codex-runtime`).

## Next implementation WP

The next **code** work package is still Runner **transport** (Unix
socket / `ContainerRunner`) behind the existing `Runner` trait. That
WP must not split `apply_patch` into multiple gateway-driven RPCs.

Do not default to a homegrown PTY / Landlock / seccomp stack. Take the
execution subgraph through an isolated workspace after the table
above. Pin bump is a deliberate release
([upstream-update.md](upstream-update.md)): SHA + patch parity, and
later runtime-adapter build plus PTY/sandbox/process regressions when
that workspace exists.

Domain expansion (PermissionProfile axes, Environment, scheduler,
approval tools) is sequenced in
[execution-substrate.md](execution-substrate.md). None of that changes
live MCP schemas in this work package.
