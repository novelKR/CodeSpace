<a id="codex-reuse-product-vs-primitive"></a>

# How CodeSpace uses Codex

> **Status: suspended as an implementation directive.** The managed-execution decisions under Reuse decisions for managed execution, including the default of a CodeSpace-owned Unix transport for `required` execution and DevGuard's planned legacy-backend decision, must not be implemented. This is pending the CS-RG integration-boundary revalidation, an owner-directed review of the CodeSpace integration plan; it is not a work unit. No replacement architecture has been approved; the owner decides after reviewing its results. This notice suspends directives only and relaxes no safety requirement. The text below is retained unchanged for historical traceability.

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

The Linux sandbox helper is binary-only. `codespace-linux-sandbox-protocol` contains its CodeSpace-owned handshake data and no Codex types. Worker UDS protocol version 7 and sandbox-helper protocol version 1 are separate contracts.

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

## Reuse decisions for managed execution

> **Status: suspended as an implementation directive.** Do not implement from this section while the CS-RG integration-boundary revalidation is pending. No replacement architecture has been approved, and no safety requirement is relaxed. The text is retained unchanged for historical traceability; the dated annotation on D3 below marks its DevGuard dependency statement as superseded on 2026-09-28, so that statement is no longer current.

DevGuard [design revision 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/design-revision-1.md) and its [ADR-006](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/decisions.md) record how CS-RG treats Codex reuse. These are decisions for planned work; the connected adapters above are unchanged.

- **Finding.** The pinned high-level spawn functions of `codex-utils-pty` reap the child in their own task and keep only descriptors that are already inheritable, so they cannot carry a DevGuard-managed execution unchanged. This is a mismatch in the reap-ownership and descriptor-passing contracts, not a finding that Codex cannot be reused.
- **D1, `required` execution.** The default is a CodeSpace-owned Unix transport at the current pin. Before new code is written, record the reuse options and their contract differences in this order: an existing public API, an upstream candidate with the same contract, limited adaptation, then in-house code.
- **D2, legacy `off` backends.** The current PTY adapter and Tokio pipe path may stay for initial compatibility. CSRG-C09 decides, before final qualification, between integrating them and keeping a limited compatibility backend on recorded grounds.
- **D3, DevGuard.** No Codex dependency is added to DevGuard now. Its core and shared client stay Codex-free; reusing a low-level utility in a DevGuard execution or platform adapter is decided by the code it actually replaces, contract fit, dependency propagation, recovery path and requalification cost, and a feature name alone never adopts a dependency.

  > **Dated annotation (2026-09-30).** Superseded on 2026-09-28 by the owner's dependency policy, now recorded normatively in [DevGuard design revision 2](https://github.com/novelKR/DevGuard/blob/9d223bbd3529d6996fb8ebabeedae5458d31f498/docs/design-revision-2.md): DevGuard may consume Codex and other external implementations behind declared adapters with reviewed, immutable pins. CodeSpace's generic resource client imports no Codex types, and CodeSpace's own pin process is unchanged.

- **`ProcessDriver`.** Codex's `ProcessDriver` is an optional candidate, not a default: at the pin its bridge skips lagged output and `ProcessHandle` terminates on Drop. It is adopted only once its output-loss, backpressure and Drop criteria are proven; otherwise CodeSpace uses its own output and handle abstractions.
- **Limited adaptation.** Permitted only for clearly separated execution mechanisms such as PTY allocation, terminal setup, resize and limited I/O helpers. It is never permitted for `codex-core` product semantics, session authority, the agent loop, broad crate copies or duplication that evades dependency checks. Each adaptation records its provenance (source repository, full SHA and file path), scope, intentional divergence from upstream, tests, and its re-examination and removal conditions; [upstream updates](upstream-update.md) re-examine it on each pin change. Copying crates to hide dependencies from the checks remains forbidden.
- **Pin.** Revision 1 keeps pin `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`. A pin change is a separate decision based on verification, not part of adopting a newer API.
