<a id="behavior-differences"></a>

# Patch behavior and recovery

[English](behavior-differences.md) | [한국어](ko/behavior-differences.md)

CodeSpace accepts Codex V4A patches and applies additional workspace rules before calling the pinned library. A supported library option is not automatically an allowed service operation.

## Patch request contract

Use relative paths and versions returned by `read`. The version `absent` means the target must not exist. Include both source and destination versions when protecting a move. A complete [preview/apply example](agent-integration.md) shows how to change the operation key between those distinct requests.

| Rule | CodeSpace behavior |
| --- | --- |
| Paths | Resolve within the registered root; reject absolute and escaping paths |
| Symlinks and special files | Reject symlink paths and devices, sockets, or FIFOs |
| Add or move destination exists | Refuse rather than overwrite the existing destination |
| Newlines | Request Codex `PreserveLineEndings`; selected parity cases are tested |
| Patch format | V4A only; no automatic unified-diff conversion or `git apply` fallback |
| Preview | `check_only: true` runs preflight without writing; it is not a reservation or proof that later apply will succeed |

## Results and recovery limits

| Status | Meaning and next action |
| --- | --- |
| `checked` | Preview passed; use a new operation key for actual application |
| `applied` | Post-apply disk hashes matched helper claims |
| `rejected` | Request was refused or an error was recorded; inspect the error and whether execution had already begun |
| `failed_rolled_back` | Helper application failed and snapshot restoration reported completion |
| `failed_partial` | Helper application failed and restoration was incomplete; inspect files |
| `unknown` | Final outcome is not known; inspect files and recorded state before retrying |

The Runner snapshots affected files and restores them if the helper apply call fails. An error while verifying a successful helper response currently returns before restoration. The gateway can record that error as `rejected` even though files may have changed. Do not interpret every rejected result as proof of no writes. Crash recovery also does not automatically restore snapshots or replay work. No `git reset --hard` is used.

A successful result includes affected `files` and `changes` with kind and available before/after hashes. Those hashes describe the observed files, not a repository commit or a successful build.

<a id="write-lock"></a>
<a id="transport"></a>

## Write lock and transport

One live command blocks other patch/exec work in that workspace with `WORKSPACE_BUSY`. There is no waiting queue. Reads and searches remain possible. Patch operation keys support replay only for matching request fingerprints; choosing a new key after an uncertain response risks applying the change twice.

stdio and Streamable HTTP expose the same tool schemas. Connection failure does not establish whether a mutation ran. See [error codes](error-codes.md) and [recovery rules](agent-integration.md).
