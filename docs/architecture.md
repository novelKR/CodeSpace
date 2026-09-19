<a id="identity"></a>

# Architecture

[English](architecture.md) | [한국어](ko/architecture.md)

CodeSpace separates the agent's decisions from workspace execution. The MCP server owns authorization and operation records. A Runner performs already-authorized filesystem and process work. Codex libraries stay behind adapters.

## Current layout

```text
External Agent Loop
  → MCP server: registry, policy, patch records, work/instruction queue
  → Runner
      ├─ InProcessRunner (default)
      └─ UdsRunner → codespace-codex-runtime → InProcessRunner
           ├─ file operations → codespace-fs
           ├─ patch transaction → codespace-patch
           └─ process supervisor
                ├─ pipes / codespace-pty
                └─ Linux helper when available → sandbox / managed proxy
```

The UDS worker and default runner both execute on the same host. A Linux command sandbox can wrap either runner's spawn path. Neither path dispatches into the Compose container fixture.

<a id="ids"></a>
<a id="repository-layout"></a>

## Responsibilities and state

| Component | Responsibility |
| --- | --- |
| `server` | MCP transport, HTTP authentication/inbox, request validation and orchestration |
| `domain` | CodeSpace tool parameters, results, IDs, error and execution types |
| `policy` | Registered roots, environments, profiles, and network policy |
| `store` | SQLite patch operations, logical works, user instructions; in-memory occupancy |
| `runner` | Execution DTOs, file scope, patch transaction, process supervision, UDS client/server protocol |
| Isolated adapters | Codex patch, PTY, filesystem, worker hardening/socket, Linux sandbox mechanisms |

Patch operations and works/intents survive restart only with a configured SQLite file. Process handles and occupancy leases are memory-only. `operation_status` does not track exec requests. The transport request ID, patch operation ID, process ID, work ID, and instruction ID serve different purposes.

<a id="patch-apply-pipeline"></a>

## Patch transaction

The gateway authorizes the workspace, obtains the write lease, checks the operation key, dispatches one Runner patch request, and records its result. The Runner checks expected versions, performs preflight, snapshots affected files, invokes the patch helper, and verifies resulting disk hashes.

If helper application fails, the Runner attempts per-file snapshot restoration. Post-apply verification errors currently propagate without entering that restoration branch. This is not an atomic filesystem transaction. See [patch behavior](behavior-differences.md) for client-visible consequences.

## Process lifetime

MCP request completion does not end a managed process. Clients continue with its `process_id`. Server restart loses those handles. In UDS mode the gateway owns the worker: internal disconnect/shutdown ends the worker and its children, with no reconnect. See [runner isolation](runner-isolation.md) for the distinct transport and isolation boundaries.

<a id="target-layout"></a>
<a id="out-of-scope-initial"></a>

## Extension boundaries

The core does not import Codex types directly. Adapters may depend on a broader Codex execution graph; this does not make the gateway a Codex agent. Operator configuration selects environments, while MCP clients select only registered workspaces. Container execution, remote runners, approval-resume tools, and a resource scheduler are not implemented.

Keep a patch transaction as one Runner call when adding transports. Keep permission decisions in the gateway rather than importing Codex user/session permissions as authority. [Execution contracts](execution-substrate.md) describe current invariants; [Codex reuse](codex-reuse.md) lists connected adapters.

<a id="protocol-compatibility"></a>
<a id="mvp-tools"></a>

## Tool and protocol references

For callable tools and integration examples, use [Agent Loop integration](agent-integration.md). For revision negotiation and transport tests, use [protocol compatibility](protocol-compatibility.md). This avoids maintaining a second tool reference inside the architecture document.
