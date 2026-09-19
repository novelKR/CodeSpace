# Codex reuse: product vs primitive

[English](codex-reuse.md) | [한국어](ko/codex-reuse.md)

CodeSpace does **not** embed the Codex agent. It also does **not**
reimplement every execution mechanism from scratch.

> CodeSpace does not avoid Codex execution code. It prevents Codex
> agent/product ownership from entering CodeSpace core. Meaning and
> authorization stay here; low-level execution comes from a pinned
> Codex subgraph.

The point is not “write less code.” A pin bump should inherit PTY,
sandbox, hardening, and path bugfixes. CodeSpace then spends its
energy on MCP, authorization, operation identity, and the Runner
contract.

This process is execution-only: no model, no Responses API
([execution-substrate.md](execution-substrate.md)). App Server
**protocol** is not an MCP translation target. `command/exec` **shape**
(standalone argv, handles, later TTY) may land on Runner DTOs.
`codex-exec`, `codex-core`, and App Server stay forbidden.

```text
WHO MAY  → CodeSpace Gateway
           MCP contract, workspace, profile meaning,
           operation/idempotency, audit, Runner trait

HOW SAFE → pinned Codex execution subgraph
           patch engine, PTY, spawn/reap, Landlock/seccomp,
           process hardening, filesystem mechanics,
           network enforcement
```

Reusing a subgraph does **not** make Codex the authorizer. Gateway still
allows; the adapter still executes.

```text
ChatGPT / Cursor / other MCP host
   │ MCP
   ▼
CodeSpace Core          ← only authorization authority
   ├─ MCP / workspace / profile meaning
   ├─ operation_key / operation_id / persist
   └─ Runner contract (CodeSpace DTOs; no Codex types)
          │
          ▼
   isolated adapter workspace
          │  crates/patch (codespace-patch)
          │  crates/codex-runtime (codespace-codex-runtime)
          │    process-hardening + UDS worker; opt-in
          │  crates/pty (codespace-pty)
          │    interactive spawn; no Codex types on the runner API
          │  crates/file-system (codespace-fs)
          │    no-follow I/O + bounded walk; PathSandbox authorizes, adapter I/O is the safety boundary
          │  crates/linux-sandbox-protocol (serde handshake; SANDBOX_HELPER_PROTOCOL=1)
          │  crates/linux-sandbox (codespace-linux-sandbox binary)
          │    process boundary: prepare / opaque plan / run exec; Restricted net hard deny
          ▼
   Codex execution subgraph (pinned) → OS
```

Codex is an **implementation** dependency behind the adapter, not an
architectural dependency of the Gateway. `InProcessRunner`,
`UdsRunner`, a later remote runner, or another sandbox backend
can change if Codex types never leave the adapter.

## Unit of reuse is a subgraph

Do not require “as narrow as `codex-apply-patch`.” Ask:

1. Is this subgraph **cohesive execution**?
2. Do **agent / model / product** types cross the Runner boundary?
3. Can it **bypass Gateway allow**?

A wide Cargo graph is not a reject by itself. Reimplementing PTY fd
handling, signal races, Landlock, mount escapes, or process hardening
means Codex bugfixes never arrive except by hand. Prefer:

```text
Codex bugfix → candidate pin → adapter compile / security / parity → promotion
```

That is **not** “track `main`.” Release acceptance stays in
[upstream-update.md](upstream-update.md). On a pin bump, diffs in
hardening / PTY / sandbox / filesystem / network crates are an
execution/security changelog, not a silent dependency bump.

Container isolation and host sandbox are **not substitutes**. A
container plus no-new-privs / seccomp / Landlock / network limits is
defense-in-depth. Later Environments (local container, remote Linux,
bare Linux) may share the same Linux sandbox subgraph.

## Core vs adapter

```text
CodeSpace core
  crates/domain, policy, store, server, runner
  ──────────────────────────────────────────
  NO Codex types (including codex-protocol)
  NO Codex crate path dependency


isolated adapter (crates/patch today;
crates/codex-runtime today; crates/pty today;
crates/file-system today; crates/linux-sandbox today,
binary-only)
  ──────────────────────────────────────────
  approved execution subgraph allowed
  including transitive codex-protocol

crates/linux-sandbox-protocol (root workspace)
  ──────────────────────────────────────────
  serde / serde_json only; no Codex types
  SANDBOX_HELPER_PROTOCOL = 1 (not UDS / WIRE_PROTOCOL)


adapter boundary
  ──────────────────────────────────────────
  CodeSpace DTO  ↔  Codex DTO
```

