# Connect an Agent Loop

[English](agent-integration.md) | [한국어](ko/agent-integration.md)

CodeSpace is the execution layer of your agent. The outer loop selects tools, writes patches, interprets results, and decides when a task is complete. Start with [a configured server and registered workspace](operations.md).

## Connect and inspect capabilities

Use an MCP client SDK to initialize stdio or Streamable HTTP, send the initialized notification, and discover tools with `tools/list`. The repository tests use MCP 2025-11-25 as the baseline. Do not send tool arguments as an ordinary HTTP body to `/mcp`; let the SDK manage MCP framing, session headers, and result decoding.

Examples below are `tools/call` parameter objects, not complete JSON-RPC messages. Tool results may be exposed by your SDK as structured content or text containing JSON. Check both MCP errors and the tool's own result before continuing.

```json
{
  "name": "workspace_info",
  "arguments": {
    "workspace_id": "demo"
  }
}
```

Inspect `execution.files.*.available`, `execution.process.available`, `execution.isolation.command_sandbox`, `execution.network`, and `execution.approvals`. The tool list says what exists; workspace information says what the registered environment permits and supports. Availability does not reserve the workspace. Read-only workspaces cannot run commands. `approvals` is an operator setting, not a client grant.

## Read and patch a file

Use a disposable project for this example. Create `hello.txt` with `hi` followed by a newline before connecting. First read it:

```json
{
  "name": "read",
  "arguments": {
    "workspace_id": "demo",
    "path": "hello.txt"
  }
}
```

Keep the returned `version`. Paths are relative to the registered root. `find` accepts a path glob, not a content search query. `read` returns at most 1 MiB and `find` returns a bounded list; their `truncated` flag means you did not receive the whole result. There is no public range-read or pagination argument.

Replace `VERSION_FROM_READ` below with the exact version returned by `read`. V4A is Codex’s text patch format, using markers such as `*** Begin Patch` and `*** Update File`. This is a complete V4A patch, with JSON newline escapes:

```json
{
  "name": "apply_patch",
  "arguments": {
    "workspace_id": "demo",
    "patch": "*** Begin Patch\n*** Update File: hello.txt\n@@\n-hi\n+hello\n*** End Patch\n",
    "expected_versions": {
      "hello.txt": "VERSION_FROM_READ"
    },
    "operation_key": "hello-preview-1",
    "check_only": true
  }
}
```

A preview returns `status: "checked"` without writing. To apply, send the same patch and expected version with `check_only: false` and a **different** key, such as `hello-apply-1`. The preview and apply requests differ and cannot share an idempotency key. Read the file again after applying. See [patch states and recovery limits](behavior-differences.md).

## Run and observe a command

```json
{
  "name": "exec_command",
  "arguments": {
    "workspace_id": "demo",
    "command": [
      "/bin/echo",
      "agent-smoke"
    ]
  }
}
```

The command is an argument array. Shell quoting, pipes, and `&&` are not interpreted unless you explicitly launch a shell. The working directory is the workspace root; environment and timeout are operator-controlled. Add `"tty": true` to allocate a pseudo-terminal (PTY) when a program requires a terminal. Spawn size is 24×80. `tty_size` is not an `exec_command` argument. Change the size of a running PTY with `process_resize`.

The response contains a server-issued `process_id` and `dispatch_status`. `confirmed` means dispatch was acknowledged, **not that the command succeeded**. Save the ID. Poll output with `read_process` using the returned cursor, and judge exit with `process_status`.

```json
{
  "name": "read_process",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC",
    "cursor": 0
  }
}
```

Each result has `chunk`, `cursor`, `eof`, `output_lost`, and `retained_from`. Output combines stdout/stderr without preserving their identity. EOF means output collection is complete; it is not a successful exit status. Judge termination with `process_status`: `state` is `running` or `exited`. After exit, `termination` is `exited`, `timeout`, `terminated`, or `unknown`. `exit_code` is present only when `termination` is `exited`. `timeout`, `terminated`, and `unknown` are not success, even when `eof` is true. The last 256 KiB are retained. If `output_lost` is true, the retained window is not the complete log. Do not claim a build or test passed solely from EOF or an incomplete log. If success cannot be established from a reliable task-specific result, report it as unverified.

```json
{
  "name": "process_status",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC"
  }
}
```

```json
{
  "name": "process_resize",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC",
    "rows": 40,
    "cols": 120
  }
}
```

These example result bodies illustrate the command above. IDs are placeholders: use the values from your own responses. The examples omit the MCP envelope and show a call without coordination context.

```json
{
  "process_id": "SERVER_ISSUED_PROCESS_ID",
  "dispatch_status": "confirmed"
}
```

```json
{
  "process_id": "SERVER_ISSUED_PROCESS_ID",
  "cursor": 12,
  "chunk": "agent-smoke\n",
  "eof": true,
  "output_lost": false,
  "retained_from": 0
}
```

Interactive input and cancellation use the same handle:

```json
{
  "name": "write_stdin",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC",
    "data": "input\n"
  }
}
```

```json
{
  "name": "terminate_process",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC"
  }
}
```

