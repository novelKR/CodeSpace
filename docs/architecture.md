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
| `store` | SQLite patch operations, confirmation holds, logical works, user instructions; in-memory occupancy |
| `runner` | Execution DTOs, file scope, patch transaction, process supervision, UDS client/server protocol |
| Isolated adapters | Codex patch, PTY, filesystem, worker hardening/socket, Linux sandbox mechanisms |

Patch operations, confirmation holds, and works/intents survive restart only with a configured SQLite file. Process handles and occupancy leases are memory-only. `operation_status` does not track exec requests. The transport request ID, patch operation ID, process ID, work ID, instruction ID, and approval ID serve different purposes.

<a id="patch-apply-pipeline"></a>

## Patch transaction

The gateway authorizes the workspace, obtains the write lease, checks the operation key, dispatches one Runner patch request, and records its result. The Runner checks expected versions, performs preflight, snapshots affected files, invokes the patch helper, and verifies resulting disk hashes.

If helper application fails, the Runner attempts per-file snapshot restoration. Post-apply verification errors currently propagate without entering that restoration branch. This is not an atomic filesystem transaction. See [patch behavior](behavior-differences.md) for client-visible consequences.

## Process lifetime

MCP request completion does not end a managed process. Clients continue with its `process_id`. Server restart loses those handles. In UDS mode the gateway owns the worker: internal disconnect/shutdown ends the worker and its children, with no reconnect. See [runner isolation](runner-isolation.md) for the distinct transport and isolation boundaries.

## Planned execution coordination

**Current behavior.** The Runner's process supervisor starts pipe commands with Tokio (`tokio::process`) and `tty: true` commands through the `codespace-pty` adapter over Codex `codex-utils-pty` at the [pinned revision](upstream-lock.md) `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` (`rust-v0.154.0`). The pinned PTY spawn reaps its child internally. On the pipe path, the exit waiter, the timeout task and the kill request each call `try_wait`.

**Target CS-RG structure.** DevGuard [design revision 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/design-revision-1.md) plans one Runner coordination layer that CodeSpace owns for every execution: execution identity, the link between approval and execution, state transitions, timeout, termination requests, output recording and cleanup coordination. Platform differences stay behind a narrow backend boundary: child creation, terminal setup, I/O wiring, exit observation and the actual reap. None of this is implemented, and the names below are design concepts, not current APIs. DevGuard client types and Codex types stay out of public MCP types.

```text
Runner execution coordinator (planned)
  ├─ resource governor: off / DevGuard
  ├─ PreparedExecution → LaunchPlan, consumed once
  ├─ process supervisor: status, timeout, termination, exit observation,
  │                      reap order, output and release coordination
  └─ process backends
       ├─ legacy Codex PTY    (BackendReaped)
       ├─ legacy Tokio pipe   (BackendReaped)
       └─ owned Unix process  (OwnerControlledReap; pipe and PTY transports)
```

| Reap model | Meaning | Planned use |
| --- | --- | --- |
| `BackendReaped` | The backend performs the reap and reports the result | Legacy paths for resource participation `off`, the default |
| `OwnerControlledReap` | CodeSpace observes the exit without reaping, then controls when the same owner reaps | The DevGuard `required` path, which must observe before reaping |

A backend that has already reaped its child must not advertise an `ExitedUnreaped` capability, and the common interface never promises what a backend cannot guarantee. A common supervisor does not by itself make the Runner the owner of a legacy backend's waiter. The contracts are in [execution contracts](execution-substrate.md); status and work order are in the [DevGuard integration roadmap](devguard-integration.md).

<a id="target-layout"></a>
<a id="out-of-scope-initial"></a>

## Extension boundaries

The core does not import Codex types directly. Adapters may depend on a broader Codex execution graph; this does not make the gateway a Codex agent. Operator configuration selects environments, while MCP clients select only registered workspaces. Container execution and remote runners are not implemented. Workspace occupancy waits on an in-memory per-resource FIFO for request-owned work. FIFO order starts at acquire. A live process returns `WORKSPACE_BUSY` for trailing waiters and new arrivals. Queue saturation is `RESOURCE_QUEUE_FULL`.

Confirmation-hold tools (`approval_create`, `approval_resolve`, `operation_resume`) are implemented. They pause a mutation the profile already allows until the hold is granted. This is not a security boundary: they do not raise `read-only` to write/exec, honor `ClientClaims.approved`, or change the permission profile. The same MCP caller can grant. Resume re-checks policy. v1 does not separate host and model callers.

Keep a patch transaction as one Runner call when adding transports. Keep permission decisions in the gateway rather than importing Codex user/session permissions as authority. [Execution contracts](execution-substrate.md) describe current invariants; [Codex reuse](codex-reuse.md) lists connected adapters.

<a id="protocol-compatibility"></a>
<a id="mvp-tools"></a>

## Tool and protocol references

For callable tools and integration examples, use [Agent Loop integration](agent-integration.md). For revision negotiation and transport tests, use [protocol compatibility](protocol-compatibility.md). This avoids maintaining a second tool reference inside the architecture document.