Public MCP stays `workspace_id` + relative path. Internally an adapter
may resolve:

```text
MCP virtual path
      ↓
CodeSpace WorkspaceResolver / path scope
      ↓
Codex AbsolutePath / PathUri  (adapter only)
      ↓
Runner helper
```

## Policy vs mechanism

| CodeSpace owns (policy) | Prefer Codex (mechanism) |
| --- | --- |
| workspace / profile allow | PTY, UDS transport primitive |
| path permission meaning | filesystem walk / symlink mechanics |
| network permission meaning | seccomp / Landlock / hardening |
| operation approval | network enforcement (when needed) |
| Runner RPC contract | shell parse / argv construction |

`codex-execpolicy` may parse or classify in the adapter. It is **not**
the final `allow(command)`.

## The `apply_patch` pattern (isolation, not crate width)

Reuse the engine. Own the service around it. Same pattern for a later
runtime adapter:

- Pin stays [upstream-lock.md](upstream-lock.md) (`6b9826e3aa83b1a5947db50f4332cb9c65f1b340`).
- Path dependency from an **isolated** Cargo workspace, not the repo
  root. Today: `crates/patch`, `crates/codex-runtime`
  (`codespace-codex-runtime`), `crates/pty` (`codespace-pty`),
  `crates/file-system` (`codespace-fs`), and `crates/linux-sandbox`
  (`codespace-linux-sandbox` binary). The runner talks to that helper
  through `crates/linux-sandbox-protocol` (serde only; root workspace
  member).
- NOTICE + Apache-2.0 attribution.
- Product policy stays in front of and behind the subgraph.
- Do not file-copy a crate out of the Codex workspace.

```text
Gateway → Runner trait → UdsRunner (opt-in)
       → codespace-codex-runtime helper → Codex execution crates
```

Root workspace must not grow a Codex path dependency.

Policy scan (`scripts/check-no-model-deps.sh`) is a cheap parallel CI
job **without** the Codex submodule. Cost is dominated by submodule
checkout and cargo, not this grep.

| Zone | Paths | Cost | When |
| --- | --- | --- | --- |
| core manifests | root + `crates/{domain,policy,runner,store,server}/Cargo.toml` | tiny | crate in the update range |
| core sources | those crates’ trees | low | same |
| server tests | `tests/` | low | `crates/server` or `tests/` changed |
| adapter manifests | `crates/patch/Cargo.toml`; `crates/codex-runtime`; `crates/pty`; `crates/file-system`; `crates/linux-sandbox` | tiny | adapter in the update range; allowlist only |
| protocol crate | `crates/linux-sandbox-protocol` | tiny | no `codex-` keys; runner must not path-dep the helper library |
| upstream | `third_party/codex` | huge / false positives | never |

Update range is `SCAN_BASE` (PR base / previous `main`). Unknown range
scans **all** core crates and adapter manifests (never skip because the
diff failed). Docs-only changes skip this job with exit 0; the rust
job still runs.

Core manifests forbid any `codex-` dependency key. Adapter manifests
allow only the approved subgraph (`crates/patch` today:
`codex-apply-patch`, `codex-exec-server` as apply-patch workspace
graph, `codex-utils-path-uri`, `codex-process-hardening`;
`crates/codex-runtime`: `codex-process-hardening`, `codex-uds`;
`crates/pty`: `codex-utils-pty`; `crates/file-system`:
`codex-file-system`, `codex-exec-server`, `codex-utils-path-uri`;
`crates/linux-sandbox`: `codex-linux-sandbox`, `codex-sandboxing`,
`codex-protocol`, `codex-utils-path-uri`). Sources keep the agent/model patterns
(`api.openai.com`, Responses, `codex-login`, `codex-core`,
`codex-app-server`, `async-openai`). Comments that mention a crate
name are not cargo deps.

The linux-sandbox **helper lock** pins the Rama **0.3.0-alpha.4** leaf
crates (`rama-error`, `rama-macros`, `rama-utils`) as resolver guards.
Codex pin `6b9826e` is validated against that train. A fresh resolve can
otherwise pick stable `0.3.0` for those leaves while `rama-core` stays
alpha.4. The runner does **not** path-depend the helper crate, so those
guards do not enter the root lock via sandbox. Root may still see Rama
through `codespace-fs`. Helper CI `cargo clippy` / `cargo test` use
`--locked`. The rust job also checks `cargo tree -p codespace-runner`
for helper-package edges (`codespace-linux-sandbox`,
`codex-linux-sandbox`). `codex-sandboxing` / `landlock` / `seccompiler`
may still appear via `codespace-fs` → `codex-protocol`; that is not the
sandbox helper graph.

