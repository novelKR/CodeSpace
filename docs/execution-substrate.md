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
| Confirmation hold | Operator `approvals` setting; row in the `approvals` table (`pending`/`granted`/`resuming` until a terminal result); not a patch operation, privilege grant, or isolation boundary |
| Work | Logical job and user-instruction queue; separate from a transport session |

`environment_id` is not an MCP tool argument. A network proxy URL or Codex user configuration supplied by the model does not become execution authority.

<a id="pathsandbox-vs-codespace-fs"></a>
<a id="command-exec-shape-vs-crates"></a>
<a id="commandexec-shape-vs-crates"></a>

## Execution and observation

Gateway fills workspace-root cwd, runner-local environment defaults, time/output limits, PTY choice, and policy into the internal Runner request. Only `tty` is exposed as a terminal option today. `tty_size` is not a spawn argument. Public calls do not accept arbitrary cwd/env/timeout overrides. See [operations](operations.md) for defaults and [Agent Loop integration](agent-integration.md) for result handling.

`exec_command` returns dispatch identity (`process_id`, `dispatch_status`). `process_status` reports `running` or `exited` plus termination metadata. `read_process` reports output including `output_lost` and `retained_from`. EOF is not success. After handle eviction the next lookup is `PROCESS_NOT_FOUND`, not a new state. On Linux sandbox, the wait status is that of the managed child (the helper argv); it is not documented as identical to the user argv.

A workspace mutation lease prevents simultaneous patch/exec mutations. Read and find remain available while a command runs, so filesystem I/O must reject symlink races at open time rather than rely on a prior path check. Runner file operations use `codespace-fs`; patch execution uses the separate patch helper. `operation_status` exposes the recorded patch ledger (`kind` is `patch`); live commands stay on `process_id` and are not recovered through that lookup.

UDS transport and Linux sandbox preparation have distinct protocols and failure boundaries. A partially delivered UDS mutation may yield an uncertain result; never retry it as a new mutation merely because the connection failed. The complete process and isolation rules belong in [runner isolation](runner-isolation.md).

<a id="approval-and-mcp-revision"></a>
<a id="scheduler-after-the-single-write-lock"></a>
<a id="fs-watch-and-search"></a>
<a id="fswatch-and-search"></a>

## Filesystem observation

The Runner may start a recursive filesystem watcher for a workspace on the first `read`, `find`, `version`, or `apply_patch` call. Events are workspace-relative invalidation hints (`Create`, `Modify`, `Remove`, `Rename`, `ResyncRequired`) with an `epoch` and a delivery `seq`. They are not an MCP tool, not a permission decision, and not a mutation precondition. Watch paths are workspace-relative invalidation names. Emitting a path does not imply that the path is readable, writable, regular, or non-symlink; normal file operations continue to enforce `PathSandbox`.

`expected_versions` and `VERSION_CONFLICT` remain the authoritative apply guard. Missing, coalesced, or restarted watch events must never make `apply_patch` succeed when the on-disk hash no longer matches. Overflow, receive failure (including a lagged subscriber), or an unclassifiable event yields `ResyncRequired` on the same epoch (treat any consumer cache as fully untrusted). Restarting the watcher increments `epoch` and also emits `ResyncRequired` only after a replacement watcher is running. This substrate does not classify self-generated versus external writes, keep a lossless event ledger, or send watch events over UDS.

`find` stays a bounded glob walk. It is not a watch API.

<a id="hooks-and-skills"></a>
<a id="roadmap-implementation-later"></a>

## Confirmation holds

`approval_create`, `approval_resolve`, and `operation_resume` are callable. They hold a mutation the workspace profile already allows until the hold is granted. This is not a security boundary: they do not escalate permissions, apply `{ "network": true }` or `ClientClaims.approved`, or write V4A snapshots into the patch operations ledger. The same MCP caller can grant.

When the operator sets workspace `approvals` to `confirm`, a policy-allowed `apply_patch` or `exec_command` returns `APPROVAL_REQUIRED` with an `approval_id` before `begin()` or spawn. Retrying the same logical request reuses that active hold. `off` (the default) still runs those tools immediately; the three tools remain listed so an explicit `approval_create` can open a hold. Grant does not change the profile. Resume claims `granted` into `resuming`, re-checks `allow()`, then runs the existing apply or exec inner path. `consumed` is recorded only together with the terminal result. A later resume returns that result, recovers a patch from the operations ledger, or returns `APPROVAL_AMBIGUOUS`. Interrupted exec is not respawned. A denied policy stays `UNAUTHORIZED`. Exec remains on `process_id`. v1 does not authenticate host versus model. The server guarantees the policy re-check on resume plus that durability contract.

## What remains unimplemented

PTY resize (`process_resize` is not provided), file range/pagination arguments, durable process recovery, container/remote dispatch, and a resource queue scheduler remain absent. MCP `fs/watch`, UDS watch events, and write-cause classification are not provided. Richer internal types and negotiated protocol flags do not imply those features are callable.

## Maintaining the boundary

`check-no-model-deps.sh` scans core dependency declarations and selected source patterns, and checks adapter dependency allowlists. It does not scan the entire upstream source tree or prove the absence of every possible model call. CI also checks the Codex pin, adapter builds/tests, and prohibited sandbox-helper library edges from the Runner. See [upstream updates](upstream-update.md) for release validation.
