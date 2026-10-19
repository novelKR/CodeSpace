<a id="recovery-qualification-report"></a>

# Gateway recovery qualification report

[English](recovery-qualification-report.md) | [한국어](ko/recovery-qualification-report.md)

This page reports the qualification of Gateway recovery (P1-RECOVERY) by unit P1R-C06, under the [Gateway recovery qualification protocol](recovery-qualification.md). The protocol says what a run must show; this page records what the runs on the frozen candidate showed and where their evidence is. Counts, durations and percentiles are what these runs observed on one host. They are not failure probabilities.

**Result:** the [independent Runner](runner-isolation.md#independent-runner) is qualified on macOS for the [combination](#recovery-report-combination) below: `uds` with `--runner-lifecycle independent` and a Runner the operator starts, with pipe and PTY executions, participations `off` and `required`, and both in one gateway. Linux `off` is CI functional verification only, Linux `required` stays unsupported until DG-LINUX, and restoring processes after the Runner itself restarts is unsupported, as [claimed](#recovery-report-claims) below. On the same candidate, the modes that CS-RG qualified passed CS-RG's suites again. The independent mode stays opt-in, and making it the default is a separate decision. How an operator adopts it and rolls it back is in [adoption and rollback](operations.md#independent-runner-adoption).

<a id="recovery-report-claims"></a>

## What is claimed

The levels are those of the [CS-RG report](cs-rg-qualification-report.md#report-claims): **implemented**, merged in CodeSpace's main branch; **feature-verified**, shown by tests with fakes, scripted workers or DevGuard's fixture authority, in CodeSpace's tests or CI; **platform-verified**, shown on macOS against DevGuard's own components by a suite of the protocol; **qualified**, the exact combination passed every suite that the protocol requires for the claim, and its sealed evidence was verified and reviewed.

| Mode or behavior | Level | Shown by |
| --- | --- | --- |
| macOS, `uds` with `--runner-lifecycle independent`, pipe and PTY, `required` | qualified | the [functional suites](#recovery-report-functional) with `required`, the [restart measurement](#recovery-report-measurement), the [regressions](#recovery-report-regressions) |
| macOS, the same, `off` | qualified | the functional suites with `off`; the measurement's ungoverned workspaces |
| macOS, `off` and `required` workspaces in one gateway | qualified | the measurement, whose every gateway serves governed and ungoverned processes |
| Linux, the same, `off` | feature-verified: CI functional tests only, never a Linux qualification (decision D7) | CodeSpace's tests of the independent Runner in Linux CI on the candidate's commit; the harness's 34 `off` scenarios also passed on Linux with the candidate's harness and code ([#115](https://github.com/novelKR/CodeSpace/pull/115)) |
| Linux, the same, `required` | unsupported until DG-LINUX | refused before anything runs, as every Linux `required` execution is: DevGuard has no native host evidence on Linux, so no execution owner registers there, as CS-RG's [Linux CI](cs-rg-qualification-report.md#report-regressions) shows |
| Restoring processes after the Runner itself restarts | unsupported, outside P1-RECOVERY; the refusal is platform-verified | observation `runner-replaced` |
| A host restart | feature-verified; a real restart is not qualified | CodeSpace's tests give a record another boot ID, which is reported `host_rebooted` |
| Another process of the same user claiming the Runner while no gateway controls it | possible by design (decision D6); that it is logged and fenced is platform-verified | fence `stale-epoch` |
| Output beyond the Runner's 256 KiB ring while no gateway reads | lost by design; that the loss is declared is platform-verified | observation `ring-overflow`; the measurement's bursts |
| A PTY process after the Runner's loss | its root is not recorded, so the operator ends it before releasing its record; platform-verified | observation `runner-loss` on a PTY |
| A replayed preparation or launch message on the runner wire | not tested, a known gap | epoch fencing refuses requests from an earlier controller (fence suite) |
| Recovery under DG-1's warm load | not measured | the measurement uses the cold load |
| The modes CS-RG qualified | passed CS-RG's suites again on this candidate; CS-RG's own qualified combination stays [`ca306ee`](cs-rg-qualification-report.md#report-combination) | [regressions](#recovery-report-regressions) |

<a id="recovery-report-combination"></a>

## The qualified combination

The `candidate` suite recorded it on 2026-10-18 at 16:31 UTC, after the five check suites had passed on the same commit and binaries. The rehearsal and the measurement started only because the record matched the combination that the checks had run on, and the code-tree hash was also recomputed from the commit independently.

| Part | Value |
| --- | --- |
| CodeSpace | commit `1c0eaddd81fdd17d35da673b0c8fbe750fe4ac60`, the merge of [#115](https://github.com/novelKR/CodeSpace/pull/115), with no uncommitted change; code-tree SHA-256 `cb18534dedbd001a1de2f42c930ee303d41791dee279a36a7e5fe1b7de808d38`. The commit that adds this report changes documentation only and keeps that code tree |
| DevGuard client | `devguard-client` 0.1.0 and `devguard-contract` 0.1.0 from `novelKR/DevGuard` at `6e7e065a0dd5b3876dc7f4546ab76069489937d1` ([client pin](upstream-lock.md#devguard-client-pin)), as for CS-RG |
| Codex | gitlink `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` (`rust-v0.154.0`) with the [recorded patch](upstream-lock.md#codex-downstream-patch), patched tree `1999937194e3a68d982be85679557f0e50fe5408`; `codex_patch.py check` passed |
| Wire and protocol | CodeSpace's runner wire 11; DevGuard protocol 1; MCP `2025-11-25`, as the harness speaks it |
| Binaries | `codespace-mcp` 0.6.0 (`-p codespace-server --features devguard`, release) `18e6f6da6becd3be35f4a3591c7ec047d999eade561d346b2ea6ea4bb6065027`, 40,183,376 bytes; `codespace-codex-runtime` (`--features devguard`, release), which is both the `uds` worker and the independent Runner, `fa3959b430fa93f7a874970dd0e4c77b274f97720d9849ba7bc6e548eeb80237`, 27,718,336 bytes. For the fixture scenarios, `fixture_authority` `ea234acf2a3ddf645d4e5a3ad8ab163cfa81df003d323070ac754c035b3ff124` and `devguard-launch` `85c2165e95e19ca52e8a6c213b3efa4f48184de335a828b5409b737dce0e5bf0`, both built from the client pin; for the load, `devguard-qualify` `694c838621b14762dc2ad4af5139a48041764fe44fd62bed005a501d6eb68cc1`. These three are CS-RG's |
| DevGuard release | `0.1.0-6e7e065-94d85365`, built from `6e7e065`; `MANIFEST.json` SHA-256 `4535b52cc6466e892ebee2a2ad52f396e6b7520d7560e7e25c20ffa3647ed0df`; the installed `devguardd`, `devguard` and `devguard-launch` match it, and the service is healthy and runs it with process ID 23023, as it did for CS-RG |
| Policy | unchanged since CS-RG: configuration fingerprint `8cbb019bbb58e92e2a0d65f797cd5eb5f1696ce9e254bfdb1af97395382efc6f`; `devguard doctor --require admission,registration,macos-cooperative` satisfied; capabilities `durable_admission`, `fenced_launch`, `per_resource_evidence`, `static_control_reservations` and `macos_cooperative`. The `codespace` consumer has the role `control_service`, generation `codespace-g1`, one instance and a control reservation of 150 mCPU, 536,870,912 bytes and 40 tasks. The SHA-256 of `host.toml` is in the sealed record |
| DG-1 qualification | DevGuard's record of the release, SHA-256 `fd4fed318e0fbc640d9e6174a8dd731db80f9607d2235bdfa773c4101f6b17a4`: cold and warm loads qualified under the same `host.toml`, fingerprint and capabilities |
| Toolchain | `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`, Python 3.9.6, Xcode 27.0 (27A266a) |
| Host | a Mac mini (Macmini9,1, Apple M1) with 8 logical CPUs and 16 GiB, on AC power, macOS 27.0 (26A428); work capacity after the consumer's reservation 5,350 mCPU, 12,079,595,520 bytes and 104 tasks |

<a id="recovery-report-runs"></a>

## Runs

Every run on the qualification host used the candidate's binaries in the minimal environment, with Rust 1.95.0 first in `PATH`, and started after checking that nothing was charged, no `codespace` instance was active and memory pressure was normal. Each run was sealed, and `verify` recomputed its verdicts from the raw files and found them unchanged. The check suites, the freeze, the rehearsal and the measurement ran from one reviewed command in the owner's Terminal, each only after the one before it was clean, under Terminal's limit of 256 open files; the CS-RG suites ran afterwards in the session that had built the binaries, as CS-RG ran them.

| Run | When (UTC) | Result |
| --- | --- | --- |
| The five check suites, `recovery-lifecycle`, `recovery-store`, `recovery-fence`, `recovery-reconcile` and `recovery-observation` | 2026-10-18 16:26–16:31 | passed: 55 scenarios with 650 checks, none failed; the two gaps recorded as not run, with their reasons |
| `candidate` | 2026-10-18 16:31 | passed |
| `recovery --rehearsal` | 2026-10-18 16:31–16:42 | inconclusive only because a rehearsal is shorter than the protocol, as designed; its functional scenarios and both intervals passed |
| `recovery` | 2026-10-18 16:42–18:52 | qualified: the functional scenarios passed, and all six intervals of the three repetitions |
| CS-RG's `candidate`, `matrix`, `faults` and `authority` | 2026-10-19 02:22–02:31 | passed, as in [regressions](#recovery-report-regressions) |
| `verify` of every P1R-C06 directory | 2026-10-19 02:31 | all 35 verified, those of the earlier candidates included |
| CI on the candidate's commit | 2026-10-18 09:18–09:31 and 21:51–22:02 | passed: every job of the push run after the merge and of the scheduled full run |

<a id="recovery-report-functional"></a>

## Functional suites

Each functional scenario ran three times on the candidate: in the five check suites before the freeze, and as the first part of the `recovery` rehearsal and of the `recovery` run. Each time all 55 scenarios that run on macOS passed, 34 with `off` and 21 with `required`, with the same 650 checks, none of which failed, and the two that the protocol lists as [gaps](recovery-qualification.md#recovery-gaps), a host restart and restoring processes after the Runner restarts, were recorded as not run, with their reasons.

| Suite | With `off` | With `required` | Checks, `off` / `required` |
| --- | --- | --- | ---: |
| `recovery-lifecycle` | exit-kinds and original-timeout, each on a pipe and on a PTY; runner-sigterm; invalid-settings | exit-kinds and original-timeout, each on a pipe and on a PTY; runner-sigterm | 98 / 95 |
| `recovery-store` | records on a pipe and on a PTY; writer-lock; schema; records-command | records on a pipe and on a PTY | 49 / 28 |
| `recovery-fence` | foreign-connection on a pipe and on a PTY; stale-epoch; stopped-gateway; concurrent-claims; other-database; concurrent-gateways | foreign-connection on a pipe and on a PTY | 58 / 30 |
| `recovery-reconcile` | running, terminal, kill-during-exec and kill-during-terminate, each on a pipe and on a PTY; approval; missing-record | running, terminal, kill-during-exec and kill-during-terminate, each on a pipe and on a PTY; approval; authority-outage | 89 / 110 |
| `recovery-observation` | ring-overflow and runner-loss, each on a pipe and on a PTY; retention; runner-replaced | runner-loss on a pipe and on a PTY | 55 / 38 |

The protocol's [functional suites](recovery-qualification.md#recovery-functional) say what each scenario shows. Every `required` scenario ran against a fresh fixture authority with its own `codespace` consumer of one instance. The waits that [#114](https://github.com/novelKR/CodeSpace/pull/114) added are recorded. In `kill-during-terminate` with `required`, 6 of the 16 rounds of the `recovery-reconcile` check, of the rehearsal and of the run each met one refusal when it sent new work to the next gateway, while that gateway reconciled the workspace or still held it for the earlier process, and started on its second try, 85 to 102 ms after its first. No round with `off` met one.

<a id="recovery-report-measurement"></a>

## Gateway restart measurement

`recovery` ran the full protocol from 16:42 to 18:52 UTC on 2026-10-18, right after its rehearsal (16:31–16:42), which was inconclusive only because a rehearsal is shorter than the protocol. After its functional scenarios (16:42–16:46), the measurement ran three repetitions of a 10-minute idle interval and a load interval of at least 30 minutes, governed, under DG-1's cold load against the installed release. The idle intervals lasted 606 to 607 seconds and the load intervals 1,837 to 1,850 seconds, because an interval completes its last cycle, and the load its runs, after the planned end. All six intervals and all three repetitions passed, so the combination qualified, and so did the run. Every interval began with nothing charged and no `codespace` instance active, and the service ran the release with process ID 23023 before and after every repetition.

Each idle interval restarted the gateway 26 times and each load interval 76 or 77 times, against a plan of one restart every 25 seconds, of which the protocol needs 80%. The gateways were ended in turn with their input closed, SIGTERM, SIGUSR1 and SIGKILL, each as asked, and each next gateway claimed the Runner under the next epoch and found every long-running process `running`. Served is the time a gateway served, from its answer to MCP `initialize`, which the samples follow; the rest is mostly the 4-second pauses that make a deadline fall while no gateway runs. Times are in milliseconds.

| Repetition | Interval | Seconds | Served | Restarts (planned) | Input closed, SIGTERM, SIGUSR1, SIGKILL | Start to serving p50 / max | Verdict |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | idle | 607 | 558 | 26 (24) | 6, 7, 7, 6 | 28.9 / 131.8 | pass |
| 1 | load | 1,850 | 1,654 | 76 (72) | 19, 19, 19, 19 | 14.5 / 4,062.6 | pass |
| 2 | idle | 607 | 558 | 26 (24) | 6, 7, 7, 6 | 31.2 / 111.4 | pass |
| 2 | load | 1,842 | 1,656 | 77 (72) | 19, 20, 19, 19 | 15.2 / 4,117.7 | pass |
| 3 | idle | 606 | 557 | 26 (24) | 6, 7, 7, 6 | 29.3 / 88.8 | pass |
| 3 | load | 1,837 | 1,658 | 77 (72) | 19, 20, 19, 19 | 15.1 / 1,659.5 | pass |

A new gateway served within 132 ms of its start in the idle intervals. Under load the median stayed at 15 ms, but the slowest start took 4.1 seconds in the first two repetitions and 1.7 seconds in the third. The protocol has no target for this time, so it is reported, not judged.

| Repetition | Interval | Fence requests (rivals) | Timeout targets (deadline in a gap) | Latest end after the deadline | Approvals (resume in flight at SIGKILL) | Longest resume | Tagged commands started, refused |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | idle | 405 (27) | 25 (12) | 103.5 | 26 (6) | 269.8 | 56, 20 |
| 1 | load | 1,155 (77) | 75 (35) | 104.1 | 76 (19) | 273.5 | 161, 58 |
| 2 | idle | 405 (27) | 25 (12) | 101.6 | 26 (6) | 279.6 | 56, 18 |
| 2 | load | 1,170 (78) | 76 (38) | 113.0 | 77 (19) | 2,014.1 | 164, 57 |
| 3 | idle | 405 (27) | 25 (12) | 119.4 | 26 (6) | 261.3 | 55, 18 |
| 3 | load | 1,170 (78) | 76 (37) | 103.7 | 77 (19) | 331.1 | 162, 62 |

After every restart the other process's Claim was refused, and its 15 requests under the previous, the current and no epoch were each refused `STALE_CONTROLLER` and changed nothing; every rival gateway waited for the writer and gave up without reaching the Runner. Every timeout target was reported `timeout` and ended at most 120 ms after its original deadline, against the protocol's 2 seconds, whether its deadline fell while a gateway ran or while none did; none ended early or had its deadline moved. Every approval was resumed by the next gateway, a second resume returned the same process, and its command ran once, the 75 whose resume was in flight when a SIGKILL ended the gateway included. A resume took at most 331 ms from its first try to its answer, waits for the workspace's reconciliation included, except in the second repetition's load interval, where the longest took 2,014 ms. No tagged command ran twice, and none that was refused ran.

The control path's targets are a `process_status` p99 of at most 500 ms and a termination acknowledgement p99 of at most 1 second, over the time a gateway served; the other times have no established target and are reported. With fewer than 100 samples, as for terminations, the nearest-rank p99 is the largest value. The schedule follows the time a gateway served, so a count can be a little above or below it.

| Repetition | Interval | Status p99 / max | Status samples / schedule | Acknowledgement p99 | Terminations / schedule | Scope ended p99 | Released p99 | I/O p99 | Refusal p99 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | idle | 3.283 / 7.786 | 1,120 / 1,116 | 0.540 | 27 / 27 | 79.1 | 188.6 | 22.942 | 49.405 |
| 1 | load | 1.882 / 41.516 | 3,297 / 3,306 | 0.487 | 82 / 82 | 92.6 | 195.3 | 2.390 | 48.449 |
| 2 | idle | 3.137 / 9.269 | 1,116 / 1,114 | 0.739 | 27 / 27 | 77.5 | 185.7 | 6.239 | 46.620 |
| 2 | load | 1.475 / missing | 3,312 / 3,312 | 0.508 | 84 / 82 | 66.7 | 182.2 | 17.081 | 47.541 |
| 3 | idle | 2.651 / 8.858 | 1,116 / 1,114 | 0.543 | 26 / 27 | 65.6 | 161.2 | 5.523 | 41.619 |
| 3 | load | 1.497 / 15.581 | 3,316 / 3,314 | 0.486 | 82 / 82 | 76.8 | 184.3 | 2.640 | 53.347 |

In the second repetition's load interval one status slot of each target was missed: its moment passed while the sample before it still ran, so it counts as a missing sample, as DevGuard's protocol counts it. A missing sample ranks above every answer, so that interval's maximum shows as missing; its p99 was 1.475 ms. No status sample was an error or an answer about a target that no longer ran. Every acknowledged termination ended its target and was released, and each status target's final termination was effective. Every I/O line came back, the slowest after 506 ms. Every refusal was `RESOURCE_UNAVAILABLE`, and no request that had to be refused was admitted. Two termination targets and two refusals met one `RECOVERY_PENDING` from a gateway that had just started, one of each in the first repetition's idle interval and in the third's load interval, and were sent again, as the protocol allows. No sample lost the MCP connection.

Both output streams were read through every gateway. The PTY's numbered lines came through with none skipped or repeated. Each 40,000-line burst of the pipe was written while no gateway read it, and each time the Runner declared a gap (`ring_overflow`) for the 22,524 lines beyond its 256 KiB ring, after which the other 17,476 came through. No line was lost or repeated outside a declared gap.

| Repetition | Interval | PTY lines read | Pipe lines read | Declared gaps | Lines in declared gaps |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | idle | 2,825 | 52,428 | 3 | 67,572 |
| 1 | load | 8,795 | 157,284 | 9 | 202,716 |
| 2 | idle | 2,825 | 52,428 | 3 | 67,572 |
| 2 | load | 8,756 | 174,760 | 10 | 225,240 |
| 3 | idle | 2,822 | 52,428 | 3 | 67,572 |
| 3 | load | 8,765 | 174,760 | 10 | 225,240 |

**Launches.** All 967 attempts that the run's gateways logged or its samples named were found in DevGuard's journal; no attempt was launched twice, no uncertain attempt was admitted again, and nothing stayed charged. The journal recorded 4 more launches than the gateways' logs in each idle interval and 13 or 14 more in each load interval: a launch that the Runner made for a gateway killed before it logged it is in the journal and in no gateway's log. These are reported, not counted as duplicates; the marker files, one line for each run of each tagged command, show that no command ran twice.

**Load.** DG-1's six consumers made 583 runs in the three load intervals, 193 to 196 in each, and every run succeeded: none exited with a nonzero status, failed to start or was uncertain, and admitted load covered 99.99% of each load interval. Every consumer ran in every interval: `cargo` 19 to 21 times, `cpu` and `memory` 15 times each, `io` 58, `output` 58 or 59 and `slow-input` 28.

**Host and footprint.** Every host sample of every interval was at memory pressure level 1 (normal); swap peaked at 1,378 MiB, and the load average at 7.92, in the second repetition's load interval. The Runner, which owns every execution in this mode and registers as the `codespace` consumer's instance, stayed within the consumer's reservation of 150 mCPU, 512 MiB and 40 tasks in every interval: its peaks were 15.25 MiB of RSS and 15 threads, with a mean CPU of at most 2.9 mCPU. Each gateway peaked at 21.3 MiB and 12 threads, with a mean CPU of at most 4.9 mCPU. This comparison is reported, not judged.

**Evidence writer.** The harness's writer of raw evidence fell behind by at most 33 ms in the idle intervals and by up to 6.7 seconds in the load intervals, while DG-1's cold builds shared the evidence disk. Since [#115](https://github.com/novelKR/CodeSpace/pull/115) each gateway's log is parsed before the writer stores it, and every termination sample was watched until DevGuard released it.

**Rehearsal and afterwards.** The rehearsal's two intervals, 127 seconds idle and 216 seconds of load with 5 and 7 restarts, passed with the same zero counts, a status p99 of at most 6.329 ms and an acknowledgement p99 of at most 0.916 ms. After the run the last interval's Runner had been drained: nothing was charged, no lease was held, every `codespace` instance in DevGuard's journal was retired, and the service still ran the release with process ID 23023, so it did not restart during the rehearsal or the run.

<a id="recovery-report-completion"></a>

## Completion evidence

The protocol's [completion evidence](recovery-qualification.md#recovery-completion), with what the runs on the candidate showed:

| What P1-RECOVERY must show | Where it is shown | Result |
| --- | --- | --- |
| The same execution is reached again after a restart | the measurement's 308 restarts, after each of which the next gateway found every long-running process `running` and kept reading both output streams; lifecycle `exit-kinds`; fence `stale-epoch` | shown: no long-running process was lost, and no line was lost or repeated outside a gap the Runner declared |
| No execution runs twice | the measurement's 308 approvals, its 654 tagged commands that started and 233 that were refused, and DevGuard's journal against the gateways' logs; reconcile `approval`, `kill-during-exec` and `kill-during-terminate` | shown: each command ran once, none that was refused ran, and no attempt was launched twice |
| No stale mutation | each cycle's 15 requests of the other process, 4,710 in all, and its rival gateway, 314 in all; the fence suite | shown: every request was refused as stale and changed nothing, and no rival reached the Runner |
| No timeout extension | the measurement's 302 timeout targets, 146 of them with the deadline while no gateway ran; lifecycle `original-timeout` | shown: none was extended, ended early or misreported, and the latest ended 119 ms after its deadline |
| Explicit gaps | the protocol's [gaps](recovery-qualification.md#recovery-gaps) | recorded as not run, with their reasons, in the `recovery-observation` check, the rehearsal and the run |
| Existing modes still pass | [regressions](#recovery-report-regressions) | shown: CS-RG's four suites on the candidate, and CI on its commit |
| Supported modes | the [claims](#recovery-report-claims) above | macOS `off` and `required` qualified; Linux `off` CI functional verification only; Linux `required` unsupported |

<a id="recovery-report-regressions"></a>

## Regressions

After the run its last Runner had been drained, and DevGuard showed no active `codespace` instance and nothing charged, so CS-RG's suites could register their own gateways. They ran on the same candidate on the qualification host, from 02:22 to 02:31 UTC on 2026-10-19, in the session that had built the binaries, under that session's limit of 1,048,576 open files; they say nothing about Terminal's limit of 256. Each passed and was verified:

| Suite | When (UTC) | Result | On CS-RG's candidate `ca306ee` |
| --- | --- | --- | --- |
| `candidate` | 02:22 | passed; the record differs from the frozen one only in the time it was taken | passed |
| `matrix` | 02:22–02:23 | passed: 45 of 45 scenarios, 22 in `in-process` and 23 in `uds`, with 210 and 213 checks | passed: 45 of 45 |
| `faults` | 02:23–02:26 | passed: 27 of 27 cases, 13 and 14, with 83 and 89 checks | passed: 27 of 27 |
| `authority` | 02:27–02:31 | passed: 10 of 10 phases; no sample lost the MCP connection | passed: 10 of 10 |

Afterwards nothing was charged, no `codespace` instance was active, and the service still ran the release with process ID 23023.

CI ran on the candidate's commit twice, and every job passed both times: the push run after the merge of [#115](https://github.com/novelKR/CodeSpace/pull/115) ([run 39114783991](https://github.com/novelKR/CodeSpace/actions/runs/39114783991), 09:18 to 09:31 UTC on 2026-10-18) and the scheduled run with the full profile ([run 39169645390](https://github.com/novelKR/CodeSpace/actions/runs/39169645390), 21:51 to 22:02 UTC). Each ran the format and pin check, the policy scan, the three Clippy builds and the seven unit stages, `integration` and `linux-isolation` on Ubuntu, and `macos-core`, `macos-fork-race` and `dependencies` on a hosted macOS runner. These are the regression stages on Linux and macOS, the independent Runner's tests included; on Linux they are the functional verification of `off` [claimed](#recovery-report-claims) above.

<a id="recovery-report-amendments"></a>

## Amendments and corrections

The candidate was frozen four times. Each of the first three froze after the five check suites had passed on it, and each failed later on a defect of the harness, never of CodeSpace or DevGuard. Each fix changed the harness, which is frozen with the protocol, so each made a new candidate, and the runs were repeated from the checks. Every run on the earlier candidates is kept and listed under [evidence](#recovery-report-evidence).

- **The open-file limit, `b2cb042` (correction).** The first candidate was the merge of [#112](https://github.com/novelKR/CodeSpace/pull/112). Its five check suites passed in a session with a high limit of open files, and its rehearsal, run from Terminal, whose default limit is 256, failed as `harness_failed`: the harness kept every gateway's pipes and transcripts open, so from the 27th functional scenario on, each failed with `Too many open files` in the harness itself, and the load interval's drain could not open the journal. [#113](https://github.com/novelKR/CodeSpace/pull/113) made the harness close what it no longer reads, which keeps at most 43 files open through a whole rehearsal, and every report now records the limit it ran under. The next candidate, `41f83b6`, passed its checks under a limit of 256.
- **Races after a restart, `41f83b6` (correction).** Its full run failed on three harness races in a gateway's first milliseconds, while the gateway refused new work as the product documents: a termination sample sent 5 ms after a gateway's Claim and before it had reconciled the sample's workspace was refused `RECOVERY_PENDING` and counted as an error; in reconcile `approval` with `required`, two rounds held their execution before the new gateway had reconciled the workspace; and in `kill-during-terminate` with `required` on a pipe, new work was refused `WORKSPACE_BUSY` less than a millisecond after `process_status` reported the earlier process exited. No command ran twice. [#114](https://github.com/novelKR/CodeSpace/pull/114) made the harness send such work again while it meets one of these refusals, as the protocol now says ([functional suites](recovery-qualification.md#recovery-functional), [intervals](recovery-qualification.md#recovery-intervals)), and record how long each sample and round waited.
- **The gateway's log on a busy disk, `b5ddd59` (correction).** Its full run failed on one termination sample of 84 in the second load interval, which the harness could not watch: its reader of the gateway's log wrote each line to the evidence before parsing it, and while DG-1's cold builds kept the evidence disk busy, the attempt ID arrived after the sample's 5-second wait. DevGuard released that attempt `scope_terminated`. [#115](https://github.com/novelKR/CodeSpace/pull/115) made the reader parse each line first and write the log on a thread of its own, as the [protocol](recovery-qualification.md#recovery-running) now says; a log it cannot write makes the interval inconclusive. On `1c0eadd` the writer still lagged up to 6.7 seconds under load, and no launch line was missed.
- **Who ran the checks and the freeze (amendment).** On `41f83b6` the session that drives the qualification host could not run `candidate`: its safety check refused the command. From then on the owner ran the check suites, `candidate`, the rehearsal and the measurement as one reviewed command in Terminal, and that command started each step only once the one before it was clean, and the rehearsal only once the record matched the checked combination: commit, code tree, the five binaries, release, policy fingerprint, DG-1's policy and memory pressure. The protocol's order is unchanged.
- **How long `WORKSPACE_BUSY` lasts (note).** The gateway's guidance for this mode says that a process found running holds its workspace, so that new work there returns `WORKSPACE_BUSY` "until the process exits or is terminated". The workspace is freed when the Runner's exit event arrives, once the process's output has closed, which can follow the moment `process_status` first reports the process exited. A client that sends new work again after this refusal, as the harness now does, is unaffected. The text is unchanged here, because this report changes documentation only; making it exact is a separate change.

<a id="recovery-report-evidence"></a>

## Evidence

The evidence directories are on the qualification host, under the DevGuard checkout's `evidence/` directory. Each is sealed with a `MANIFEST.json` of every file's SHA-256, and its files are read-only. None holds a credential, a credential digest or a child's environment. A directory checks out when its `MANIFEST.json` has the SHA-256 below and `qualify-devguard.py verify` accepts it. The console and `verify` output of every run are kept beside its directory.

| Directory | Run | `MANIFEST.json` SHA-256 |
| --- | --- | --- |
| `p1r-c06-check-recovery-lifecycle-2026-10-18-1c0eadd` | `recovery-lifecycle` | `ea8f534f434080e9eb33909a8b4aebb98bd6c3791996fb8ca424c0307d0b1da3` |
| `p1r-c06-check-recovery-store-2026-10-18-1c0eadd` | `recovery-store` | `45123bc349de4341202cfdbcfce19ee182584ad4f1272b8d131d3b276b506197` |
| `p1r-c06-check-recovery-fence-2026-10-18-1c0eadd` | `recovery-fence` | `299e646f77497f7ae8a2d7c58f92b89ea59499f562d86ca648d64f3d9675e6fb` |
| `p1r-c06-check-recovery-reconcile-2026-10-18-1c0eadd` | `recovery-reconcile` | `a176626961017483061f4d4a9334ebf4811037782170084697c43bbf66d4164c` |
| `p1r-c06-check-recovery-observation-2026-10-18-1c0eadd` | `recovery-observation` | `37fcf791b8bfca09bf1c805141ee7108a44448a790551611e2c62699a6ffa407` |
| `p1r-c06-candidate-2026-10-18-1c0eadd` | `candidate` | `c7b26e3dc118e4b07b27255e038dcb02d2e1b411ef073d6027754f60fb4622ad` |
| `p1r-c06-recovery-rehearsal-2026-10-18-1c0eadd` | `recovery --rehearsal` | `ef5ab907a9fbe03fa737b2b23d0f2023e64b36685b466248465a32a5a0880894` |
| `p1r-c06-recovery-2026-10-18-1c0eadd` | `recovery` | `1f6f7ba8e4694d4aba6aa371db2edaeeb6fc0b0d7812dd17847444d1e06f790b` |
| `p1r-c06-qual-candidate-2026-10-19-1c0eadd` | CS-RG's `candidate` | `caff08ef246a57f5e673e7b54a41fd3d99376b3c14d12d463270911c2750898f` |
| `p1r-c06-qual-matrix-2026-10-19-1c0eadd` | `matrix` | `33443fce5b6d0261e74cfe77a317a7de3bcbd019836331c3a63cefca40e2011f` |
| `p1r-c06-qual-faults-2026-10-19-1c0eadd` | `faults` | `bb29b613eff8df0c8df8108f0d1ba953b5cc9298a2c79cafbee6f4a280b55936` |
| `p1r-c06-qual-authority-2026-10-19-1c0eadd` | `authority` | `7cebeacfb92cb39b6956607b9cc2e80663102ce307152cf438a7bb4229bfbb65` |

The runs on the earlier candidates are kept too. Each was verified with its own harness when it ran, and `verify` on the candidate's harness accepted every one again on 2026-10-19; the rehearsal on `b2cb042`, recorded `harness_failed`, recomputes as failed:

| Directory | Result | `MANIFEST.json` SHA-256 |
| --- | --- | --- |
| `p1r-c06-check-recovery-lifecycle-2026-10-17-b2cb042` | passed | `b401d3c878d95148f2aa3f42d0b6013fa0a5e0a723c2f0acf352809e5d5b9ab6` |
| `p1r-c06-check-recovery-store-2026-10-17-b2cb042` | passed | `c0aaca7ed5f5c917ec5711c4f8b435a8fb49b616c284ddef8298c990974c7b0a` |
| `p1r-c06-check-recovery-fence-2026-10-17-b2cb042` | passed | `5efb6bf4d9830fbbe50cbce31a0ac0f570ec3a67787eb9b481a18a42063dc778` |
| `p1r-c06-check-recovery-reconcile-2026-10-17-b2cb042` | passed | `d70cd660821dd558c75d0fa5a8f9c5a97029c8147b28cd446335112077a59193` |
| `p1r-c06-check-recovery-observation-2026-10-17-b2cb042` | passed | `971fc90242a11cbd4d9db405d26f6e79ecf12f41ddf6ad3b415f3655383f52d9` |
| `p1r-c06-candidate-2026-10-17-b2cb042` | passed | `15c6735be844292b3f84ad74ce20a534d1ec6b9d9bdc38ab7988f98d971f87c7` |
| `p1r-c06-recovery-rehearsal-2026-10-17-b2cb042` | `harness_failed`: the open-file limit | `5f58673c69f35c993852953daf4d5a09ff0dbaf2c857a7fca343aeb507fac631` |
| `p1r-c06-check-recovery-lifecycle-2026-10-17-41f83b6` | passed | `0614d740f7eef0a96941e6c36ec974666b22a301f3f958e0642f29d05a6efb62` |
| `p1r-c06-check-recovery-store-2026-10-17-41f83b6` | passed | `395b4c051b483c3e9609bf3aaee4ecfbb19df82ad3ae213156fa652fc3cddfe8` |
| `p1r-c06-check-recovery-fence-2026-10-17-41f83b6` | passed | `f81578c3398efefda24a9f2758b53499d5ebb969d93b80800825ba5218824271` |
| `p1r-c06-check-recovery-reconcile-2026-10-17-41f83b6` | passed | `9ad23449bb1bc765f99b937ee9b7ad8716b8864f6ee72a97e3b2d53e0d54dfa1` |
| `p1r-c06-check-recovery-observation-2026-10-17-41f83b6` | passed | `66434755ffe5ffb3aa0c60fb28c4397f188d0ff6bd0fdc91bacd92813c3a8921` |
| `p1r-c06-candidate-2026-10-17-41f83b6` | passed | `37ed6aa3967f25602dc00f7ce7c40bb8b773ecea102329e2c50ccdb5c94b4fdc` |
| `p1r-c06-recovery-rehearsal-2026-10-17-41f83b6` | inconclusive, as designed | `aad4c41e36b6d6ddcffe76e24de32fd013f765d781c3de7e70ae5c1e28cfefd6` |
| `p1r-c06-recovery-2026-10-17-41f83b6` | failed: races after a restart | `1b2b57fbea66ae3a08b78dcd76d86f992dc4709a21e22a51343b97cf0f0b22a2` |
| `p1r-c06-check-recovery-lifecycle-2026-10-18-b5ddd59` | passed | `221a31cd238d47ec57190b1355994f473c7fa2592185752bcc81ae647c9ff501` |
| `p1r-c06-check-recovery-store-2026-10-18-b5ddd59` | passed | `b8533e38c2a663dac17dd23e9789d0e30aec9c02cb65870d1bd1cf4800c0991b` |
| `p1r-c06-check-recovery-fence-2026-10-18-b5ddd59` | passed | `9109c738e3f1e4e0279368a37fcefd6b4ef9a86414dbe7b0921ea6b16ebe4048` |
| `p1r-c06-check-recovery-reconcile-2026-10-18-b5ddd59` | passed | `2f07f807876c13f1b7cf63120bb6e92763e19de5a9d5cebeaa3d5e753c60dfb8` |
| `p1r-c06-check-recovery-observation-2026-10-18-b5ddd59` | passed | `bec8ab11383407e43fa041ffdfe163c606ecbf8ad996d1738ac0a93b48b37f50` |
| `p1r-c06-candidate-2026-10-18-b5ddd59` | passed | `1fe4928c3666310f83da9914985627dcc9fb72ff8202d1b7ae5d95b5b7d2abf9` |
| `p1r-c06-recovery-rehearsal-2026-10-18-b5ddd59` | inconclusive, as designed | `5b2237d8c70037cd6ef1a1fa8e346063a4cf43363a3b62a99c4ca603756f1226` |
| `p1r-c06-recovery-2026-10-18-b5ddd59` | failed: the gateway's log on a busy disk | `dbdee96032d7d715a56a729cb1b6b97ace2901bc93f834e9fdf85e2dc7945f02` |

<a id="recovery-report-outside"></a>

## Outside P1-RECOVERY

P1-RECOVERY ends here. These stay outside it, and nothing here claims them:

- **Restoring processes after the Runner itself restarts.** A gateway never takes a new Runner for the one it lost; an operator settles what the lost Runner ran ([Runner loss](operations.md#runner-loss)).
- **Making the independent mode the default**, a separate decision under DevGuard's ADR-002.
- **DG-LINUX**, enforcement on Linux. Until it qualifies, `required` is refused on Linux, and `off` is functionally verified there only.
- **A change to DevGuard's release or policy**, such as more than one instance for the `codespace` consumer (decision D12), which would need DG-1 to be measured again.
- **Another combination.** A different DevGuard client pin, release, policy or host, a code change, or another toolchain is a new candidate that needs its own runs.
- **What the runs did not show:** a real host restart, DG-1's warm load, a replayed message on the runner wire, and CS-RG's control-path measurement on this candidate.
