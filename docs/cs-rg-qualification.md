<a id="cs-rg-qualification"></a>

# CS-RG qualification protocol

[English](cs-rg-qualification.md) | [한국어](ko/cs-rg-qualification.md)

This page is the protocol for qualifying CodeSpace's use of DevGuard (CS-RG): which combination is qualified, which modes are claimed, how each claim is tested and measured, what makes a run pass, and what evidence a run leaves. It is frozen together with its harness, [`scripts/qualify-devguard.py`](../scripts/qualify-devguard.py), before any qualification run. A change to it, or to the harness, after the first qualification run is a new protocol: it is reviewed like any other change, and the runs it affects are repeated. The [integration roadmap](devguard-integration.md) describes what each unit built; this page decides whether the result qualifies.

**Status:** the protocol and the harness exist; no qualification run has been made, and nothing here is a qualification result. CS-RG stays in progress and not qualified until a run under this protocol passes and its evidence is reviewed.

<a id="candidate"></a>

## The candidate

A run qualifies one exact combination, recorded by the harness's `candidate` suite in `candidate.json`, never by hand:

| Part | Recorded as |
| --- | --- |
| CodeSpace | the commit, whether tracked files outside the Codex submodule differ from it, and the SHA-256 of its tree without `docs/`, `docs-site/` and `*.md` files (the Codex gitlink included) |
| DevGuard client | the `devguard-*` entries of `Cargo.lock` ([client pin](upstream-lock.md#devguard-client-pin)) |
| Codex | the gitlink and the result of `scripts/codex_patch.py check` ([recorded patch](upstream-lock.md#codex-downstream-patch)) |
| Binaries | SHA-256 and size of `codespace-mcp` (built with `-p codespace-server --features devguard`) and the worker `codespace-codex-runtime` (`--features devguard`), and of the fixture, helper and `devguard-qualify` binaries when given |
| DevGuard release | the installed release ID, the SHA-256 of its `MANIFEST.json`, whether each installed binary matches it, and the service's status |
| Policy | the SHA-256 of `host.toml`, `devguard doctor --require admission,registration,macos-cooperative` (configuration fingerprint, capabilities, protocol), and the `codespace` consumer's role, generation, instance limit and control reservation. The consumer's credential digest is never read into the record |
| DG-1 qualification | DevGuard's qualification record of the release, when it exists: its SHA-256, its verdicts, and whether it was measured under the same `host.toml`, fingerprint and capabilities |
| Toolchain and host | `rustc -vV`, `cargo -V`, Python, Xcode; macOS version, model, logical CPUs, memory, boot time; the service's work capacity and pressure state |

`candidate` passes when the checkout has no uncommitted changes outside the Codex submodule, the Codex check passes (the submodule holds the pinned commit with exactly the recorded patch), the service runs the release, every installed binary matches the manifest, `doctor` is satisfied, `host.toml` has a `codespace` consumer with the `control_service` role, and Rust 1.95.0 is first in `PATH`. A final report also needs a DG-1 qualification of the same release under the same policy; `candidate` records it without requiring it, so that the harness can run before it exists.

Any change to code, binaries, the release, the policy, Codex or the toolchain is a new candidate and repeats the runs it affects. A commit that changes only documentation keeps the candidate, and the code-tree hash shows it.

<a id="supported-modes"></a>

## Supported modes

Each claimed cell is tested on its own; no cell is inferred from another.

| Platform | Runner | I/O | Participation | Claim | Shown by |
| --- | --- | --- | --- | --- | --- |
| macOS, installed service with native host evidence | `in-process` | pipe, PTY | `required` | qualify | the functional matrix, the fault cases and the control-path measurement |
| macOS | `uds` with `--runtime-bin` (the worker owns executions) | pipe, PTY | `required` | qualify | the same, with the worker registered |
| macOS | `uds` with `--runner-socket` | pipe, PTY | `required` | unsupported | refused `RESOURCE_POLICY_UNSUPPORTED` before an attempt exists |
| macOS | any | pipe, PTY | `required` under `--devguard status` | unsupported | refused `RESOURCE_POLICY_UNSUPPORTED` before an attempt exists |
| macOS | supported | pipe, PTY | `required` without a usable launch helper | unsupported | admitted, then refused `MANAGED_LAUNCH_UNAVAILABLE` with the attempt cancelled |
| macOS | supported | pipe, PTY | `off` and `required` workspaces in one gateway | qualify | concurrent governed and ungoverned children, each holding only its standard descriptors |
| macOS, Linux | all | pipe, PTY | `off` | existing behavior | the [regression stages](#regressions), with and without the `devguard` feature |
| Linux | all | pipe, PTY | `required` | unsupported until DG-LINUX | refused before anything runs: `RESOURCE_AUTHORITY_UNAVAILABLE` under `--devguard register`, since the owner cannot register without native host evidence, otherwise `RESOURCE_POLICY_UNSUPPORTED`; shown by Linux CI |

A gateway built without the `devguard` feature has no DevGuard flags; `required` workspaces there are refused as with `--devguard status`.

<a id="running"></a>

## Running the harness

The harness runs on the qualification host, a Mac with the release installed and the `codespace` consumer provisioned, with the system Python (3.9 or later) and no third-party packages. Every process it starts gets a minimal environment (`HOME`, `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, `TMPDIR`, `LANG=C` and the `CODESPACE_*` settings it names), and the harness itself is started the same way:

```bash
env -i HOME="$HOME" TMPDIR="$TMPDIR" PATH="<Rust 1.95.0>/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  /usr/bin/python3 scripts/qualify-devguard.py matrix --out evidence/<new directory> \
  --mcp-bin <codespace-mcp> --runtime-bin <codespace-codex-runtime> --release <release ID>
```

| Suite | Against | Extra arguments |
| --- | --- | --- |
| `candidate` | the installed service | `--release`; optionally `--fixture-bin`, `--fixture-helper`, `--qualify-bin` to record them |
| `matrix` | the installed service | `--release` |
| `faults` | DevGuard's fixture authority | `--fixture-bin` (`crates/devguard` example `fixture_authority`), `--fixture-helper` (`devguard-launch` built from the client pin) |
| `authority` | DevGuard's fixture authority | as `faults` |
| `control` | the installed service under DG-1's load | `--release`, `--work` (a new directory), `--qualify-bin` (DevGuard's `devguard-qualify`), `--devguard-checkout` (a DevGuard checkout holding the release's source commit), `--load cold` or `warm`, `--rehearsal`, `--repetitions` |
| `verify` | a sealed evidence directory | the directory |

`--modes` limits a suite to `in-process` or `uds`; both run by default. The installed-service suites default to the service's socket, journal, log, `host.toml` and the consumer's credential file, whose path is handed to the gateway and never read by the harness. Exit status: 0 passed or qualified, 1 failed, 2 incomplete or inconclusive, 3 the harness failed, 130 interrupted; `verify` exits 0 when the run checks out and 1 when it does not.

The harness talks to `codespace-mcp` over standard input and output as any MCP client does (protocol `2025-11-25`), with its own small client, so a timing is the tool call's local round trip from the harness and includes no network. It reads DevGuard's journal read-only, as DevGuard's own measurement does, and takes a governed launch's attempt ID and scope root from the gateway's log.

<a id="functional-matrix"></a>

## Functional matrix

`matrix` runs against the installed service, once per runner mode, after checking that the service runs the release and nothing is charged. Governed workspaces request 50 mCPU, 64 MiB and 8 tasks; the gateway's process timeout is 15 seconds. It never runs a command that leaves a descendant, escapes its group or restarts the service, so that no attempt can stay Suspect on the owner's service.

| Scenario | Pipe and PTY | Passes when |
| --- | --- | --- |
| registration | | `workspace_info` reports participation `registration`, the authority available, the owner (`in_process` or `worker`) registered, governing executions, and DevGuard's one active `codespace` instance is that process |
| exits | each | exit 0 and exit 3 are reported as exited with their codes; a SIGTERM death is `signaled` with signal 15 on a pipe, and exited with code 143 (128 + 15) on a PTY, as the pinned Codex PTY reports a governed child, which inherits its launch descriptors ([process status](agent-integration.md)) |
| io | each | on a pipe, `write_stdin` reaches `cat` and its output returns, `process_resize` is refused `PROCESS_NOT_TTY` and termination is reported; on a PTY, the terminal starts at 24×80, a resize to 40×100 is answered and seen by the process, and stdin reaches it |
| termination | each | `terminate_process` answers, the process is reported terminated and its scope's process group is gone |
| timeout | each | a `sleep 600` is reported `timeout` |
| exec-failed | each | a script whose interpreter is missing or not executable is refused `PROCESS_SPAWN_FAILED` with errno 2 or 13; the helper claimed the grant and DevGuard released it when the scope ended |
| shortage | each | three calls to a workspace asking for more CPU than the work capacity are each refused `RESOURCE_UNAVAILABLE` as new, denied attempts holding nothing, never `WORKSPACE_BUSY` |
| unsupported-minimum | each | a workspace whose minimum asks the kernel to enforce tasks is refused `RESOURCE_POLICY_UNSUPPORTED`, denied by DevGuard |
| mixed | each | an ungoverned and a governed execution run at once in one gateway, and each child holds only descriptors 0, 1 and 2 |
| no-helper, unusable-helper | | a gateway without a launch helper, or with one that does not exist, refuses `MANAGED_LAUNCH_UNAVAILABLE`, DevGuard records the attempt cancelled, and `off` workspaces still run |
| status-mode | | under `--devguard status` a governed execution is refused `RESOURCE_POLICY_UNSUPPORTED` before an attempt exists; `off` still runs |
| authority-unavailable | | with no authority at the socket, it is refused `RESOURCE_AUTHORITY_UNAVAILABLE`, DevGuard holds no attempt for it, and `off` still runs |
| runner-socket | `uds` only | with a worker the gateway did not start, governed executions are refused `RESOURCE_POLICY_UNSUPPORTED` before an attempt exists |
| settled | | nothing of the consumer stays charged and no instance stays active, both awaited for up to 60 seconds because DevGuard retires an instance once it sees that its owner is gone; otherwise the matrix stops |

Every governed launch is then checked against DevGuard's own record of its attempt ([accounting](#accounting)).

<a id="fault-cases"></a>

## Fault cases

`faults` injects the faults that would leave the installed service with Suspect or charged work, so it runs them against DevGuard's fixture authority: DevGuard's daemon code at CodeSpace's client pin, in a private state directory, provisioning its own `codespace` consumer. Each case gets a fresh fixture authority; governed workspaces request 50 mCPU, 32 MiB and 4 tasks.

| Case | Pipe and PTY | Passes when |
| --- | --- | --- |
| descendant | each | a root that exits leaving a descendant keeps its attempt charged, not Suspect, while the descendant runs, and the attempt is released once it ends. On a PTY the reconciler is paused around the exit, so only the observation before the reap can adopt the descendant |
| escape | each | a descendant that leaves the scope's group with `setsid` keeps the attempt charged as Suspect after everything has ended |
| stalled-observer | each | with the reconciler paused and the authority held past the observation's bound, CodeSpace reaps the root before the hold ends, and the unproven descendant leaves the attempt Suspect, also after it ends |
| authority-lost | | with the authority killed while a pipe and a PTY execution run, both still answer `running` and are terminated within 4 seconds, a new governed execution is refused `RESOURCE_AUTHORITY_UNAVAILABLE` with nothing at DevGuard, and `off` still runs |
| authority-restart | | with the authority restarted while an approved, resumed execution runs, the process stays CodeSpace's and runs to its end, its attempt stays charged as Suspect, resuming the approval again returns the same process with one launch, and a new execution is admitted and released |
| authority-stalled | | with the authority's lock held past the owner's deadlines, the call is refused `ADMISSION_UNKNOWN` or `RESOURCE_AUTHORITY_UNAVAILABLE` before the hold ends, the command never runs, DevGuard never hands the attempt to a helper and nothing of it stays charged, and the next execution is a new attempt that runs |
| helper-ends | | a helper that exits before READY makes the call refused `PROCESS_SPAWN_FAILED` (ended before it reported ready), the command never runs, and DevGuard releases the grant as never started (`no_helper_created`) |
| credential | | with a credential the authority refuses and with a missing one, governed executions are refused `RESOURCE_AUTHORITY_UNAVAILABLE` naming `credential_refused` or `credential_unavailable`, and `off` still runs |
| terminate-race | | twelve terminations around the process's own exit, alternating pipe and PTY, each end once as exited or terminated, are launched once and released |
| shutdown | | when the gateway's input closes with a governed pipe and PTY execution running (in `uds` mode the worker goes with it), the gateway and the owner exit, both scopes end and are released, and a new gateway registers and runs without launching either again |
| worker-killed | `uds` only | with the worker killed while it prepares a governed execution, the gateway answers the call, the command never runs, DevGuard never hands the attempt to a helper and holds nothing of it, and the gateway keeps running |

<a id="fault-coverage"></a>

### Coverage of the order's fault list

| Fault | Where it is shown |
| --- | --- |
| Admission refusal | matrix `shortage` and `unsupported-minimum` (installed service, both modes, pipe and PTY) |
| Authority gone before admission | matrix `authority-unavailable`; faults `credential` |
| Authority gone after start | faults `authority-lost`, `authority-restart`; `authority` suite |
| Lost launch answer | faults `authority-stalled` for the request a session is waiting on. A lost answer to a request already past registration cannot be produced from outside the owner; DevGuard's client tests and CodeSpace's runner and gateway tests with a scripted worker cover it |
| Helper failure | matrix `no-helper`, `unusable-helper`; faults `helper-ends` |
| READY, then the program cannot run | matrix `exec-failed` (errno 2 and 13, pipe and PTY) |
| Observer timeout | faults `stalled-observer` |
| Surviving descendant | faults `descendant`, `escape` |
| Terminate and exit race | faults `terminate-race` |
| Shutdown and exit race | faults `shutdown` |
| Runner connection lost | faults `worker-killed` while preparing; a worker lost during the launch itself is covered by the gateway tests with a scripted worker |
| Stale or duplicate request | faults `authority-restart` (resume again returns the same process, one launch) and the control measurement's duplicate-launch count. A replay of a preparation or launch message on the runner wire is not tested; this is a known gap |
| Uncertain execution | faults `authority-stalled`; the control measurement fails when an uncertain attempt is admitted again |
| Credential failure | faults `credential` |
| Journal or storage refusal | not injected: the fixture authority cannot make its journal refuse. DevGuard's own suites cover the refusal, and CodeSpace's tests cover how its codes map |

The gateway tests that use the fixture authority run only on macOS with the DevGuard test binaries, in the [regression stages](#regressions) on the qualification host. macOS CI runs the adapter's tests, the runner's DevGuard tests and the gateway's `devguard` module tests, but not those gateway tests.

<a id="control-path"></a>

## Control-path availability

The targets are DevGuard's established ones that apply to the control path ([SLO protocol](https://github.com/novelKR/DevGuard/blob/ea98d7fadf6ac10e9393db923105bf31f719952a/docs/planning/verification.md#slo-protocol)), not redefined here: local `process_status` p99 at most 500 ms; termination acknowledgement p99 at most 1 second, with the scope's actual end reported separately; no MCP connection lost; no execution launched twice; no uncertain execution started again. The foreground browser targets are DG-1's and are not measured here. Rejection latency of a new governed execution and output responsiveness have no established target; they are measured and reported, with the design's 250 ms preparation budget as the reference.

<a id="control-intervals"></a>

### Intervals and samples

`control` measures each runner mode (`in-process` and `uds`) in three repetitions. A repetition is a 10-minute idle interval followed by a load interval of at least 30 minutes. Each interval starts a new gateway registered with the installed service, whose governed workspaces request 25 mCPU, 16 MiB and 2 tasks each, and samples on fixed schedules:

| Schedule | Every | What is timed |
| --- | --- | --- |
| status | 1 s, two targets | `process_status` of a running governed `cat` (pipe) and `sleep` (PTY) |
| termination | 20 s | a fresh governed `sleep`, alternating pipe and PTY: `terminate_process` to its answer; separately, to the exited status, to the scope's process group gone, and to DevGuard's journal showing the attempt released |
| I/O | 5 s | a numbered line written with `write_stdin` to the `cat` until `read_process` returns it |
| refusal | 30 s | a governed execution in a workspace asking for more CPU than the work capacity, which must be refused |
| host | 5 s | memory pressure level, swap and load average |
| service | 60 s | whether the service still runs the release, and its process ID |
| footprint | 5 s | the execution owner's RSS, threads, processes and CPU (and the gateway's in `uds` mode), compared with the consumer's control reservation and reported |

A slot whose moment passes while the previous sample still runs is recorded as missed, never taken late. A missed slot, an error, or an answer about a target that no longer runs is a missing sample, and a missing sample ranks above every value in the nearest-rank percentile, as in DG-1. A termination target refused for capacity or pressure (`RESOURCE_UNAVAILABLE`) is not a sample. At the end of the interval both status targets are terminated, and each termination must be effective.

<a id="control-load"></a>

### Load

The load is DG-1's: the six consumers of DevGuard's `scripts/measure.py`, from the given DevGuard checkout, run through the installed release's `devguard exec` with `devguard-qualify` and a Cargo build of the release's source, so that pressure and queueing are the service's own. `--load cold` (the default) builds from empty target directories; `--load warm` builds a target directory first and is inconclusive if that build fails. A qualification run uses the cold condition; a warm run is an additional condition, reported separately when it is made. The consumers run until the interval's planned end and then finish their current runs within DG-1's completion limit. The harness's targets and the load fit in half the work capacity, the capacity DevGuard keeps under memory warning.

<a id="control-verdicts"></a>

### Verdicts

An interval is **inconclusive** (an invalid observation, never a pass) when other work was charged as it began, the journal could not be read, a status target could not be started, raw evidence could not be written, the service could not be observed or had another release selected, or the load did not finish within its limit.

Otherwise it **fails** when the status p99 exceeds 500 ms; the acknowledgement p99 exceeds 1 second; a termination was acknowledged without ending its target; a terminated target was not released; an I/O line never came back; any sample lost the MCP connection; a status target's final termination was not effective; the measured release restarted or became unhealthy; a load run is uncertain or a load attempt stays charged; or a harness attempt stays charged, an execution was launched more than once, or an uncertain attempt was admitted again.

It is also **inconclusive** when too little was observed: fewer status samples than 90% of the schedule, fewer termination samples than 80%, or a load that was not DG-1's (a consumer with no successful run, one that ran for less than a quarter of the interval, one with failed runs, or admitted load running for less than half of it). Otherwise it **passes**.

A repetition passes when both intervals pass and the service still runs the release with the same process afterwards; a failing idle interval makes it inconclusive, never a failure of the load. A runner mode qualifies when all three repetitions pass, and fails when any fails; values are never pooled across repetitions. The run qualifies only under the full protocol (both modes, three repetitions, 10 and 30 minutes) with both modes qualified, and fails when either fails. `--rehearsal` (30 seconds idle, 60 seconds load, one repetition) tries the run and never qualifies.

<a id="authority-failure"></a>

### Authority failure

`authority` measures process control while the fixture authority is unhealthy, in five 15-second phases per runner mode, against the same running status targets: `baseline`; `paused` (the reconciler paused); `stalled` (its lock held for the whole phase); `restarted`; `stopped` (its process killed). Status is sampled every second, the I/O echo every 3 seconds, three governed targets started before the fault are terminated at 3, 7 and 11 seconds, and a new governed execution is attempted at 1, 5, 9 and 13 seconds. The targets are started once DevGuard admits them: a host sample that went stale while the authority was held keeps new work closed until its samples have been clear for 30 seconds, so after `stalled` a target refused `RESOURCE_UNAVAILABLE` is tried again every second for up to 90 seconds, and the phase records how long that took. Each phase is judged by the interval rules above, and new executions must be admitted while the authority can answer (`baseline`, `paused`, `restarted`) and refused `RESOURCE_AUTHORITY_UNAVAILABLE` or `ADMISSION_UNKNOWN` while it cannot (`stalled`, `stopped`). After a restart or once stopped, DevGuard cannot release targets started before the fault: an unreleased target is reported there, not failed.

<a id="accounting"></a>

## Resource accounting

The harness checks DevGuard's record, never CodeSpace's own view. For each governed launch in the matrix it reads the attempt the gateway logged from the journal: the reserved and applied quantities equal the workspace's request; CPU, memory and tasks are each `applied`; the scope root is the process the gateway launched; and once the scope ends the attempt is released with `scope_terminated`, holds nothing and its tracking stayed complete. A refused call names its attempt, which DevGuard records in the expected phase (`denied`, `cancelled` or `released`) holding nothing, or no attempt exists where the refusal comes first. Under the control load, the measured footprint of the gateway and the worker is compared with the `codespace` reservation and reported; the reservation is DevGuard's accounting estimate, so the comparison is not a verdict.

<a id="regressions"></a>

## Regression and parity

On the candidate, on the qualification host in the minimal environment, `python3 scripts/validate-upstream.py all macos-core macos-fork-race` runs every stage, with the DevGuard test binaries required in the stages that use them; Linux is shown by CI on the candidate head, whose `dependencies` stage checks the dependency graph with the `devguard` feature on and off. A regression in any stage fails the qualification.

<a id="evidence"></a>

## Evidence

Every suite but `verify` writes a new directory and seals it: `report.json` (the suite, its arguments, the harness's file hashes and commit, the verdict, and what was settled at the end), the suite's records, and the raw samples as JSON lines, including each gateway's MCP transcript and log. Sealing records every file's SHA-256 in `MANIFEST.json` and makes the files read-only; failed, inconclusive and interrupted runs are sealed and kept too. Nothing in a directory holds a credential, a credential digest or a child's environment.

`qualify-devguard.py verify <directory>` checks the manifest and recomputes every verdict from the raw files: scenario verdicts from their checks; each authority phase and each control interval from its samples; then each repetition, runner mode and the run. It reports whether the harness that ran is the one verifying, and writes nothing.

<a id="legacy-off-path"></a>

## Legacy `off` path decision

This is the decision record that the work order requires for the existing backends (CSRG-C09).

**Decision:** keep participation `off` as the compatibility path and default, unchanged.

- **Scope.** Participation `off`, the default and what a missing setting means, on every platform and runner mode, pipe and PTY. Its executions are the request's own command: pipe children have CodeSpace's single reaper, which governed executions share; PTY children use the pinned Codex PTY spawn without the observation window before the reap.
- **Why.** The work order does not migrate code for neatness alone. `off` is the product default; `required` is an operator's explicit choice, unsupported on Linux until DG-LINUX, so `off` is the only path there. Making governed launching the only path would remove every Linux execution and every setup without DevGuard.
- **What is duplicated.** One spawn path serves both and chooses the program: the request's command, or DevGuard's launch helper with its permit and report descriptors (`How::Direct` and `How::Managed` in [`crates/runner/src/process.rs`](../crates/runner/src/process.rs)). Reaping is not duplicated: a pipe child of either kind has CodeSpace's one reaper, and a PTY child the pinned Codex PTY's waiter. A PTY differs in one call, `spawn_inheriting` for ungoverned executions and `spawn_observed`, from the [recorded patch](upstream-patches.md), for governed ones. A governed PTY child also inherits its launch descriptors, so the pinned Codex PTY spawns it on its descriptor-keeping path, which reports a signal death as exit code 128 plus the signal where an ungoverned PTY reports 1 ([process status](agent-integration.md)). The governed path adds admission, launch and settlement ([`crates/runner/src/launch.rs`](../crates/runner/src/launch.rs)), which `off` never enters. Without the `devguard` feature no DevGuard package is in the build; with it, only DevGuard's client and contract crates are, as the `dependencies` stage checks.
- **What `off` does not provide.** No admission, reservation or accounting at DevGuard, no scope policing, and no observation of an ungoverned PTY root before its reap. Only `off` executions can use the Linux command sandbox; governed ones are refused where it is on.
- **Revisit.** When DG-LINUX qualifies Linux enforcement, when a Codex pin move changes PTY spawning, or when `required` becomes the default.
- **Removal.** Only with an independent product reason and regression evidence that every supported mode keeps its behavior without it.
