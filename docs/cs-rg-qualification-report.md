<a id="cs-rg-qualification-report"></a>

# CS-RG qualification report

[English](cs-rg-qualification-report.md) | [한국어](ko/cs-rg-qualification-report.md)

This page reports the qualification of CodeSpace's use of DevGuard (CS-RG) by unit CSRG-U6, under the [CS-RG qualification protocol](cs-rg-qualification.md). The protocol says what a run must show; this page records what the runs on the frozen candidate showed and where their evidence is. Counts, durations and percentiles are what these runs observed on one host. They are not failure probabilities.

**Result:** CS-RG is qualified on macOS for the [combination](#report-combination) below: participation `required` in the runner modes `in-process` and `uds` with `--runtime-bin`, each with pipe and PTY executions, and `off` and `required` workspaces in one gateway. Every other mode is unsupported or keeps its existing behavior, as [claimed](#report-claims) below. Linux `required` stays unsupported until DG-LINUX. How an operator adopts the qualified combination, and how to roll it back, is in [qualification and adoption](devguard-integration.md#devguard-qualification).

<a id="report-claims"></a>

## What is claimed

The work order separates four levels of result. Here **implemented** means merged in CodeSpace's main branch; **feature-verified**, shown by tests with fakes, scripted workers or DevGuard's fixture authority, in the regression stages or CI; **platform-verified**, shown on macOS against DevGuard's own components by a suite of the protocol; **qualified**, the exact combination passed every suite that the protocol requires for the claim, and its sealed evidence was verified and reviewed.

| Mode or behavior | Level | Shown by |
| --- | --- | --- |
| macOS, `in-process`, pipe and PTY, `required` | qualified | [matrix](#report-matrix), [faults](#report-faults), [authority failure](#report-authority), [control path](#report-control), [regressions](#report-regressions) |
| macOS, `uds` with `--runtime-bin`, pipe and PTY, `required` | qualified | the same |
| macOS, `off` and `required` workspaces in one gateway, pipe and PTY | qualified | matrix `mixed` in both runner modes |
| macOS, `uds` with `--runner-socket`, `required` | unsupported; the refusal is platform-verified | matrix `runner-socket` |
| macOS, `required` under `--devguard status` | unsupported; the refusal is platform-verified | matrix `status-mode` |
| macOS, `required` without a usable launch helper | unsupported; the refusal is platform-verified | matrix `no-helper` and `unusable-helper` |
| macOS and Linux, `off` | existing behavior, feature-verified | the regression stages on the candidate, and CI |
| Linux, `required` | unsupported until DG-LINUX; the refusal is feature-verified | Linux CI on the candidate |
| A lost answer to a request past registration; a worker lost during the launch itself | feature-verified | DevGuard's client tests; CodeSpace's runner and gateway tests with a scripted worker |
| A replayed preparation or launch message on the runner wire | not tested, a known gap | |
| A journal or storage refusal | not injected; feature-verified | DevGuard's own suites; CodeSpace's tests of how its codes map |
| Control under DG-1's warm load | not measured | the qualification uses the cold load |
| Foreground browser responsiveness | DG-1's, not measured here | DevGuard's DG-1 qualification of the release |

<a id="report-combination"></a>

## The qualified combination

The `candidate` suite recorded it on 2026-10-13 at 16:23 UTC, and it was checked independently against the commit, with the code-tree hash recomputed.

| Part | Value |
| --- | --- |
| CodeSpace | commit `ca306eeaec42818c5fdb2a6ca3b6765507e8af8c`, the merge of [#98](https://github.com/novelKR/CodeSpace/pull/98), with no uncommitted change; code-tree SHA-256 `32235f28b930ec1487f6ca508ff6b0c7f77772275462080972c3cfe1d2b4245f`. The commit that adds this report changes documentation only and keeps that code tree |
| DevGuard client | `devguard-client` 0.1.0 and `devguard-contract` 0.1.0 from `novelKR/DevGuard` at `6e7e065a0dd5b3876dc7f4546ab76069489937d1` ([client pin](upstream-lock.md#devguard-client-pin)) |
| Codex | gitlink `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` (`rust-v0.154.0`) with the [recorded patch](upstream-lock.md#codex-downstream-patch), patched tree `1999937194e3a68d982be85679557f0e50fe5408`; `codex_patch.py check` passed |
| Wire and protocol | CodeSpace's runner wire 7; DevGuard protocol 1; MCP `2025-11-25`, as the harness speaks it |
| Binaries | `codespace-mcp` 0.6.0 (`-p codespace-server --features devguard`, release) `1e091e02b648c24bb933d90b1347ecc22119ba0745d401c1f6482364c6500480`, 38,849,664 bytes; `codespace-codex-runtime` (`--features devguard`, release) `a1aebb98919a4a8c67783b0590f1016d6472095a0f72bda01645e3ca03efddaa`, 26,867,360 bytes. For the fixture suites, `fixture_authority` `ea234acf2a3ddf645d4e5a3ad8ab163cfa81df003d323070ac754c035b3ff124` and `devguard-launch` `85c2165e95e19ca52e8a6c213b3efa4f48184de335a828b5409b737dce0e5bf0`, both built from the client pin; for the load, `devguard-qualify` `694c838621b14762dc2ad4af5139a48041764fe44fd62bed005a501d6eb68cc1` |
| DevGuard release | `0.1.0-6e7e065-94d85365`, built from `6e7e065`; `MANIFEST.json` SHA-256 `4535b52cc6466e892ebee2a2ad52f396e6b7520d7560e7e25c20ffa3647ed0df`; the installed `devguardd`, `devguard` and `devguard-launch` match it, and the service is healthy and runs it |
| Policy | configuration fingerprint `8cbb019bbb58e92e2a0d65f797cd5eb5f1696ce9e254bfdb1af97395382efc6f`; `devguard doctor --require admission,registration,macos-cooperative` satisfied; capabilities `durable_admission`, `fenced_launch`, `per_resource_evidence`, `static_control_reservations` and `macos_cooperative`. The `codespace` consumer has the role `control_service`, generation `codespace-g1`, one instance and a control reservation of 150 mCPU, 536,870,912 bytes and 40 tasks. The SHA-256 of `host.toml` is in the sealed record |
| DG-1 qualification | DevGuard's record of the release, SHA-256 `fd4fed318e0fbc640d9e6174a8dd731db80f9607d2235bdfa773c4101f6b17a4`: cold and warm loads qualified on 2026-10-13 under the same `host.toml`, fingerprint and capabilities, and promoted ([DevGuard #34](https://github.com/novelKR/DevGuard/pull/34)) |
| Toolchain | `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`, Python 3.9.6, Xcode 27.0 (27A266a) |
| Host | a Mac mini (Macmini9,1, Apple M1) with 8 logical CPUs and 16 GiB, on AC power, macOS 27.0 (26A428); work capacity after the consumer's reservation 5,350 mCPU, 12,079,595,520 bytes and 104 tasks |

<a id="report-runs"></a>

## Runs

Every run used the candidate's binaries in the minimal environment, with Rust 1.95.0 first in `PATH`, and started after checking that nothing was charged, no `codespace` instance was active and memory pressure was normal. Each run was sealed, and `verify` recomputed its verdicts from the raw files and found them unchanged.

| Run | When (UTC) | Result |
| --- | --- | --- |
| `candidate` | 2026-10-13 16:23 | passed |
| `matrix` | 2026-10-13 16:40–16:42 | passed: 45 of 45 scenarios |
| `faults` | 2026-10-13 16:42–16:45 | passed: 27 of 27 cases |
| `authority` | 2026-10-13 16:45–16:49 | passed: 10 of 10 phases |
| `control --rehearsal` | 2026-10-13 16:50–16:53 | inconclusive only because a rehearsal is shorter than the protocol, as designed |
| regression stages, `all` | 2026-10-13 17:00–17:53 | every stage passed except `linux-isolation`, which needs Linux |
| regression stages, `macos-core` and `macos-fork-race` | 2026-10-13 17:57–18:01 | passed |
| `control --rehearsal` | ⟨CTRL-REH-WHEN⟩ | ⟨CTRL-REH-RESULT⟩ |
| `control` | ⟨CTRL-WHEN⟩ | ⟨CTRL-RESULT⟩ |
| Linux CI | 2026-10-13 23:25–23:42 | passed: every job of the full profile ([run 38484155990](https://github.com/novelKR/CodeSpace/actions/runs/38484155990)) |

<a id="report-matrix"></a>

## Functional matrix

`matrix` ran against the installed service in both runner modes: 22 scenarios in `in-process` and 23 in `uds`, all passed.

| Scenario | `in-process` | `uds` |
| --- | --- | --- |
| registration | passed | passed |
| exits, io, termination, timeout, exec-failed, shortage, unsupported-minimum, mixed | passed on a pipe and on a PTY | passed on a pipe and on a PTY |
| no-helper, unusable-helper, status-mode, authority-unavailable | passed | passed |
| runner-socket | | passed |
| settled | passed | passed |

Every governed launch matched DevGuard's journal: the reserved and applied quantities equal the workspace's request, CPU, memory and tasks were each `applied`, the scope root was the launched process, and the attempt was released `scope_terminated` holding nothing, with its tracking complete. Every refused call's attempt was in the expected phase holding nothing, or did not exist where the refusal comes first.

<a id="report-faults"></a>

## Fault cases

`faults` ran against DevGuard's fixture authority, a fresh one for each case, in both runner modes: 13 cases in `in-process` and 14 in `uds`, all passed.

| Case | `in-process` | `uds` |
| --- | --- | --- |
| descendant, escape, stalled-observer | passed on a pipe and on a PTY | passed on a pipe and on a PTY |
| authority-lost, authority-restart, authority-stalled, helper-ends, credential, terminate-race, shutdown | passed | passed |
| worker-killed | | passed |

The protocol's [fault cases](cs-rg-qualification.md#fault-cases) say what each one shows. Together they show no replay and no premature release: a descendant keeps its attempt charged until it ends, an escaped or unobserved descendant leaves it charged as Suspect, an uncertain or restarted attempt is never launched again, and a call that provably started nothing leaves nothing charged.

<a id="report-authority"></a>

## Authority failure

`authority` ran its five phases in both runner modes against the fixture authority, and all ten passed. Process status, the I/O echo and the termination of governed targets that were already running worked in every phase. New governed executions were admitted while the authority could answer and refused while it could not.

| Phase | New governed executions | Termination targets admitted after (`in-process` / `uds`) |
| --- | --- | --- |
| baseline | admitted | 0.09 s / 0.09 s |
| paused | admitted | 0.09 s / 0.09 s |
| stalled | refused | 0.09 s / 0.10 s, before the hold |
| restarted | admitted | 36.61 s after 36 refusals / 34.61 s after 34 |
| stopped | refused | 0.12 s / 0.16 s, before the kill |

The host sample that went stale while `stalled` held the lock kept new work closed until DevGuard's samples had been clear for 30 seconds, so the targets of `restarted` were refused `RESOURCE_UNAVAILABLE` and tried again every second, within the protocol's 90 seconds.

<a id="report-control"></a>

## Control-path availability

⟨CTRL-SECTION⟩

<a id="report-accounting"></a>

## Resource accounting

The work order's accounting items, each checked against DevGuard's record rather than CodeSpace's view:

| Item | Where it is shown | Result |
| --- | --- | --- |
| Requested, reserved, supported and applied values | [matrix](#report-matrix): DevGuard's journal record of every governed launch | matched in both runner modes, pipe and PTY |
| Headroom and control reservations | the consumer's reservation is subtracted from the work capacity: 5,500 to 5,350 mCPU, 12,616,466,432 to 12,079,595,520 bytes and 144 to 104 tasks when it was provisioned; matrix `shortage` refuses a request that cannot fit; the [control path](#report-control) compares the execution owner's footprint with the reservation | ⟨ACCT-FOOTPRINT⟩ |
| Pressure | [authority failure](#report-authority): after a stale host sample, new work stays closed until samples have been clear for 30 seconds, while running targets stay controllable; the [control path](#report-control): host pressure under DG-1's load; the service's own pressure behavior is DG-1's | ⟨ACCT-PRESSURE⟩ |
| No release on root exit alone while a descendant remains | faults `descendant`, pipe and PTY | passed |
| Conservative accounting for an unknown state | faults `escape`, `stalled-observer` and `authority-restart`: the attempt stays charged as Suspect | passed |
| No replay after an uncertain launch | faults `authority-stalled`, `authority-restart` (one launch) and `worker-killed`; the control path's duplicate-launch count | passed; ⟨ACCT-DUPLICATES⟩ |
| Cleanup after a definite no-start | matrix `no-helper` and `unusable-helper` (cancelled), `shortage` (denied), `exec-failed` (released when the scope ended); faults `helper-ends` (released as `no_helper_created`), `authority-stalled` and `worker-killed` (nothing held) | passed |

<a id="report-regressions"></a>

## Regression and parity

The regression stages ran on the candidate on the qualification host, in the minimal environment, with the DevGuard test binaries required:

| Stage | Seconds | Result |
| --- | ---: | --- |
| `pin` | 0.67 | passed |
| `policy` | 0.13 | passed |
| `python` | 8.91 | passed |
| `format` | 0.93 | passed |
| `clippy-root` | 192.45 | passed |
| `clippy-adapters` | 235.36 | passed |
| `clippy-codex` | 149.78 | passed |
| `integration` | 2,196.63 | passed |
| `dependencies` | 5.19 | passed |
| `unit-patch` | 21.76 | passed |
| `unit-codex-runtime` | 31.37 | passed |
| `unit-pty` | 7.94 | passed |
| `unit-file-system` | 307.08 | passed |
| `unit-linux-sandbox` | 14.34 | passed |
| `unit-devguard` | 11.32 | passed |
| `unit-linux-sandbox-protocol` | 4.72 | passed |
| `linux-isolation` | | not run on macOS; Linux CI |
| `macos-core` | 75.28 | passed |
| `macos-fork-race` | 129.40 | passed: 400 trials, none failed, 72,946 children each with an exit code, no new crash report |

`all` passed 782 tests, failed none and ignored 9; the macOS-only stages passed 189, failed none and ignored 9. The adapter's known intermittent failure ("another authority owns this state directory") did not occur. These stages hold the existing tests of every area the work order names: workspace authorization, approvals, pipe and PTY execution, stdin and output, resize, timeout, termination, shutdown, patch helpers, sandbox helpers, the UDS worker and feature-off behavior, which the `dependencies` stage and the builds without the `devguard` feature cover. `macos-core` also runs the gateway's tests against DevGuard's fixture authority and launch helper, which run only on macOS.

Linux CI ran the full profile on the candidate in the scheduled [run 38484155990](https://github.com/novelKR/CodeSpace/actions/runs/38484155990), from 23:25 to 23:42 UTC on 2026-10-13, and every job passed: the format and pin check, the policy scan, the three Clippy builds and the seven unit stages, `integration` and `linux-isolation` on Ubuntu, and `macos-core`, `macos-fork-race` and `dependencies` on a hosted macOS runner. On Linux, DevGuard has no native host evidence, so the owner cannot register: the adapter's unit tests check that its registration is `incompatible`, and the gateway's tests in `integration` check that a governed execution is refused `RESOURCE_AUTHORITY_UNAVAILABLE` and that nothing is started. That is the feature-verified refusal of Linux `required` [claimed](#report-claims) above.

<a id="report-amendments"></a>

## Amendments and corrections

- **The regression command (amendment).** The protocol named one command, `validate-upstream.py all macos-core macos-fork-race`. `all` replaces every other stage name and leaves out the macOS-only stages ([`scripts/validate-upstream.py`](../scripts/validate-upstream.py)), so that command runs neither `macos-core` nor `macos-fork-race`. Both ran in a second invocation on the same candidate, and the protocol now names the two invocations. This changes documentation only; no run was repeated or changed.
- **Corrections before the freeze.** The check of the harness ([#97](https://github.com/novelKR/CodeSpace/pull/97)) on the qualification host at `7a0a748` found three harness defects, fixed before it merged (`8ed8b08`): the expected status of a governed PTY child killed by SIGTERM, which is exit code 143 because the pinned Codex PTY reports a child that inherits its launch descriptors that way; the settled check, which read the active instances once instead of awaiting them; and the `authority` suite, which started the targets of `restarted` before DevGuard admitted work again after `stalled`. It also found a product defect: at gateway shutdown in `uds` mode the worker was killed outright, so a governed pipe execution outlived it, and its scope ended only when its command did, about 30 seconds later. [#98](https://github.com/novelKR/CodeSpace/pull/98) fixed it: the gateway now closes the worker's connection and waits for the worker, which ends its processes as it exits, and kills it only if it still runs 5 seconds later. The same case then ended both scopes at once. The candidate was frozen after #98, every qualification run is on it, and the failed and incomplete checks are kept with the checks that passed afterwards.

<a id="report-evidence"></a>

## Evidence

The evidence directories are on the qualification host, under the DevGuard checkout's `evidence/` directory. Each is sealed with a `MANIFEST.json` of every file's SHA-256, and its files are read-only. None holds a credential, a credential digest or a child's environment, and each was scanned for them. A harness run's directory checks out when its `MANIFEST.json` has the SHA-256 below and `qualify-devguard.py verify` accepts it. The regression directories hold `validate-upstream.py`'s report and logs, copied unchanged. The control runs' console and `verify` output are kept beside their directories.

| Directory | Run | `MANIFEST.json` SHA-256 |
| --- | --- | --- |
| `csrg-u6-candidate-2026-10-13-ca306ee` | candidate | `3e3587d1694e4108912c742b6825684ff95b00e658c6f78fbe63143e37aadc95` |
| `csrg-u6-qual-matrix-2026-10-13-ca306ee` | matrix | `faf673aa028f4d4c4646eb271e617395f2b753740eecb3501241936e2b2ad688` |
| `csrg-u6-qual-faults-2026-10-13-ca306ee` | faults | `b000a94a70c68ea74658e37e2da275fe48a1c7f8b74cd6c7e4bdbb2525ae5025` |
| `csrg-u6-qual-authority-2026-10-13-ca306ee` | authority | `c4a7fb26703939eb56627848affba8d4790e72c6337663533356ca5662b78256` |
| `csrg-u6-qual-control-rehearsal-2026-10-13-ca306ee` | control rehearsal | `2c9c7fc08f4204e8f9c1a921459b50adc0c2ee439219ec8232124d785f0c66b1` |
| `csrg-u6-qual-regressions-2026-10-13-ca306ee` | regression stages, `all` | `2825fb3ca091a5d0280376a3f4134e3d0050e0718cbc95409633fe259840e52e` |
| `csrg-u6-qual-regressions-macos-2026-10-13-ca306ee` | `macos-core` and `macos-fork-race` | `5d160894fbf4b63fd822da63bf137bd8ab6a1d7a1aba005c256f5ba3ff59c2bd` |
| `csrg-u6-control-rehearsal-2026-10-14-ca306ee` | control rehearsal | `ac79024e237ee496b64f524696228e171a7161ccfde7652e02b023c67c25039f` |
| `csrg-u6-control-2026-10-14-ca306ee` | control | `7706aac71c2f793dbc273b5c78f888dcc2af50269c0039d132b21c236b47e9be` |

The checks before the freeze are kept too:

| Directory | Commit | Result | `MANIFEST.json` SHA-256 |
| --- | --- | --- | --- |
| `csrg-u6-harness-check-candidate-2026-10-13` | `7a0a748` | passed | `6b1cc6a10df9f3ca2b80472b5c0d9090a7e1a3a3517a99c1124d10b08a4d9400` |
| `csrg-u6-harness-check-matrix-2026-10-13` | `7a0a748` | failed: the two harness defects | `bf20056a7351b155f2e73388dbb22cd34d3a54721db0ed0b420842b3a2681f4a` |
| `csrg-u6-harness-check-faults-2026-10-13` | `7a0a748` | failed: 26 of 27, `uds` shutdown | `e1568fcb5ccb298e2eb581e6687c0947ccf40c8dee1960bcedddb57473792c1a` |
| `csrg-u6-harness-check-authority-2026-10-13` | `7a0a748` | incomplete: `restarted` | `42f8ef5f6b20665120e059add4fd1d7fa3f4ad6f98c8a5b8588fe35f5def1290` |
| `csrg-u6-harness-check-control-2026-10-13` | `7a0a748` | rehearsal, inconclusive as designed | `2a8507da10ac571259e4a7f3437102dd24a63cf7d129d9a30492c80f30940681` |
| `csrg-u6-harness-check-matrix-2026-10-13-8ed8b08` | `8ed8b08` | passed | `64198825c697e333aaab14469224f297b59852067192afd4614c8e0c77302aa2` |
| `csrg-u6-harness-check-authority-2026-10-13-8ed8b08` | `8ed8b08` | passed | `ffe9332ace437ff926ea2b787daf94ed58a8a691f00e23374fbefaa7797372e4` |
| `csrg-u6-harness-check-faults-2026-10-13-b74e5c2` | `b74e5c2` | passed | `6057f26fb81a59f4f8ef6f8c12ff95235ae25c10b7503c7a6f2aaa5264ddae8b` |
| `csrg-u6-harness-check-matrix-2026-10-13-b74e5c2` | `b74e5c2` | passed | `577962c15c89e70b4debfe93c2f7513876fa592c6d15ff86bf7535a585121a76` |
| `csrg-u6-harness-check-authority-2026-10-13-b74e5c2` | `b74e5c2` | passed | `dad385b738361ef883928329f3ed6879f9a874ebdc32f3bdd5aed464ba6dc679` |

DG-1's measurement of the release, which the policy row relies on, is sealed beside them and recorded in DevGuard.

<a id="report-outside"></a>

## Outside CS-RG

CS-RG ends here. These stay outside it, and nothing here claims them:

- **P1-RECOVERY**, the next milestone on the critical path: gateway restart and reconnection while an independent runner keeps its processes. It starts only with the owner's separate authorization.
- **DG-LINUX**, enforcement on Linux. Until it qualifies, `required` is refused on Linux, and `off` is the only Linux path.
- **DG-CACHE** and **DG-ADAPTERS**, DevGuard's cache and further adapters.
- **Codex convergence.** Governed PTY executions are observed before the reap through CodeSpace's [recorded Codex patch](upstream-lock.md#codex-downstream-patch) until upstream offers the hook ([openai/codex#53298](https://github.com/openai/codex/issues/53298)). The next Codex pin move is its own change and a new candidate.
- **Another combination.** A different DevGuard client pin, release, policy or host, a code change, or another toolchain is a new candidate that needs its own runs.
- **What the runs did not show:** DG-1's warm load for the control path, foreground browser responsiveness, a replayed message on the runner wire, an injected journal refusal, and protection for arbitrary request or file sizes.
