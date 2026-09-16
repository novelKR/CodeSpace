# Protocol compatibility

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
`tools/call`. Live tools today are `workspace_info`, `read`, and `find`.
Later packages (`apply_patch`, `exec_*`, `operation_status`) add rows to
the same forced-version matrix. They still use `tools/call`.

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

`crates/domain`, `crates/policy`, `crates/patch`, and `crates/runner` do
not import `ProtocolVersion` or `NegotiatedFeatures`. The server adapter
in `crates/server/src/protocol.rs` maps a negotiated revision to
enhancement flags. Handlers keep `tools/call` even when those flags are
true.

## 2026-07-28

When a client negotiates 2026-07-28, the server may advertise enhancement
flags. Semantics of `workspace_info` / `read` / `find` stay identical to
2025-11-25. This revision is **progressive enhancement only**.

Existing Auto tests that prefer 2026-07-28 and fall back to 2025-11-25
prove fallback. They do **not** replace 2025-11-25-only coverage.

## Out of scope

- **2024-11-05 HTTP+SSE.** Not a target. `rmcp` 3.x does not provide that
  transport.
- User-opinion / steering queues (`steer_*`, IntentQueue, Web UI).
- Implementing MRTR, Tasks, or subscriptions.

## Tests

`crates/server/tests/protocol_compat.rs` pins:

1. Forced **2025-11-25** on stdio and HTTP (`ClientLifecycleMode::Initialize`
   + `with_protocol_version(V_2025_11_25)`).
2. Forced **2026-07-28** on HTTP and stdio, with **no** legacy fallback
   (`Auto { preferred_versions: [V_2026_07_28], legacy_version: None }`).

After handshake, `tools/list` includes `workspace_info` and `tools/call`
matches the existing payload contract.