Do **not** wrap the standalone `apply_patch` binary as a security
boundary. Do **not** wrap Codex App Server as an internal backend.

## Why supervisor code still exists

`process_id`, stdin, terminate, and timeout converge because request
lifetime is not process lifetime. The in-process supervisor remains the
**default**. Opt-in `UdsRunner` still runs that supervisor inside
`codespace-codex-runtime`. `operation_key` / `operation_status` recover
a lost **remote MCP mutating RPC**, not a Codex thread.

Pulling `codex-core` / `codex-exec` / App Server to get spawn+PTY also
pulls login, models, plugins, and rollout. That blast radius is still
rejected.

## Staged take (when those WPs exist)

Documented order. **Taken in code this WP:** process-hardening, UDS, PTY
(`crates/pty` → `codex-utils-pty`), filesystem (`crates/file-system`
→ `LOCAL_FS` / `ExecutorFileSystem`), and linux-sandbox
(`crates/linux-sandbox` binary → prepare / opaque plan / `run --plan`
exec, Restricted hard deny). **Not
taken:** network (`Enabled` + proxy).

```text
process-hardening → PTY → UDS / path → filesystem → linux-sandbox → network
```

Complexity and lock-in grow in that order. Pin still defines every
taken subsystem at once.

## Candidates at pin `6b9826e`

Judged from the pin’s `Cargo.toml` files, not from Codex `main`.

### Reuse now (in code)

**`codex-apply-patch`** via `crates/patch`. Parse, hunk verify, apply,
parity subset.

**`codex-process-hardening`** via `codespace-patch` and
`codespace-codex-runtime` `pre_main_hardening()`. This hardens the
worker/helper **process**, not command sandbox. Keep it as the first
line of `main`; do not add `ctor` unless the dependency fan-out is
justified.

**`codex-uds`** via `codespace-codex-runtime` bind. RPC stays CodeSpace.

**`codex-utils-pty`** via `crates/pty` (`codespace-pty`).
Unix: `portable-pty`, `tokio`, `libc`. Default size 24x80. Wiring it
does **not** add a PTY MCP tool. `exec_command` has optional `tty`
(default false). Gateway still mints `process_id`. Resize stays off the
Runner/MCP surface (P1).

**`codex-file-system`** via `crates/file-system` (`codespace-fs`).
Bounded walk, no-follow I/O through `LOCAL_FS` (`sandbox: None`).
Public types stay CodeSpace (`Path` / bytes / walk result / `FsError`).
`PathSandbox` remains the **authorizer** (logical workspace selection).
It is not the I/O safety boundary; live processes may race a pre-check.
`codespace-fs` owns race-resistant no-follow open/read/write/remove/walk
and typed errors (`SymlinkRejected`, `NotRegularFile`). MCP `read` /
`find` stay workspace-relative.

**`codex-linux-sandbox`** via `crates/linux-sandbox`
(`codespace-linux-sandbox` binary).
([`codex-rs/linux-sandbox/Cargo.toml`](../third_party/codex/codex-rs/linux-sandbox/Cargo.toml))

Process boundary, not a library adapter. The runner sends
`SandboxPrepareRequest` JSON (`SANDBOX_HELPER_PROTOCOL = 1`) to
`prepare`, gets a plan **pathname** only, then spawns managed
`run --plan`. The helper unlinks the 0600 plan and `exec`s itself with
Codex argv (same PID). `WIRE_PROTOCOL` stays `3`. Restricted network is
`--unshare-net` plus Restricted seccomp. Direct proxy flags
(`--allow-network-for-proxy`, `--proxy-route-spec`) are unused; that is
the next WP. Runtime deps do not include `codex-core`; **dev-dependencies
do** — adapter tests must not pull that graph into the product binary.
`codex_protocol::PermissionProfile` stays inside the helper.
`codespace-runner` depends on `codespace-linux-sandbox-protocol` only.

**Transitive (allowed in the adapter):** `codex-sandboxing`,
`codex-network-proxy`, `codex-protocol`. Direct use of the proxy is for
when a PermissionProfile **network** axis exists. Not an allow engine.
Not a root-workspace dep.

### Prefer reuse (when that WP)

**`codex-uds`** (already in `codespace-codex-runtime`)
([`codex-rs/uds/Cargo.toml`](../third_party/codex/codex-rs/uds/Cargo.toml))