A live command occupies the workspace. Wait for it to end or terminate it before applying a patch or starting another command. Reads and searches remain available. Long-lived development servers therefore require a workflow that stops them before edits.

`workspace_info.execution.process.capabilities.lifetime` advertises the owner: the runner instance, not the MCP session. Streamable HTTP or MCP client disconnect keeps the process running; reconnect with the saved `process_id`. Losing the UDS worker connection or shutting down the gateway (including stdio EOF) terminates the owned subtree and drops handles. Restart does not restore `process_id`. If the spawn response is lost before you receive `process_id`, do not search with `process_status` and do not start a duplicate command.

## Confirm a held mutation

Default workspaces run allowed patches and commands immediately. If the operator set `approvals` to `confirm`, those tools return `APPROVAL_REQUIRED` and an `approval_id` instead of writing or spawning. This is a workflow pause, not a privilege grant, isolation boundary, or a way to raise `read-only` to write or exec. Extra arguments such as `approved: true` or `network: true` do not grant rights. The same MCP caller can grant the hold.

```json
{
  "name": "approval_resolve",
  "arguments": {
    "approval_id": "APPROVAL_ID_FROM_HOLD",
    "decision": "grant"
  }
}
```

```json
{
  "name": "operation_resume",
  "arguments": {
    "approval_id": "APPROVAL_ID_FROM_HOLD"
  }
}
```

`approval_resolve` does not change the permission profile. `operation_resume` re-checks policy, then runs the original apply or exec path once. `consumed` is stored only with a terminal result. Repeating resume returns that stored result, recovers a recorded patch from the operations ledger, or returns `APPROVAL_AMBIGUOUS`. An interrupted exec resume is not respawned. Deny is terminal. The same three tools exist when approvals are `off`; only an explicit `approval_create` opens a hold in that mode. v1 does not distinguish host from model: the same MCP caller can grant.

## Retry and recover deliberately

| Situation | Agent action |
| --- | --- |
| `APPROVAL_REQUIRED` | Policy allowed the mutation; grant with `approval_resolve` then `operation_resume`. Do not treat this as a grant of extra rights |
| `APPROVAL_CONFLICT` | The hold is still pending, was denied, or a resume is already in progress |
| `APPROVAL_AMBIGUOUS` | Resume was interrupted and the terminal result is not known; do not respawn exec. A patch may still be recoverable with `operation_status` |
| Patch response lost | Query `operation_status` using the original key, or the operation ID if known |
| `VERSION_CONFLICT` | Read current content and produce a new patch; do not force the old one |
| `OPERATION_KEY_CONFLICT` | The key belongs to different arguments; inspect the earlier request |
| Patch `unknown` or `failed_partial` | Inspect affected files and report uncertainty before deciding on a new operation |
| Exec `dispatch_status: unknown` | A process may exist. Inspect or terminate the returned handle if reachable; do not blindly start another |
| Lost spawn response / no `process_id` | Do not invent or search for a handle. Do not start a duplicate; the process may still occupy the workspace |
| Client/HTTP session lost after spawn | Process keeps running. Reconnect and use the saved `process_id` |
| `WORKSPACE_BUSY` | Wait for the owning task or cancel the process; avoid a tight retry loop |
| `TIMEOUT` | Treat execution as interrupted; inspect partial effects |
| Server/worker lost | Reconnect and inspect capabilities/files; old process handles are not recoverable |

```json
{
  "name": "operation_status",
  "arguments": {
    "operation_key": "hello-apply-1"
  }
}
```

Use exactly one lookup identifier. `operation_status` returns the recorded patch ledger: `kind` is always `patch`, plus `workspace_id`, `created_at`, optional `finished_at`, `files`, `changes` (path, kind, and available before/after hashes), and `minted`/`finished` events. An unfinished record has no `finished_at`. A `finished` event with `reason: "unknown"` means the gateway closed the record without a confirmed result. The lookup does not re-run the patch and does not track `exec_command`; commands stay on `process_id`. A request ID identifies a transport message; `operation_id` identifies a recorded patch; `process_id` identifies a managed process. None of these IDs grants authority. A recorded patch refusal after dispatch does not prove that disk contents are unchanged; inspect files when verification failed after application.

## Handle user instructions and finish

Optionally call `work_open` with `workspace_id` and a title. Pass its `work_id` on tools that accept it. At safe checkpoints, call `steer_status`, then `steer_claim_next`; process any claimed instruction and mark it `done` or `blocked` with `steer_complete`. Counts in `coordination` are hints, not the instruction body.

Users create and queue drafts through HTTP `/inbox`; drafts are not delivered until queued. This API requires HTTP mode and has no built-in browser UI. Queued text never changes workspace permissions.

Call `work_finish` with the work ID after draining instructions. If it returns `closed: false` and `reason: "pending_user_input"`, handle the remaining queue instead of declaring completion. This closes coordination state; it does not certify code correctness or replace test evidence.

A useful integration acceptance test is: read → preview → apply → read back → run → collect output → cancel a long command → recover a patch by key. Add a failing command and an oversized log to confirm that your loop handles the current result limitations honestly.
