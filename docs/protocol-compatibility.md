<a id="protocol-compatibility"></a>

# MCP protocol compatibility

[English](protocol-compatibility.md) | [한국어](ko/protocol-compatibility.md)

Use ordinary MCP initialization, tool discovery, and `tools/call`. stdio and Streamable HTTP at `/mcp` expose the same application tools. Work and instruction queues are application state, independent of transport sessions.

<a id="core-baseline"></a>
<a id="what-core-must-not-require"></a>
<a id="_2026-07-28"></a>
<a id="2026-07-28"></a>
<a id="out-of-scope"></a>

## Supported baseline

The repository explicitly tests MCP `2025-11-25` over both transports. It also tests negotiated `2026-07-28` through the pinned SDK without fallback. The newer negotiation does not add different execution behavior. These are repository compatibility targets; a client must negotiate a revision supported by both sides.

HTTP's transport floor is `2025-03-26`. Legacy `2024-11-05` HTTP+SSE is not a supported transport here. Tasks, subscriptions, multi-round-trip requests, and header-based tool routing are not required or implemented as the core execution path. The server dispatches JSON-RPC methods and tool names from the body.

## What the client should inspect

Read `initialize.instructions`, discover tools, then call `workspace_info` with a registered ID. Its `execution` object distinguishes permission from backend support, and reports file/process eligibility, PTY capabilities, serialization, sandbox, and network enforcement. Eligibility does not include transient occupancy.

For actual tool arguments, result interpretation, and retry rules, use [Agent Loop integration](agent-integration.md). Instruction queues use `work_id` and `intent_id`, not MCP Tasks. No protocol flag grants additional workspace permissions.

## Tests

`crates/server/tests/protocol_compat.rs` forces each tested revision on stdio and HTTP, then exercises tool discovery and calls. Tests that prefer a newer revision but allow fallback are separate evidence; they do not replace forced-baseline tests.

`http_contract`, `stdio_contract`, and `transport_contract` cover the transport adapters. Local test success does not establish the protocol revision used by a live ChatGPT account. See [ChatGPT connection status](chatgpt-connector.md).
