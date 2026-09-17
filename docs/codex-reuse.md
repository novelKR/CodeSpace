# Codex reuse: product vs primitive

CodeSpace does **not** embed the Codex agent. It also does **not**
reimplement every Codex-adjacent function from scratch. The rule after
W16:

> Keep Codex **product / runtime** out. Pull **independent execution
> primitives** when their crate boundary is as clear as
> `codex-apply-patch`.

This process is execution-only: no model, no Responses API
([execution-substrate.md](execution-substrate.md)). App Server
**protocol** is not an MCP translation target. `command/exec` **shape**
(standalone argv, handles, later TTY) may land on Runner DTOs;
`codex-exec` / App Server crates stay forbidden.

Same features (exec handles, path limits, patch apply, steering-shaped
queues) do not imply the same implementation cut. Codex layers are not
clean libraries. CodeSpace needs its own authorization, workspace
contract, and operation recovery in front of a small Runner.

```text
ChatGPT / Cursor / other MCP host
   │ MCP
   ▼
CodeSpace Gateway     ← only authorization authority
   ├─ workspace registry / profile
   ├─ operation_key / operation_id / persist
   └─ Runner contract (execution DTOs)
          │
          ├─ CodeSpace-specific
          │     container lifecycle, RPC boundary, workspace mapping
          └─ Codex primitives (pinned, isolated workspace)
                today: parse / verify / apply
                later: only if the crate graph stays narrow
```

## Why the small reimplementation existed

Three constraints, not NIH:

1. **Library boundary.** The same *function* in Codex is often not an
   independent library. `codex-apply-patch` is. `codex-exec` is not.
2. **Who authorizes.** Codex session config / `permissionProfile` /
   sandbox policy must not become a second policy engine under the
   gateway. Gateway allows; Runner executes.
3. **Runtime churn.** Pulling App Server / `codex-core` / `codex-exec`
   to get spawn+PTY also pulls login, models, plugins, rollout, and
   daemon. A Codex `main` bump would then move the whole product.

`process_id`, stdin, terminate, and timeout look like Codex
`command/exec` because any command service whose **request lifetime is
not process lifetime** converges on that shape. The in-process
supervisor stays CodeSpace code. `operation_key` /
`operation_status` recover a lost **remote MCP mutating RPC**; they are
not Codex thread/session recovery.

## The `apply_patch` pattern (do this again)

Reuse the engine. Own the service around it.

```text
CodeSpace policy / versions / snapshot / verify / persist
                      │
                      ▼
              parse_patch
              hunk verify
              apply_patch_with_options
                      │
                      ▼
              filesystem result
```

How to take a primitive:

- Pin stays [upstream-lock.md](upstream-lock.md) (`6b9826e3aa83b1a5947db50f4332cb9c65f1b340`).
- Path dependency from an **isolated** Cargo workspace (today
  `crates/patch`), not the repo root workspace.
- NOTICE + Apache-2.0 attribution.
- Product policy stays in front of and behind the crate. The crate is
  not the sandbox and not the authorizer.
- Do not file-copy a single crate out of the Codex workspace.

Do **not** wrap the standalone `apply_patch` binary as a security
boundary (sandbox `None`, symlink follow). Do **not** wrap Codex App
Server as an internal backend.

## Forbidden (product runtime)

Not candidates for CodeSpace dependencies:

| Crate / surface | Why |
| --- | --- |
| `codex-exec` | App Server client, `codex-core`, login, config, rollout, history, worktree |
| `codex-core` | Agent loop, tools, session |
| `codex-app-server` and its protocol/client | Full product RPC, not an execution daemon |
| `codex-exec-server` | HTTP/WS runtime plus config, OTel, network-proxy, sandboxing, PTY |
| Codex config `permissionProfile` / session sandbox as allow | Second authorizer |

Root workspace must not grow a Codex path dependency. New primitives
follow `crates/patch`, not `crates/runner`.

## Candidates at pin `6b9826e`

Judged from the pin’s `Cargo.toml` files, not from Codex `main`.
**No crate is added in this work package.**

### Reject

**`codex-exec`**
([`codex-rs/exec/Cargo.toml`](../third_party/codex/codex-rs/exec/Cargo.toml))