Unix: Tokio `fs` / `net` / `rt`. Socket primitive for the opt-in Runner
Unix-socket worker. **RPC protocol stays CodeSpace-owned**.

**`codex-utils-absolute-path` / `codex-utils-path-uri`**

Small path/URI layer (`dirs`, `dunce`, URL). Patch already needs them.
MCP still exposes workspace-relative paths only.

**`codex-file-search`**
([`codex-rs/file-search/Cargo.toml`](../third_party/codex/codex-rs/file-search/Cargo.toml))

`ignore`, `nucleo`, Tokio. No `codex-core`. MCP stays
`find(query, workspace_id)`; the engine can move behind the adapter.

### Conditional / active evaluation

**`codex-shell-command`**

Tree-sitter Bash/PowerShell, shlex, `which`. Parse / quoting /
executable resolution only. Not the allow engine.

### Internal protocol candidate

**`codex-exec-server-protocol`**
([`codex-rs/exec-server-protocol/Cargo.toml`](../third_party/codex/codex-rs/exec-server-protocol/Cargo.toml))

file-system, network-proxy, protocol, shell-command, path-uri. Later
worker DTO / adapter substrate. **Not** an MCP or `crates/domain` type.

### Isolated-layer transitive: `codex-protocol`

Heavy: execpolicy, http-client, network-proxy, extension items,
Landlock/seccompiler on Linux. **Forbidden in core.** Allowed in the
adapter because forbidding it forces a rewrite of file-system,
sandbox, and shell-command. `codex_protocol::PermissionProfile` must
not appear on MCP or in `crates/domain`.

### Experimental backend (not now)

**`codex-exec-server`**
([`codex-rs/exec-server/Cargo.toml`](../third_party/codex/codex-rs/exec-server/Cargo.toml))

HTTP/WS plus `codex-api`, `codex-config`, OTel, protocol, sandboxing,
PTY. Too heavy as today’s Runner backend. Not a forever reject. Later
compare UdsRunner + low-level crates vs Gateway adapter →
exec-server. Measure compile graph and upgrade cost.

### Future Environment (not P0)

**`codex-git-utils` / `codex-worktree`** — isolated checkout /
worktree lifecycle if Environment provisioning needs it. Pull
file-system, protocol, PTY, `gix`. Leave until that WP.

### Code allowed, authority forbidden

**`codex-execpolicy`** — Starlark prefix rules. Classification/parsing
in the adapter is fine. Final allow stays Gateway.

### Reject

**`codex-exec`** — App Server client, `codex-core`, login, config,
rollout, history. Product exec flow, not `spawn`.

**`codex-core`** — agent loop, tools, session.

**App Server embed** (`ChatGPT → MCP adapter → Codex App Server`) —
re-imports agent infrastructure and splits authorization.

**login / model / Responses** — execution-only violation.

**Codex session `permissionProfile` / user sandbox config as allow** —
second authorizer.

## What stays CodeSpace

- MCP tool schemas and domain types (no `rmcp` in runner/domain; no
  Codex types in core).
- Workspace registry, **meaning** of profiles, path policy.
- Write lock, shell occupancy, `WORKSPACE_BUSY` (until a scheduler WP).
- `operation_key` replay, `operation_id`, `operation_status`.
- Host/in-process process supervisor as the default; UDS worker is opt-in.
- Container lifecycle and workspace bind-mount **policy**.
- Isolated adapter workspaces (`crates/patch`,
  `crates/codex-runtime` / `codespace-codex-runtime`,   `crates/file-system` / `codespace-fs`,
  `crates/linux-sandbox` / `codespace-linux-sandbox` binary).
  Handshake types live in `crates/linux-sandbox-protocol`.

## Next implementation WP

The next **code** work package is the remaining execution subgraph
(network: `Enabled` + proxy) behind the existing `Runner`
trait. Do not split `apply_patch` into multiple gateway-driven RPCs.

Do not default to a homegrown PTY / Landlock / seccomp stack. Take the
execution subgraph through an isolated workspace after the table above.
Pin bump is a deliberate release
([upstream-update.md](upstream-update.md)): SHA + patch parity now;
runtime-adapter build plus PTY / sandbox / process regressions when
those crates are taken.

Domain expansion remaining (scheduler queue, approval tools) is
sequenced in [execution-substrate.md](execution-substrate.md). Live MCP
tool names stay unchanged; `exec_command` gained optional `tty`.
