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
| DG-1 | DevGuard | Real macOS daemon/client/launcher, generic and Cargo development consumption, candidate tests under a stable parent budget, recovery and measured responsiveness |
| CS-RG | CodeSpace | Qualified full-SHA consumption, pre-spawn process slots, PrepareExec/ExecPrepared, approval preservation, bounded control/data paths and replay, InProcess/UDS parity, upstream regression qualification |
| P1-RECOVERY | CodeSpace | Durable process identity and recovery reconciled with the resource authority; no replay of uncertain exec |
| DG-LINUX | Both | Actual Linux cgroup controllers, ancestor constraints, complete sandbox/proxy scope and control protection; required for overall completion |
| DG-CACHE / DG-ADAPTERS | DevGuard | Safe registered-cache reclamation and additional tool adapters; not prerequisites for P1-RECOVERY |

After these prerequisites, retain the existing relative order of watch completion, file-search engine work, deterministic hooks, skills, remote environments, federation and artifacts. A contract test on a fake Linux scope is not Linux enforcement qualification.

<a id="resource-consumer-boundary"></a>

## Consumer boundary

Development consumption uses the future independent CLI to govern builds and tests in both repositories. Product consumption belongs in the Runner on the actual execution host; it does not turn DevGuard into a process or PTY broker. The current UDS worker is on the same host as the gateway, not a remote worker.

The runtime adapter must distinguish an accounted reservation, a proposed execution plan and verified applied policy. Required participation does not imply a tree-wide hard limit on macOS. DevGuard's journal cannot restore CodeSpace process handles or replace the patch operations ledger.

CS-RG must prepare admission before marking an approval resume as dispatching. Resource refusal leaves an unconsumed hold queued. A timeout, lost response or missing process handle is not proof that a command did not run. New work is rejected quickly when the authority is unavailable; existing status and termination remain Runner-local.

The integration will separately qualify the pinned contract/client, daemon/helper artifacts and CodeSpace Runner wire. It must reject missing required capabilities instead of starting a second host-wide authority or silently disabling policy.

<a id="resource-plan-evidence"></a>

## Plan and evidence references

The source baseline is CodeSpace commit `e94d21475643608ad2a466256fb57266b86faa47`. Codex remains pinned to `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`. The approved DevGuard design has SHA-256 `97b67a1f9518c1781156a4b3b26829b285f84f5c9a44da60f3c5dcf1bc768df8` and is stored as `docs/design.ko.md` in the independent repository.

The initial DG-0 source reference is [DevGuard commit d59cbd4](https://github.com/novelKR/DevGuard/tree/d59cbd43d206a9a9281328a946eddf1dc199f710), with [macOS and Ubuntu contract CI](https://github.com/novelKR/DevGuard/actions/runs/35671367559). This identifies the reviewed foundation; it is not a CodeSpace runtime client pin. The [immutable design](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/docs/design.ko.md) and [milestone ledger](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/milestones.json) are available without the local checkout.

The authorized local source is `/Volumes/DevData/Projects/IdeaProjects/DevGuard`. Its `milestones.json`, `docs/contracts.md` and `docs/milestones.md` distinguish implemented work from platform qualification. Run `python3 scripts/validate.py` there with Rust 1.95.0 to generate an exact-source DG-0 report. Reports explicitly leave real OS controls, browser SLOs, self-governed candidate execution and CodeSpace integration as `not_run` until their milestones are completed.

Use the full approved design for future configuration and CLI examples. They are not installation instructions for the current CodeSpace release. Keep `target/upstream-reports/local`, operational databases, Git metadata and stable recovery artifacts outside automatic cache reclamation.
