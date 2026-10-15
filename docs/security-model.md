# Security model

[English](security-model.md) | [한국어](ko/security-model.md)

CodeSpace is designed for a personal, single-user deployment. The operator chooses which directories an external agent may use. File-tool permissions, command isolation, and network enforcement are distinct controls; inspect the effective workspace contract rather than inferring safety from a tool name.

<a id="authentication-vs-selection"></a>
<a id="product-path-policy-always-even-if-the-crate-would-allow-it"></a>

## Trust boundaries

The gateway owns authentication, workspace registration, and permission decisions. `workspace_id`, `work_id`, instruction text, and client claims such as `approved: true` do not grant rights. HTTP optionally checks a static Bearer token; this is not OAuth or per-user authorization.

The Runner executes approved requests. `PathSandbox` checks logical file scope; `codespace-fs` performs no-follow I/O for Runner file operations. A pre-check alone cannot prevent a concurrent process from replacing a path. The patch helper separately applies CodeSpace path policy and uses the Codex no-follow patch options; helper preflight and post-hash code still contain direct filesystem calls. Do not describe all file handling as one race-proof primitive.

The optional UDS worker separates processes on the same host. Its socket lives in a private directory, but process separation is not an OS command sandbox. The Linux helper supplies the command sandbox when its probe succeeds. [Runner isolation](runner-isolation.md) defines those conditions and failure behavior.

An [independent Runner](runner-isolation.md#claim), not a supported mode yet, serves every connection of its own user and lets one at a time control it after a Claim. While no gateway controls it, any process of that user that can reach its socket can claim it, a command run without the Linux sandbox included; see [who can take control](runner-isolation.md#claim-exposure).

<a id="workspace-registry"></a>
<a id="profiles-mvp"></a>
<a id="runner-isolation-linux"></a>

## Permissions and isolation

`read-only` permits reads but denies patches and exec. `workspace-write` permits both; an arbitrary command can change or delete workspace files. Relative-path restrictions on MCP file tools do not by themselves confine a host command.

Without a Linux helper, a restricted-policy workspace can execute unsandboxed host commands, and `network.enforcement` is `none`. This is not a guarantee of network denial. An enabled-network workspace requires the helper and does not fall back to host networking. The enabled proxy currently permits all destination domains; it is not a domain allowlist feature.

The operator-registered root is a trust anchor. Keep tokens, the operations database, gateway configuration, and other secrets outside managed roots. Do not expose a privileged host, Docker socket, or SSH agent through a workspace merely because file tools validate relative paths.

<a id="patch-honesty"></a>
<a id="process-honesty"></a>

## Operation and recovery safety

Version checks prevent applying a patch to an unexpected file version. Operation keys detect duplicate patch requests; they are not authorization tokens. Workspace occupancy serializes mutating patch and exec work. Request-owned conflicts wait in an in-memory FIFO that starts at acquire. A live process returns `WORKSPACE_BUSY` for trailing waiters and new arrivals. Occupancy is not durable. There is no durable process recovery, except that a gateway with an [independent Runner](runner-isolation.md#reconciliation) restores the occupancy of each process that it finds running when it reconciles, and starts no new work in a workspace before that. When its Runner is lost, each execution that had not ended keeps its workspace until an operator confirms that nothing of it runs and releases it, one process at a time; nothing is run again ([Runner loss](operations.md#runner-loss)).

Confirmation holds in `CODESPACE_OPERATIONS_DB` (`approvals`) store the V4A patch or exec argv while the row is `pending`, `granted`, `queued`, or `resuming`. The patch operations ledger stores hashes, not patch text. After `denied` or `consumed`, the hold body is replaced with digest metadata (tool, workspace, fingerprint). A gateway with an [independent Runner](runner-isolation.md#process-records) also records its executions there (`processes`), each with the SHA-256 digest of its argv, never the argv, its environment or a secret, and keeps there the database's random identity (`store_identity`), which it names when it claims the Runner: it tells one database from another and is not a secret. Keep this database outside the workspace root, with tokens and gateway configuration. The hold is a workflow pause: the same MCP caller can grant it. It is not an isolation boundary.

Patch snapshot restoration is best effort and is not an all-failure rollback guarantee. On `unknown`, partial failure, or post-apply verification error, inspect affected files. See [patch behavior](behavior-differences.md) and [integration recovery rules](agent-integration.md).

<a id="logging"></a>

## Evidence and limits

Security test sources cover path escapes, special files, authentication responses, operation replay, and concurrent work. Linux helper tests cover sandbox and proxy behavior. Test coverage is not a kernel-escape audit or proof for every deployment. See the [coverage inventory](../tests/adversarial-report.md).

Structured stderr logging includes redaction helpers. Avoid logging raw tool requests or secrets; there is no separate tamper-resistant audit service. Public HTTPS/ChatGPT deployment and multi-tenant authorization are not established by local MCP tests.
