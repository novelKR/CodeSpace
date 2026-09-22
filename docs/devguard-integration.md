<a id="devguard-integration"></a>

# DevGuard integration roadmap

[English](devguard-integration.md) | [한국어](ko/devguard-integration.md)

[DevGuard](https://github.com/novelKR/DevGuard) is an independent resource authority for development workloads, licensed under Apache-2.0 like CodeSpace. Its approved integration path adds a shared admission and accounting layer while CodeSpace keeps process ownership, PTY, input/output, permissions, approval holds and workspace coordination.

**Current status:** the independent DG-0 repository provides resource contracts, a durable authority core and fake-backend contract tests. CodeSpace runtime integration is planned. No DevGuard client pin, running daemon, `resources` workspace setting or Runner wire change is enabled by this documentation update.

<a id="resource-integration-prerequisites"></a>

## Prerequisites and priority

The priority path is **DG-0 → DG-1 → CS-RG → P1-RECOVERY**, following the existing P1-SCHED work. Process recovery remains separate from resource-accounting recovery.

| Milestone | Owner | Exit condition |
| --- | --- | --- |
| DG-0 | DevGuard | Independent repository, accepted design, stable attempt identity, reservation/plan/applied-evidence types, durable fenced transitions, registration and compatibility contracts, qualified fake-backend tests |
| DG-1 | DevGuard | Real macOS daemon/client/launcher, generic and Cargo consumption, bounded candidate tests under a functionally tested bootstrap reference, independent repair and a separately qualified stable artifact |
| CS-RG | CodeSpace | Qualified full-SHA consumption, pre-spawn process slots, PrepareExec/ExecPrepared, approval preservation, bounded control/data paths and replay, InProcess/UDS parity, upstream regression qualification |
| P1-RECOVERY | CodeSpace | Opt-in Gateway restart/reconnection while an independent Runner retains processes and I/O; reconcile workspace, approvals and resource leases without replaying uncertain exec |
| DG-LINUX | Both | Actual Linux cgroup controllers, ancestor constraints, complete sandbox/proxy scope and control protection; required for overall completion |
| DG-CACHE / DG-ADAPTERS | DevGuard | Safe registered-cache reclamation and additional tool adapters; not prerequisites for P1-RECOVERY |

After these prerequisites, retain the existing relative order of watch completion, file-search engine work, deterministic hooks, skills, remote environments, federation and artifacts. A contract test on a fake Linux scope is not Linux enforcement qualification.

DG-1 qualifies the standalone daemon/CLI, development workloads and bounded self-use. CS-RG then qualifies the integrated Runner, approvals, replay and saturated control paths. DG-1 does not depend on unimplemented CS-RG behavior.

DG-1 is delivered as six sequential PR groups, each reviewed, checked on its current head, normally merged and verified on main before the next. Foreground daemons are used through P4; P5 introduces a current-user LaunchAgent. At C10, first test and freeze a parent containing the new parent-budget capability, then immediately begin bounded real self-use. The earlier functional P4/P5 artifacts are not presumed to support newly added parent operations. C12 separately qualifies and promotes the measured artifact/policy/environment combination.

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

Development consumption uses the future independent CLI to govern builds and tests in both repositories. Product consumption belongs in the Runner on the actual execution host; it does not turn DevGuard into a process or PTY broker. The current UDS worker is on the same host as the gateway, not a remote worker.

The execution-owning **Runner registers once**. InProcess registers with the Gateway PID; UDS registers from the worker PID. One static control reservation covers both Gateway and Runner costs. For this SDK consumer, `service-exec` prepares credentials and startup, and the selected Runner completes registration. Service/subordinate-worker registration is deferred until multiple Runners or shared service reservations require it.

Private credential FDs survive only the required pipe/PTY helper stages and close before the user executable. The pinned Codex PTY already supports selected FD inheritance; CodeSpace's private adapter must connect it and test payload isolation. This work does not require updating the Codex pin or exposing credentials through MCP arguments.

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

<a id="resource-plan-evidence"></a>

## Plan and evidence references

The detailed planning revision is **`3abf08f6feffeda63f58b17ac2bbe8fff19ec20b`**, submitted in [DevGuard documentation PR #1](https://github.com/novelKR/DevGuard/pull/1). These immutable links remain valid before that PR is merged. English is the editorial source, with reviewed Korean counterparts and hash checks. The [English design reference](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/design.md) and [Korean planning translation](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/README.md) are maintained separately from the immutable approval artifact. This revision identifies documents, not a selected runtime client dependency. The plan defines **46 proposed implementation commit units in 23 logical PR groups** across seven milestones; future implementation remains not started.

| Authoritative planning document (English) | Use |
| --- | --- |
| [Planning index and milestone map](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/README.md) | Navigate all seven milestone plans and their commit/PR boundaries |
| [Accepted decisions](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/decisions.md) | Registration/recovery alternatives, rationale and reconsideration conditions |
| [Consumer readiness](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/consumer-readiness.md) | Generic minimum conditions and supported platform claims |
| [CodeSpace integration specification](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/codespace-integration.md) | Current source paths, mode-specific registration, execution, failures and recovery |
| [CS-RG work packages](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/milestones/CS-RG.md) / [P1 recovery work packages](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/milestones/P1-RECOVERY.md) | CodeSpace's proposed commits, tests, entry/exit gates and rollback |
| [Verification](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/verification.md) / [PR delivery](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/planning/pr-delivery.md) | Current versus proposed commands, evidence, SLOs and review handoff |

Qualification retains a 10-minute idle baseline, at least 30 minutes of load and three repetitions, with raw measurements. Local control latency and remote network time are separate. The documented initial targets remain process status p99 ≤500ms and termination acknowledgement p99 ≤1 second, with actual scope termination measured separately. A new documentation commit or passing fake-backend test does not qualify an OS control or product SLO.

The source baseline is CodeSpace commit `e94d21475643608ad2a466256fb57266b86faa47`. Codex remains pinned to `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`. The approved DevGuard design has SHA-256 `97b67a1f9518c1781156a4b3b26829b285f84f5c9a44da60f3c5dcf1bc768df8` and is stored as `docs/design.ko.md` in the independent repository.

The initial DG-0 source reference is [DevGuard commit d59cbd4](https://github.com/novelKR/DevGuard/tree/d59cbd43d206a9a9281328a946eddf1dc199f710), with [macOS and Ubuntu contract CI](https://github.com/novelKR/DevGuard/actions/runs/35671367559). This identifies the reviewed foundation; it is not a CodeSpace runtime client pin. The [immutable design](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/docs/design.ko.md) and [milestone ledger](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/milestones.json) are available without the local checkout.

The authorized local source is `/Volumes/DevData/Projects/IdeaProjects/DevGuard`. Its `milestones.json`, `docs/contracts.md` and `docs/milestones.md` distinguish implemented work from platform qualification. Run `python3 scripts/validate.py` there with Rust 1.95.0 to generate an exact-source DG-0 report. Reports explicitly leave real OS controls, browser SLOs, self-governed candidate execution and CodeSpace integration as `not_run` until their milestones are completed. The existing roadmap commit `fb822fc24c98f6628dce62d33a5cc67275f8ca34` is included in this documentation delivery; the runtime baseline remains the separate commit above.

Use the English design reference and accepted decisions for ongoing development; preserve the full approved design as historical evidence for configuration and CLI examples. They are not installation instructions for the current CodeSpace release. Keep `target/upstream-reports/local`, operational databases, Git metadata and stable recovery artifacts outside automatic cache reclamation.
