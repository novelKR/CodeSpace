<a id="devguard-integration"></a>

# DevGuard integration roadmap

> **Status: suspended as an implementation directive.** The CS-RG work order, the planned consumer boundary and the design revision 1 references on this page must not be implemented as described; current status and prerequisites are unaffected. This is pending the CS-RG integration-boundary revalidation, an owner-directed review of the CodeSpace integration plan; it is not a work unit. No replacement architecture has been approved; the owner decides after reviewing its results. This notice suspends directives only and relaxes no safety requirement. The text below is retained unchanged for historical traceability, except the current status and the [status-only connection](#devguard-status-connection) of CSRG-U1, which the owner approved on 2026-10-02 outside this suspension.

[English](devguard-integration.md) | [한국어](ko/devguard-integration.md)

[DevGuard](https://github.com/novelKR/DevGuard) is an independent resource authority for development workloads, licensed under Apache-2.0 like CodeSpace. Its approved integration path adds a shared admission and accounting layer while CodeSpace keeps process ownership, PTY, input/output, permissions, approval holds and workspace coordination.

**Current status:** DevGuard has completed DG-1 in its independent repository ([`395315d`](https://github.com/novelKR/DevGuard/tree/395315d34b5d458ea1774446727f0cb14bd8a120), [milestone ledger](https://github.com/novelKR/DevGuard/blob/395315d34b5d458ea1774446727f0cb14bd8a120/milestones.json)). On top of the DG-0 contracts it provides native macOS host evidence, an authenticated daemon installed as the current user's LaunchAgent, the fenced launch helper and reconciliation, the `devguard` command-line owner with Cargo adapters, parent leases for candidate tests, and upgrade and repair. Its macOS SLO is qualified for release `0.1.0-5daee5d-b3fa569e` (a release ID, not a Git tag) on the measured host and policy; Linux enforcement is not qualified. CodeSpace consumption (CS-RG) began with CSRG-U1, an opt-in, status-only connection: a gateway built with the `devguard` feature and started with `--devguard status` reports DevGuard's status in `workspace_info`, through DevGuard's client pinned at [`f1f9084`](upstream-lock.md#devguard-client-pin). It registers, admits and launches nothing. CodeSpace still has no `resources` workspace setting or Runner wire change, so a running DevGuard service does not govern CodeSpace execution. The qualified release remains a candidate for execution, not a selected pin; the client pin is a source pin of the client crates.

DevGuard [design revision 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/design-revision-1.md), merged in [DevGuard PR #8](https://github.com/novelKR/DevGuard/pull/8) as `d4981b4c241cff42687f5c2c681b583c7847776e`, revised the CS-RG execution layer and work order. It changes plans and design decisions that bind later implementation. It does not change current CodeSpace behavior or any implementation or qualification status.

<a id="resource-integration-prerequisites"></a>

## Prerequisites and priority

The priority path is **DG-0 → DG-1 → CS-RG → P1-RECOVERY**, following the existing P1-SCHED work. Process recovery remains separate from resource-accounting recovery.

| Milestone | Owner | Exit condition |
| --- | --- | --- |
| DG-0 | DevGuard | Independent repository, accepted design, stable attempt identity, reservation/plan/applied-evidence types, durable fenced transitions, registration and compatibility contracts, qualified fake-backend tests |
| DG-1 | DevGuard | Real macOS daemon/client/launcher, generic and Cargo consumption, bounded candidate tests under a functionally tested bootstrap reference, independent repair and a separately qualified stable artifact |
| CS-RG | CodeSpace | Execution-boundary fitness, qualified full-SHA consumption, common execution coordination with pre-spawn process slots and one reaper per execution, one-time PrepareExec/ExecPrepared, approval preservation, bounded control/data paths and replay, InProcess/UDS parity, the legacy backend decision, and upstream regression qualification of the resulting head |
| P1-RECOVERY | CodeSpace | Opt-in Gateway restart/reconnection while an independent Runner retains processes and I/O; reconcile workspace, approvals and resource leases without replaying uncertain exec |
| DG-LINUX | Both | Actual Linux cgroup controllers, ancestor constraints, complete sandbox/proxy scope and control protection; required for overall completion |
| DG-CACHE / DG-ADAPTERS | DevGuard | Safe registered-cache reclamation and additional tool adapters; not prerequisites for P1-RECOVERY |

After these prerequisites, retain the existing relative order of watch completion, file-search engine work, deterministic hooks, skills, remote environments, federation and artifacts. A contract test on a fake Linux scope is not Linux enforcement qualification.

DG-1 qualifies the standalone daemon/CLI, development workloads and bounded self-use. CS-RG then qualifies the integrated Runner, approvals, replay and saturated control paths. DG-1 does not depend on unimplemented CS-RG behavior.

DG-1 is delivered as six sequential PR groups, each reviewed, checked on its current head, normally merged and verified on main before the next. Foreground daemons are used through P4; P5 introduces a current-user LaunchAgent. At C10, first test and freeze a parent containing the new parent-budget capability, then immediately begin bounded real self-use. The earlier functional P4/P5 artifacts are not presumed to support newly added parent operations. C12 separately qualifies and promotes the measured artifact/policy/environment combination.

<a id="devguard-status-connection"></a>

## Status-only connection (CSRG-U1)

CSRG-U1 is the first CS-RG unit. It needs two settings, both off by default:

- **Build.** The gateway's Cargo feature `devguard` (`cargo build -p codespace-server --features devguard`). It links DevGuard's `devguard-client` and `devguard-contract` from the [pinned commit](upstream-lock.md#devguard-client-pin) and needs Rust 1.95. Without it, the binary's dependency graph, its flags and its MCP contract are unchanged.
- **Run.** `--devguard status` (`CODESPACE_DEVGUARD=status`), with an operator-provisioned DevGuard consumer:

| Flag | Environment | Value |
| --- | --- | --- |
| `--devguard-socket` | `CODESPACE_DEVGUARD_SOCKET` | DevGuard's socket, normally `/private/tmp/devguard-<uid>/authority.sock` |
| `--devguard-consumer` | `CODESPACE_DEVGUARD_CONSUMER` | the consumer's id in DevGuard's operator configuration |
| `--devguard-generation` | `CODESPACE_DEVGUARD_GENERATION` | that consumer's generation |
| `--devguard-credential-file` | `CODESPACE_DEVGUARD_CREDENTIAL_FILE` | a private file (0600, owned by this user, one link) holding exactly the consumer's 64-character secret |

**What it reports.** Each `workspace_info` call opens one session through DevGuard's client (connect, `Hello`, `Authenticate`, `Status`) and closes it. DevGuard's 250 ms per-frame deadlines bound a session at about 1.75 s. A call that arrives while a session runs waits and shares the next one, so CodeSpace holds at most one session. The result is `workspace_info.resource_authority`:

- `participation: status` and `governs_execution: false`, always;
- `state`: `available`, `unavailable`, `untrusted_authority`, `incompatible`, `credential_refused` or `credential_unavailable`;
- `error_code`: DevGuard's code for the step that failed. Its message is not passed on;
- `report`, when available: DevGuard's protocol, its capabilities, the role it granted, and its storage, registration and execution readiness with its reason.

A DevGuard failure never fails `workspace_info` or any other tool, and an unknown workspace is refused before any session opens.

**What it does not do.** It registers, admits and launches nothing, and no execution path consults it. There is no `resources` setting and no Runner wire change. In UDS mode the gateway opens the session, not the worker.

**The secret.** CodeSpace reads it from its file for each session and sends it only in `Authenticate`. It is never in a flag, the environment, `workspace_info` or a log; only the state and DevGuard's code are logged.

**Children.** DevGuard's client creates its session socket with `socket()`, which macOS cannot make close-on-exec atomically. CodeSpace's pipe, patch-helper, sandbox-helper and worker spawns mark every descriptor above 2 close-on-exec in the child ([#79](https://github.com/novelKR/CodeSpace/issues/79)), and PTY children close theirs, so a session socket open during a spawn reaches no child. Tests check this against the pipe, PTY and worker spawners.

## CS-RG work order

> **Status: suspended as an implementation directive.** Do not implement from this section while the CS-RG integration-boundary revalidation is pending. No replacement architecture has been approved, and no safety requirement is relaxed. The text is retained unchanged for historical traceability.

Design revision 1 plans CS-RG as 10 work units in six logical PR groups. The IDs are DevGuard planning labels, not commits or GitHub PR numbers, and no unit is implemented.

| Proposed group | Units | Planned content |
| --- | --- | --- |
| CSRG-P0 | C00 | Execution-boundary fitness: an ownership table for every spawn and reap path, a minimal managed PTY through DevGuard's launch helper, a descriptor and spawn-guard fitness report, and measured deadline propagation and pre-reap observation. Test-only; product behavior does not change |
| CSRG-P1 | C01, C02 | A qualified DevGuard client revision with separate client and helper provenance; execution-owner registration and `resources` settings |
| CSRG-P2 | C03, C04 | A common supervisor with pre-spawn slots and one reaper per execution; one-time launch plans and approval handling that never replays uncertain work |
| CSRG-P3 | C05, C06 | Control/data protection; bounded replay, output and lifecycle delivery |
| CSRG-P4 | C07, C09 | Parity and fault tests across modes, then the backend convergence decision |
| CSRG-P5 | C08 | Qualification of the head that C09 leaves |

CSRG-C09 is a mandatory decision before final qualification. It either integrates the legacy `off` backends and removes the replaced code, branches, fixtures and dependencies, or keeps a limited compatibility backend with a recorded reason, scope, duplicated parts, unsupported capabilities, revisit point and removal criteria. A document stating "reviewed" does not complete it. If C09 changes code, the affected C07 parity tests run again, and CSRG-C08 qualifies that resulting head.

This counterpart documentation, listed as CSP-D04 in DevGuard's [PR delivery plan](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/pr-delivery.md), is a delivery condition before CSRG-P0 starts. The planned structure is in [architecture](architecture.md), its execution contracts in [execution contracts](execution-substrate.md), the reuse decisions in [Codex reuse](codex-reuse.md) and the dependency and CI boundaries in [upstream updates](upstream-update.md).

<a id="resource-adoption-levels"></a>

## Minimum adoption conditions

| Level | Minimum evidence | Allowed use |
| --- | --- | --- |
| Contract preparation | DG-0 contracts and exact-source tests | Design adapters and error/state mappings; no operational protection claim |
| Bounded functional trial | DG-1 candidate with real authentication, launch and reclamation in an explicit test environment | Candidate tests; not daily-use qualification |
| macOS development | DG-1 qualification, actual host probes, sufficient budget and a real CLI/adapter entrypoint | The validated development commands and host combination |
| CodeSpace macOS runtime | DG-1 and CS-RG qualification, supported client/artifact/wire combination | Explicit required participation and measured control protection for validated modes |
| Linux enforcement | Additional qualification for actual controllers, delegation and ancestor limits | Kernel controls for the verified resources and scope |
| DevGuard self-use | Functionally tested C10-capable parent artifact, parent lease, isolated test state/credentials/cache and independent repair | Bounded real candidate development begins at C10; daily use requires C12 SLO qualification |

Every participating consumer on the actual execution host shares one normal authority and budget. A configuration file alone does not route commands through the governor. Subtract host headroom and static control reservations before admitting work; reject a workload that cannot fit. Report requested, supported and actually applied policy per resource. A different socket or state directory must not create a second full-host budget.

<a id="resource-consumer-boundary"></a>

## Consumer boundary

> **Status: suspended as an implementation directive.** Do not implement from this section while the CS-RG integration-boundary revalidation is pending. No replacement architecture has been approved, and no safety requirement is relaxed. The text is retained unchanged for historical traceability.

Development consumption uses DevGuard's independent CLI, `devguard exec` from DG-1, to govern builds and tests in both repositories. Product consumption belongs in the Runner on the actual execution host; it does not turn DevGuard into a process or PTY broker. The current UDS worker is on the same host as the gateway, not a remote worker.

The execution-owning **Runner registers once**. InProcess registers with the Gateway PID; UDS registers from the worker PID. One static control reservation covers both Gateway and Runner costs. DG-1 has no separate `service-exec` path: the Gateway passes the consumer credential to a UDS worker through `CredentialHandoff`, InProcess reads it directly, and each bounded session registers that same instance again. Service/subordinate-worker registration is deferred until multiple Runners or shared service reservations require it.

Private credential FDs survive only the required helper stages and close before the user executable; credentials are never exposed through MCP arguments. The pinned Codex PTY accepts selected inherited FDs, but its high-level spawn reaps the child internally and keeps only descriptors that are already inheritable. Extending the current PTY wrapper therefore cannot carry a DevGuard-managed launch; the planned path is described in [execution contracts](execution-substrate.md). Design revision 1 keeps the Codex pin, and changing it is a separate decision based on verification.

The runtime adapter must distinguish an accounted reservation, a proposed execution plan and verified applied policy. Required participation does not imply a tree-wide hard limit on macOS. DevGuard's journal cannot restore CodeSpace process handles or replace the patch operations ledger.

After existing authorization and workspace FIFO acquisition, CS-RG reserves a process slot before spawn and prepares admission before marking an approval resume as dispatching. Resource refusal leaves an unconsumed hold queued. Only a refusal or failure proven not to have started the matching attempt permits approval reuse. A timeout, lost response or missing process handle is not that proof. A tracked helper (`confirmed`), helper `READY` and successful user executable start are distinct events.

Execution slots, resource leases and completed-output retention have separate lifetimes. Resource shortage, authority failure, unsupported required policy and uncertain execution remain distinct outcomes. New work is rejected quickly when the authority is unavailable; existing `process_status` and `terminate_process` use Runner-owned handles without requiring fresh admission. Public MCP tool names remain unchanged.

Control protection covers pending and concurrent requests, total queued/replay/response bytes and retention periods. Separate control/data sockets are insufficient if a shared mutex, writer or callback can still block control. The current full-file read/hash path also needs a separate memory-bound improvement; until then, qualification fixes file sizes and concurrency and does not claim protection for arbitrary file sizes.

The integration will separately qualify the pinned contract/client, daemon/helper artifacts and CodeSpace Runner wire. It must reject missing required capabilities instead of starting a second host-wide authority or silently disabling policy.

Default participation remains `off`; `required` is an explicit operator choice in the future runtime implementation. Strict contract/journal decoding requires actual old/new reader and writer tests: an added field is not automatically backward compatible. Upgrade recovery preserves the current ledger and cannot restore an old snapshot after new admissions.

<a id="resource-gateway-recovery"></a>

## Initial Gateway recovery scope

P1-RECOVERY introduces an operator-selected independent Runner mode and explicit capability. Existing modes keep their current shutdown/disconnect contracts. InProcess shares the Gateway PID and is excluded from live Gateway restart recovery.

The new mode distinguishes normal shutdown, explicit service stop, restart detach and unexpected connection loss. A surviving Runner retains handles, PTY/pipe ownership, bounded output and the original timeout during detach or Gateway loss. Reconnection authenticates the new Gateway, fences stale controller epochs and reconciles workspace occupancy, approvals and resource leases before mutations resume. Reconnecting to an existing execution does not require a new workload budget.

Runner or host loss remains uncertain until actual termination is established. Persisted PID/state does not restore PTY or pipe ownership, and stored argv is never automatically re-executed. Recovery across loss of the Runner and its I/O owner requires a separate future design.

## Design revision 1 references

> **Status: suspended as an implementation directive.** Do not implement from this section while the CS-RG integration-boundary revalidation is pending. No replacement architecture has been approved, and no safety requirement is relaxed. The text is retained unchanged for historical traceability.

These immutable links identify the documents that apply design revision 1. They are design provenance, not a runtime client pin.

| Document at `d4981b4` (English) | Use |
| --- | --- |
| [Design revision 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/design-revision-1.md) | Full specification: execution ownership, F1a–F1e, D1–D3, limited adaptation and the revised work order |
| [Accepted decisions](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/decisions.md) | ADR-006 execution ownership and reuse policy; ADR-001 registration rule with its superseded wording marked |
| [CodeSpace integration specification](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/codespace-integration.md) | Source mapping at CodeSpace `b6e7ed2`, execution ownership and backend decisions |
| [CS-RG work packages](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/milestones/CS-RG.md) | CSRG-C00 to C09, tests, entry/exit gates and rollback |
| [Verification](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/verification.md) / [PR delivery](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/pr-delivery.md) | CS-RG execution verification matrix, maintenance measurements and final-head order |

When this counterpart was written on 2026-09-27, DevGuard main was `30b5fa6f705f053876a8da8d00882772bcf4c41b`, after [DevGuard PR #9](https://github.com/novelKR/DevGuard/pull/9), which changed one test file. That commit is not design provenance, and the qualified release `0.1.0-5daee5d-b3fa569e` is recorded separately from both commits.

The revision re-fixed the confirmation baseline at CodeSpace `b6e7ed22e2c730ac987297455e250cbd6e8e8b0c` and keeps the earlier `e94d214` below as the historical inspection baseline. With CSRG-C00 and CSRG-C09 added, the plan totals **48 proposed implementation commit units in 25 logical PR groups**. DG-1's 12 units in six groups are implemented; the remaining 36 units in 19 groups have not started.

<a id="resource-plan-evidence"></a>

## Plan and evidence references

The original detailed planning revision is **`3abf08f6feffeda63f58b17ac2bbe8fff19ec20b`**, submitted in [DevGuard documentation PR #1](https://github.com/novelKR/DevGuard/pull/1). These immutable links remain valid before that PR is merged. English is the editorial source, with reviewed Korean counterparts and hash checks. The [English design reference](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/design.md) and [Korean planning translation](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/README.md) are maintained separately from the immutable approval artifact. This revision identifies documents, not a selected runtime client dependency. At this revision the plan defined 46 proposed implementation commit units in 23 logical PR groups across seven milestones; design revision 1 changed the totals as stated above.

| Authoritative planning document (English) | Use |
| --- | --- |
| [Planning index and milestone map](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/README.md) | Navigate all seven milestone plans and their commit/PR boundaries |
| [Accepted decisions](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/decisions.md) | Registration/recovery alternatives, rationale and reconsideration conditions |
| [Consumer readiness](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/consumer-readiness.md) | Generic minimum conditions and supported platform claims |
| [CodeSpace integration specification](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/codespace-integration.md) | Current source paths, mode-specific registration, execution, failures and recovery |
| [CS-RG work packages](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/milestones/CS-RG.md) / [P1 recovery work packages](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/milestones/P1-RECOVERY.md) | CodeSpace's proposed commits, tests, entry/exit gates and rollback |
| [Verification](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/verification.md) / [PR delivery](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/pr-delivery.md) | Current versus proposed commands, evidence, SLOs and review handoff |

Qualification retains a 10-minute idle baseline, at least 30 minutes of load and three repetitions, with raw measurements. Local control latency and remote network time are separate. The documented initial targets remain process status p99 ≤500ms and termination acknowledgement p99 ≤1 second, with actual scope termination measured separately. A new documentation commit or passing fake-backend test does not qualify an OS control or product SLO.

The historical inspection baseline is CodeSpace commit `e94d21475643608ad2a466256fb57266b86faa47`; design revision 1 confirms `b6e7ed22e2c730ac987297455e250cbd6e8e8b0c`, as stated above. Codex remains pinned to `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`. The approved DevGuard design has SHA-256 `97b67a1f9518c1781156a4b3b26829b285f84f5c9a44da60f3c5dcf1bc768df8` and is stored as `docs/design.ko.md` in the independent repository.

The initial DG-0 source reference is [DevGuard commit d59cbd4](https://github.com/novelKR/DevGuard/tree/d59cbd43d206a9a9281328a946eddf1dc199f710), with [macOS and Ubuntu contract CI](https://github.com/novelKR/DevGuard/actions/runs/35671367559). This identifies the reviewed foundation; it is not a CodeSpace runtime client pin. The [immutable design](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/docs/design.ko.md) and [milestone ledger](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/milestones.json) are available without the local checkout.

The authorized source is a local checkout of the DevGuard repository. Its `milestones.json`, `docs/contracts.md` and `docs/milestones.md` distinguish implemented work from platform qualification. Run `python3 scripts/validate.py` there with Rust 1.95.0 for an exact-source report, and `scripts/qualify.py` for the DG-1 suites. The suites leave Linux enforcement, browser responsiveness and real self-use under an installed parent as `not_run`; `scripts/measure.py` measures the macOS SLO on the target host, and only CS-RG qualifies CodeSpace integration. The existing roadmap commit `fb822fc24c98f6628dce62d33a5cc67275f8ca34` is included in this documentation delivery; the runtime baseline remains the separate commit above.

Use the English design reference and accepted decisions for ongoing development; preserve the full approved design as historical evidence for configuration and CLI examples. They are not installation instructions for the current CodeSpace release. Keep `target/upstream-reports/local`, operational databases, Git metadata and stable recovery artifacts outside automatic cache reclamation.
