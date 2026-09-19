<a id="execution-substrate"></a>

# Execution contracts

[English](execution-substrate.md) | [한국어](ko/execution-substrate.md)

An external Agent Loop owns planning, model context, and completion decisions. CodeSpace supplies deterministic tool operations and observable state. Adding an execution feature must not require an internal model call or a Codex agent session.

<a id="invariant"></a>
<a id="take-leave-concepts"></a>

## Policy and mechanism

The gateway decides whether a registered workspace permits an action. Runner requests contain CodeSpace-owned types; adapters translate them into Codex execution types. Direct Codex crate dependencies and types stay out of core manifests and interfaces, although adapters can bring transitive dependencies into the build graph.

The operator registry maps `read-only` and `workspace-write` to effective permissions and chooses `restricted` or `enabled` networking. Internal policy types can represent more detailed path rules, but those globs are not a public, fully enforced permission feature. Client arguments cannot escalate permissions.

<a id="four-axes-target-domain"></a>

## Environment and identity

| Concept | Current use |
| --- | --- |
| Environment | Operator-selected execution location; host implemented, registered container backend unavailable |
| Workspace | Registered root selected by `workspace_id`; MCP file paths are relative |
| Permission profile | Gateway-owned meaning of allowed file and process actions |
| Operation | Persisted patch ledger with `operation_id`, optional idempotency key, `files`/`changes` hashes, and minted/finished events. Look up with `operation_status`. Does not track exec |
| Process | Server-issued handle for a command; memory-only |
| Work | Logical job and user-instruction queue; separate from a transport session |

`environment_id` is not an MCP tool argument. A network proxy URL or Codex user configuration supplied by the model does not become execution authority.

<a id="pathsandbox-vs-codespace-fs"></a>
<a id="command-exec-shape-vs-crates"></a>
<a id="commandexec-shape-vs-crates"></a>

## Execution and observation

Gateway fills workspace-root cwd, runner-local environment defaults, time/output limits, PTY choice, and policy into the internal Runner request. Only `tty` is exposed as a terminal option today. Public calls do not accept arbitrary cwd/env/timeout overrides. See [operations](operations.md) for defaults and [Agent Loop integration](agent-integration.md) for result handling.

A workspace mutation lease prevents simultaneous patch/exec mutations. Read and find remain available while a command runs, so filesystem I/O must reject symlink races at open time rather than rely on a prior path check. Runner file operations use `codespace-fs`; patch execution uses the separate patch helper. `operation_status` exposes the recorded patch ledger (`kind` is `patch`); live commands stay on `process_id` and are not recovered through that lookup.

UDS transport and Linux sandbox preparation have distinct protocols and failure boundaries. A partially delivered UDS mutation may yield an uncertain result; never retry it as a new mutation merely because the connection failed. The complete process and isolation rules belong in [runner isolation](runner-isolation.md).

<a id="approval-and-mcp-revision"></a>
<a id="scheduler-after-the-single-write-lock"></a>
<a id="fs-watch-and-search"></a>
<a id="fswatch-and-search"></a>
<a id="hooks-and-skills"></a>
<a id="roadmap-implementation-later"></a>

## What remains unimplemented

Process exit codes and explicit output-loss metadata are not exposed to MCP. PTY resize, file range/pagination arguments, durable process recovery, container/remote dispatch, approval-resume tools, and a resource queue scheduler remain absent. Richer internal types and negotiated protocol flags do not imply those features are callable.

## Maintaining the boundary

`check-no-model-deps.sh` scans core dependency declarations and selected source patterns, and checks adapter dependency allowlists. It does not scan the entire upstream source tree or prove the absence of every possible model call. CI also checks the Codex pin, adapter builds/tests, and prohibited sandbox-helper library edges from the Runner. See [upstream updates](upstream-update.md) for release validation.