Depends on `codex-app-server-client`, `codex-app-server-protocol`,
`codex-cloud-config`, `codex-config`, `codex-core`, `codex-features`,
`codex-history`, `codex-login`, `codex-model-provider-info`,
`codex-rollout`, `codex-worktree`, and related product crates. This is
the CLI/exec product flow, not `spawn(command)`.

**`codex-exec-server`**
([`codex-rs/exec-server/Cargo.toml`](../third_party/codex/codex-rs/exec-server/Cargo.toml))

Depends on `codex-api`, `codex-config`, `codex-http-client`,
`codex-network-proxy`, `codex-otel`, `codex-protocol`,
`codex-sandboxing`, `codex-utils-pty`, websocket/axum. Execution
daemon plus Codex runtime.

**`codex-sandboxing`**
([`codex-rs/sandboxing/Cargo.toml`](../third_party/codex/codex-rs/sandboxing/Cargo.toml))

Depends on `codex-network-proxy`, `codex-protocol`,
`codex-utils-pty`, `codex-windows-sandbox`, and on Windows
`codex-mxc-sandbox`. Not a Linux-only sandbox library. Codex’s
goal is macOS + Linux + Windows + MXC + PTY + network proxy in one
product. CodeSpace’s target is an isolated Linux workspace behind
an external MCP server.

**App Server embed** (`ChatGPT → MCP adapter → Codex App Server`)

Would re-import the agent infrastructure CodeSpace exists to keep
out. Authorization would split between CodeSpace policy and Codex
session policy.

### Likely (when PTY is in scope)

**`codex-utils-pty`**
([`codex-rs/utils/pty/Cargo.toml`](../third_party/codex/codex-rs/utils/pty/Cargo.toml))

Unix deps: `portable-pty`, `tokio`, `libc`, `anyhow`. That is
apply-patch-shaped: a narrow helper, not a session. Taking it does
**not** by itself add a PTY MCP tool. Gateway still mints handles;
Runner still owns lifetime.

### Hold / partial

**`codex-linux-sandbox`**
([`codex-rs/linux-sandbox/Cargo.toml`](../third_party/codex/codex-rs/linux-sandbox/Cargo.toml))

Landlock / seccomp / process-hardening are OS execution work. The
same manifest also depends on `codex-protocol`,
`codex-network-proxy`, `codex-sandboxing`, `codex-install-context`.
A Linux **container** Runner may already supply isolation, so
“import this crate” is not the default. Revisit only if host-side
landlock is chosen *instead of* (or in addition to) a container,
and only if the protocol/proxy edges can stay out of the root
workspace.

### Keep in CodeSpace (reference only)

**`codex-execpolicy`**
([`codex-rs/execpolicy/Cargo.toml`](../third_party/codex/codex-rs/execpolicy/Cargo.toml))

Starlark prefix rules plus `codex-utils-absolute-path`. The graph
is small, but command allow/deny is Gateway authority. Using
Codex’s rule file as the allow engine would split policy. Fine to
read later; not a drop-in authorizer.

## What stays CodeSpace even after a primitive lands

- MCP tool schemas and domain types (no `rmcp` in runner/domain).
- Workspace registry, profiles, path policy.
- Write lock, shell occupancy, `WORKSPACE_BUSY`.
- `operation_key` replay, `operation_id`, `operation_status`.
- Host/in-process process supervisor until a Runner transport exists.
- Container lifecycle and workspace bind-mount policy.

## Next implementation WP

The next **code** work package is still Runner **transport** (Unix
socket / `ContainerRunner`) behind the existing `Runner` trait. That
WP must not split `apply_patch` into gateway-driven RPCs.

Sandbox, PTY, and network isolation are **not** “write our own
landlock/seccomp/PTY by default.” Attach a candidate from the table
only after this graph test, using the `crates/patch` isolation
pattern. Pin bump is a separate, deliberate release
([upstream-update.md](upstream-update.md)).

Domain expansion (PermissionProfile axes, Environment, scheduler,
approval tools) is sequenced in
[execution-substrate.md](execution-substrate.md). None of that changes
live MCP schemas in this work package.
