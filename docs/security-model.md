# Security model

CodeSpace is not a kernel sandbox. CoS and cokacremote are not either.
Authorization is **gateway policy**. Linux container isolation is the
**target** execution OS; current `exec_command` is a host process with
workspace cwd. The Codex patch crate does not supply the product
boundary. Codex session config and `permissionProfile` are not an
allow path. Gateway and runner are both Rust; splitting languages would
not add a trust boundary. Primitive vs product:
[codex-reuse.md](codex-reuse.md). Execution-only substrate:
[execution-substrate.md](execution-substrate.md).

## Trust boundaries

1. **MCP client** — untrusted for authorization. It may send any tool
   arguments, including `workspace_id`, `approved: true`, and absolute
   paths. Those arguments never grant rights.
2. **Gateway (`crates/server` + `crates/policy`)** — trusted to
   authenticate the transport (optional static Bearer on HTTP
   experiments), select a registered workspace, and refuse work the
   profile does not allow.
3. **Runner (`crates/runner`, in-process today)** — trusted to enforce
   path policy (`PathSandbox`) and to supervise **host** processes for
   an already-authorized action. Untrusted to see gateway secrets. A
   later Unix-socket / container split keeps the same Rust workspace; it
   is a process boundary, not a language one. Compose under `deploy/` is
   an isolation fixture, not this process.
4. **Patch helper (`codespace-patch` + `crates/patch`)** — trusted to
   parse/verify/apply Codex V4A **in-process inside the helper child**.
   The gateway talks to that child over JSON stdin/stdout. Untrusted as a
   sandbox (upstream standalone apply uses sandbox `None` and may follow
   symlinks). This is not the retired `native/patch-worker`.

## Authentication vs selection

- Optional static Bearer is for **HTTP experiments only**. It is not an
  OAuth server. Tokens must never appear in logs or error payloads.
- `workspace_id` is a selector. Knowing the id does not authenticate.
- `work_id` and `intent_id` are selectors. Knowing them does not authenticate.
- User-intent bodies are instructions. They never raise the workspace
  profile or bypass path policy.
- ChatGPT conversation ids are not a trust base.

## Workspace registry

Workspaces are registered in **server configuration**, not by the model.

Each entry maps `workspace_id` → `{ root, profile }`.

Unknown ids are rejected. Paths are resolved **before** being passed to
the engine. Relative paths only. After resolve they must stay inside that
workspace root.

## Profiles (MVP)

| Profile | Meaning |
| --- | --- |
| `read-only` | Default. `read` / `find` / `workspace_info` / `operation_status`. No patch, no shell. |
| `workspace-write` | Explicit. Mutating patch and shell **inside** the workspace. A live shell can delete workspace files; the product says so honestly. |
| `host-admin` | **Excluded from MVP.** |

A busy shell holds the workspace write lock. Other mutating work waits or
fails with `WORKSPACE_BUSY`.

## Product path policy (always, even if the crate would allow it)

- Relative paths only.
- Reject symlink targets and special files (devices, sockets, fifos).
- Reject Add File when the destination already exists.
- Reject Move when the destination already exists.
- Do not follow `..` out of the workspace.
- Do not pass host-absolute paths from the model into the engine.

See [behavior-differences.md](behavior-differences.md).

## Runner isolation (Linux)

**Current:** `exec_command` runs on the host as argv + workspace cwd +
`env_clear`. Path sandboxing applies to `read` / `find` / versions /
rollback, not as a Linux namespace.

**Target / fixture:** unprivileged container user. Mount the workspace
at `/workspace` (or an equivalent dedicated volume). Do **not** mount:

- host home
- SSH agent socket
- `/var/run/docker.sock`
- gateway `.env`, Bearer files, SQLite

[`deploy/compose.yml`](../deploy/compose.yml) demonstrates those
properties with `sleep infinity`. It is not connected to
`exec_command`. There is no runner control socket today.

Gateway unit tests may run on macOS. That is not a claim that Linux
isolation was verified on the development laptop.

## Patch honesty

Statuses: `applied`, `checked`, `rejected`, `failed_rolled_back`,
`failed_partial`, `unknown`.

- Successful `check_only` preview → no file changes (`checked`).
- Preflight / policy failure → no file changes (`rejected`).
- Apply failure that restored snapshots → `failed_rolled_back`.
- Apply failure with leftover drift → `failed_partial` or `unknown`.
- Never report `applied` unless disk hashes match the helper's claimed
  `after_version` (deletes must be absent).
- Never `git reset --hard`.
- Never treat HTTP timeout as rollback or as success.

After `Store::begin` mints an `operation_id`, execution errors include
that id on `ErrorBody`. Transport and pre-`begin` refusals do not.

## Process honesty

`process_id` values are minted by the server. Clients cannot invent
handles. Output is read by cursor and bounded (256 KiB per process).
Time, live process count, and **completed-handle retention** (15 minutes
or 64 completed slots) are limited. Disconnect does not imply the
process died. Process state is volatile: it is not stored in SQLite.

## Logging

Redact Authorization headers, Bearer tokens, and `.env` values. Prefer
structured fields (`workspace_id`, `operation_id`) over dumping raw
requests. There is no separate audit subsystem.
