# Protocol compatibility

[English](protocol-compatibility.md) | [한국어](ko/protocol-compatibility.md)

CodeSpace is an execution-tools MCP server. Outer clients choose what to
do. This process never calls a model.

## Core baseline

| Layer | Baseline | Notes |
| --- | --- | --- |
| Protocol | **MCP 2025-11-25** | First-class. CI forces this revision on stdio and Streamable HTTP. |
| Transport | **stdio** and **Streamable HTTP `/mcp`** | Same tool contract on both. |
| HTTP floor | **2025-03-26** | First revision with Streamable HTTP. Not the CI pin. |
| Progressive enhancement | **2026-07-28** | Optional. Same tool semantics. Never required for core. |

Required primitives for core execution: `initialize`, `tools/list`,
`tools/call`. Live tools today are `workspace_info`, `read`, `find`,
`apply_patch`, `operation_status`, `exec_command`, `write_stdin`,
`read_process`, `terminate_process`, `work_open`, `steer_status`,
`steer_claim_next`, `steer_complete`, and `work_finish`. They still use
`tools/call`. User drafts and reorder live on HTTP `/inbox`, not MCP.

## What core must not require

These 2026-07-28 (and related) features may be negotiated later as UX or
acceleration. They are **not** part of the core contract and must not
gate `tools/call`:

- MRTR (multi-round-trip requests)
- Tasks
- subscriptions
- `Mcp-Method` / `Mcp-Name` header routing (SEP-2243)
- 2026-07-28 stateless lifecycle as application state

Auth and routing stay Bearer middleware (optional) plus `/mcp` → rmcp
tool dispatch. Dispatch reads the JSON-RPC method and tool name from the
body. It does not route on `Mcp-Name`.

`crates/domain`, `crates/policy`, `crates/patch`, `crates/store`, and `crates/runner` do
not import `ProtocolVersion` or `NegotiatedFeatures`. The server adapter
in `crates/server/src/protocol.rs` maps a negotiated revision to
enhancement flags. Handlers keep `tools/call` even when those flags are
true. Work/steer is application state (`work_id` / `intent_id`), not MCP
Tasks, MRTR, or subscriptions.

Later **approval** or long-running **Tasks** (see
[execution-substrate.md](execution-substrate.md)) must keep a 2025-11-25
`tools/call` path. Extra permission is a policy refusal today. A future
`approval_*` fallback comes before requiring MRTR. Interactive processes
stay `process_id` handles; Tasks must not replace them.

## 2026-07-28

When a client negotiates 2026-07-28, the server may advertise enhancement
flags. Semantics of the live tools stay identical to 2025-11-25. This
revision is **progressive enhancement only**.

Existing Auto tests that prefer 2026-07-28 and fall back to 2025-11-25
prove fallback. They do **not** replace 2025-11-25-only coverage.

## Out of scope

- **2024-11-05 HTTP+SSE.** Not a target. `rmcp` 3.x does not provide that
  transport.
- Implementing MRTR, Tasks, or subscriptions. Those remain optional
  progressive enhancement and are **not** required for work/steer,
  exec, patch, or a future approval flow.
- Browser Inbox UI (HTTP `/inbox` JSON is in this release).

## Tests

`crates/server/tests/protocol_compat.rs` pins:

1. Forced **2025-11-25** on stdio and HTTP (`ClientLifecycleMode::Initialize`
   + `with_protocol_version(V_2025_11_25)`).
2. Forced **2026-07-28** on HTTP and stdio, with **no** legacy fallback
   (`Auto { preferred_versions: [V_2026_07_28], legacy_version: None }`).

After handshake, `tools/list` includes `workspace_info` and `tools/call`
matches the existing payload contract. Forced 2025-11-25 also covers
`read` / `apply_patch` / `exec_command` / `operation_status` and the
work/steer checkpoint (`work_open` → `/inbox` queue → `steer_claim_next`
→ `work_finish`). Forced 2026-07-28 uses the same `tools/call` surface.

Client-facing execution semantics stay on that surface. `initialize.instructions`
states global invariants (request lifetime ≠ process lifetime, host is
not an OS sandbox, unenforced network is not permission). It does not
assert a network policy value; `workspace_info.execution.network` reports
the effective policy. `workspace_info`
with a `workspace_id` adds an `execution` object (permissions vs backend
support, `files.*.available` and `process.available` as permission and
backend support (not occupancy; `exec_command` or `apply_patch` may still
return `WORKSPACE_BUSY`), PTY, mutation lease, isolation, network).
`files.read.available` and `files.find.available` require read permission
and `file_read_supported`. `files.patch.available` requires write
permission and `file_write_supported`. `process.available` requires both
exec permission and backend support and does not include transient
occupancy; tool existence is `tools_exposed`.
`output_combined=true` means `read_process` exposes one combined stream;
stdout/stderr identity is not preserved. `exec_command` results add
`dispatch_status` (`confirmed` or `unknown`).
Empty argv and confirmed spawn failures still serialize as
`INVALID_PATCH`. `INVALID_COMMAND` and `PROCESS_SPAWN_FAILED` are a
follow-up reclassification; this surface does not add them.
Tool **names** do not
grow. `environment_id`, `cwd`, `tty_size`, and `process_resize` stay off
the client schema.
