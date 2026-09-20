<a id="codex-reuse-product-vs-primitive"></a>

# How CodeSpace uses Codex

[English](codex-reuse.md) | [한국어](ko/codex-reuse.md)

CodeSpace uses selected libraries from a pinned Codex checkout to implement execution. The external agent still plans and generates code. CodeSpace owns MCP, workspace permissions, operation identity, and process management.

<a id="unit-of-reuse-is-a-subgraph"></a>
<a id="reuse-now-in-code"></a>

## Connected components

| Adapter | Codex components | Current role |
| --- | --- | --- |
| `crates/patch` | `codex-apply-patch`, `codex-exec-server::LOCAL_FS`, path utilities, process hardening | V4A parsing/application in `codespace-patch` |
| `crates/codex-runtime` | `codex-process-hardening`, `codex-uds` | Optional hardened worker and Unix socket |
| `crates/pty` | `codex-utils-pty` | Terminal-backed execution for `tty: true` |
| `crates/file-system` | `codex-file-system`, `LOCAL_FS`, path utilities | Runner file I/O and bounded walks with no-follow handling |
| `crates/linux-sandbox` | `codex-linux-sandbox`, `codex-sandboxing`, `codex-protocol`, `codex-network-proxy` | Binary-only command sandbox helper and enabled-network proxy |

A dependency's presence is not evidence that its entire service is running. For example, file and patch adapters use `LOCAL_FS` from `codex-exec-server`; CodeSpace does not use that server as its general command backend. The Linux helper owns its proxy and sandbox translation. Public types remain CodeSpace types.

<a id="core-vs-adapter"></a>
<a id="the-apply-patch-pattern-isolation-not-crate-width"></a>
<a id="the-apply_patch-pattern-isolation-not-crate-width"></a>
<a id="isolated-layer-transitive-codex-protocol"></a>

## Boundaries

```text
Agent → CodeSpace MCP/policy/store → Runner contract
                                      → adapter → Codex execution library → OS
```

Core crates have no direct Codex dependencies. Adapter crates are separate Cargo workspaces to accommodate the pinned upstream workspace dependencies. The filesystem and PTY adapters are library dependencies of the Runner; patch and Linux sandbox operations use helper processes. An isolated Cargo workspace alone does not create a process or security boundary.

The Linux sandbox helper is binary-only. `codespace-linux-sandbox-protocol` contains its CodeSpace-owned handshake data and no Codex types. Worker UDS protocol version 4 and sandbox-helper protocol version 1 are separate contracts.

<a id="policy-vs-mechanism"></a>
<a id="why-supervisor-code-still-exists"></a>
<a id="code-allowed-authority-forbidden"></a>
<a id="reject"></a>
<a id="what-stays-codespace"></a>

## Authority stays in CodeSpace

Gateway policy decides which workspace actions are allowed. Codex execution code implements mechanisms such as no-follow file access, PTY creation, and sandbox setup. Importing Codex session permissions, login, model selection, or an agent loop would change that responsibility split and is not part of this product.

Current entrypoints do not embed `codex-core`, `codex-exec`, or Codex App Server as a product runtime. Broad transitive crate graphs are evaluated separately from runtime call sites. See [security boundaries](security-model.md).

<a id="staged-take-when-those-wps-exist"></a>
<a id="candidates-at-pin-6b9826e"></a>
<a id="prefer-reuse-when-that-wp"></a>
<a id="conditional-active-evaluation"></a>
<a id="internal-protocol-candidate"></a>
<a id="experimental-backend-not-now"></a>
<a id="future-environment-not-p0"></a>
<a id="next-implementation-wp"></a>

## Updates and future work

All reused components currently come from the [same pinned revision](upstream-lock.md). Upstream fixes arrive only after an explicit pin update and validation; they are not automatically inherited. [Update checks](upstream-update.md) include each connected adapter, not only patch tests.

`codex-file-search`, shell-command parsing, worktree provisioning, and a general `codex-exec-server` backend remain candidates, not connected features. Evaluate a candidate by the execution function it supplies, its build/upgrade cost, and whether model or permission authority would cross the adapter boundary. Current user-facing limitations are listed in [Agent Loop integration](agent-integration.md).
