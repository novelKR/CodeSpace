<a id="recovery-qualification"></a>

# Gateway recovery qualification protocol

[English](recovery-qualification.md) | [한국어](ko/recovery-qualification.md)

This page is the protocol for qualifying the [independent Runner](runner-isolation.md#independent-runner), the opt-in mode of Gateway recovery (P1-RECOVERY): which combinations are claimed, how each claim is tested and measured, what makes a run pass, and what evidence a run leaves. It follows the [CS-RG qualification protocol](cs-rg-qualification.md), whose candidate, evidence and verification it reuses, and its harness is the same [`scripts/qualify-devguard.py`](../scripts/qualify-devguard.py). It is frozen together with the harness before any qualification run; a change to either after the first run is a new protocol, reviewed like any other change, and the runs it affects are repeated. The [integration roadmap](devguard-integration.md#resource-gateway-recovery) describes what each unit built; this page decides whether the result qualifies.

**Status:** frozen with its harness before the first qualification run. No combination is qualified yet, so the independent Runner is not a supported mode anywhere.

<a id="recovery-candidate"></a>

## The candidate

A run qualifies one exact combination, recorded by the harness's `candidate` suite as for CS-RG ([the candidate](cs-rg-qualification.md#candidate)): the CodeSpace commit and its code tree, the DevGuard client pin, the Codex pin with its recorded patch, the binaries, the installed DevGuard release and its policy, and the toolchain and host. The independent Runner is the same `codespace-codex-runtime` binary as the `uds` worker. The run uses the installed DevGuard release and the policy as they are: it changes neither, installs nothing and provisions nothing. Any change to code, binaries, the release, the policy, Codex or the toolchain is a new candidate and repeats the runs it affects; a commit that changes only documentation keeps it.

<a id="recovery-supported-modes"></a>

## Supported modes

Each claimed cell is tested on its own; no cell is inferred from another. The modes that CS-RG qualified keep their claims, and the run shows that they still hold ([regressions](#recovery-regressions)).

| Platform | Runner | I/O | Participation | Claim | Shown by |
| --- | --- | --- | --- | --- | --- |
| macOS, installed service with native host evidence | `uds` with `--runner-lifecycle independent`, an independent Runner the operator starts | pipe, PTY | `off` and `required` | qualify | the recovery suites with both participations and the gateway restart measurement |
| Linux | the same | pipe, PTY | `off` | CI functional tests only | CodeSpace's tests of the independent Runner in Linux CI; the harness's `off` scenarios also run on Linux. This is functional verification, never a Linux qualification |
| Linux | the same | pipe, PTY | `required` | unsupported until DG-LINUX | refused before anything runs, as every Linux `required` execution is |
| macOS, Linux | the same | pipe, PTY | restoring processes after the Runner itself restarts | unsupported | outside P1-RECOVERY's scope; `runner-replaced` shows that a gateway never takes a new Runner for the one it lost |

The independent mode is opt-in. Making it the default is a separate decision, outside this protocol.

<a id="recovery-running"></a>

## Running the harness

The harness runs on the qualification host as the CS-RG suites do ([running the harness](cs-rg-qualification.md#running)): the system Python, no third-party packages, and a minimal environment for itself and every process it starts. It starts each independent Runner itself, as an operator would: `codespace-codex-runtime <socket> --lifecycle independent` in its own session, with a private socket directory, its log kept as evidence, and for `required` the DevGuard settings of the authority it uses, the credential file given by its path, which the harness never reads. Nothing is installed as a LaunchAgent and no policy changes. Its gateways run with `--runner uds --runner-socket <socket> --runner-lifecycle independent`, an exit intent, an operations database and, for `required`, `--devguard register`. It takes a governed launch's attempt ID and scope root from the gateway's log as the gateway writes it, and writes that log to the evidence on a thread of its own, so a disk the load keeps busy delays the log file, never the attempt a sample waits for; a log it cannot write counts as raw evidence that could not be written.

| Suite | What it shows | Extra arguments |
| --- | --- | --- |
| `recovery-lifecycle` | the Runner across every way a gateway ends, timeouts and settings ([lifecycle](#recovery-lifecycle)) | `--participation`; on macOS with `required`, `--fixture-bin` and `--fixture-helper` as for `faults` |
| `recovery-store` | process records and the operations database's one writer ([store](#recovery-store)) | as `recovery-lifecycle` |
| `recovery-fence` | Claims, controller epochs and other processes at the socket ([fence](#recovery-fence)) | as `recovery-lifecycle` |
| `recovery-reconcile` | reconciliation after a restart, its races and an authority outage ([reconcile](#recovery-reconcile)) | as `recovery-lifecycle` |
| `recovery-observation` | output gaps, retention and a lost Runner ([observation](#recovery-observation)) | as `recovery-lifecycle` |
| `recovery` | every suite above, then the [gateway restart measurement](#recovery-measurement) | as above, and on macOS with `required` the `control` suite's `--release`, `--work`, `--qualify-bin`, `--devguard-checkout`, `--load`, `--rehearsal` and `--repetitions` |

`--participation` takes `off`, `required` or both, the default. `off` scenarios run on every platform without an authority. On macOS a `required` scenario runs against DevGuard's fixture authority, as the CS-RG fault cases do, each with a fresh one that provisions its own `codespace` consumer with one instance, as the installed policy does; its governed workspaces request 50 mCPU, 32 MiB and 4 tasks. On Linux `required` scenarios are recorded as not run, with the reason, and are not counted. A suite passes when every scenario that ran passed and at least one ran. Exit statuses are those of the CS-RG suites.

```bash
env -i HOME="$HOME" TMPDIR="$TMPDIR" PATH="<Rust 1.95.0>/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  /usr/bin/python3 scripts/qualify-devguard.py recovery --out evidence/<new directory> \
  --mcp-bin <codespace-mcp> --runtime-bin <codespace-codex-runtime> \
  --fixture-bin <fixture_authority> --fixture-helper <devguard-launch> \
  --release <release ID> --work <new directory> --qualify-bin <devguard-qualify> \
  --devguard-checkout <DevGuard checkout>
```

<a id="recovery-functional"></a>

## Functional suites

Each scenario gets its own work directory, operations database and Runner, and with `required` its own fixture authority; whatever a scenario leaves running is ended afterwards. A scenario passes only when each of its checks passed. Unless a scenario says otherwise, a gateway's process timeout is 10 minutes and its exit intent is `detach`. "Both" in the I/O column means that the scenario runs once with a pipe and once with a PTY, and the participation column lists the participations the scenario runs with. Right after a restart a gateway refuses new work with `RECOVERY_PENDING` in each workspace where work started, until it has reconciled it. A gateway that holds a workspace for a process reports the process exited once the Runner has reaped it, but frees the workspace only when the Runner's exit event follows, once the process's output has closed, and refuses new work there with `WORKSPACE_BUSY` until then. Where a scenario needs new work started, it sends it again while it meets one of these refusals, for up to 30 seconds; where its row expects one of these refusals, it checks it.

<a id="recovery-lifecycle"></a>

### Lifecycle

| Scenario | I/O | Participation | Passes when |
| --- | --- | --- | --- |
| exit-kinds | both | `off`, `required` | a process runs while its gateway ends in every way a gateway ends: its input closed and SIGTERM with the exit intent `detach`, SIGUSR1 with `stop` (a restart always detaches) and SIGKILL, each followed by the next gateway. The Runner and the process outlive each of them; each next gateway claims the Runner under the next epoch, finds the same process running and reads its output on from where the gateway before stopped. Then a gateway with `stop` whose input closes ends the process, and the Runner removes its socket and exits 0 |
| original-timeout | both | `off`, `required` | two processes reach their timeouts across gateway restarts, the first while no gateway runs and the second while a later gateway with a much longer process timeout serves: each ends at the deadline its record held when it started, never at one that a restart moved or lengthened, and is reported `timeout` |
| runner-sigterm | | `off`, `required` | the Runner's own SIGTERM, as an operator stops it, ends its pipe and PTY processes as a controller's Stop does; it exits 0 and removes its socket, its gateway reports neither process running and refuses new work, and with an authority both attempts are released and the instance retires |
| invalid-settings | | `off` | settings that do not fit together are refused before anything reaches the Runner: an exit intent without the independent lifecycle, the lifecycle without an exit intent, in process, with a worker to start, without a socket or without an operations database, a managed gateway at an independent Runner and an independent gateway at a managed worker. A Runner refuses a socket directory that is not private, a shared one, no socket, an unknown lifecycle, a credential handed to it and a socket where another Runner listens. The listening Runner is left as it was: it logged no Claim, and a gateway then claims it under epoch 1 and runs a command |

<a id="recovery-store"></a>

### Process records

| Scenario | I/O | Participation | Passes when |
| --- | --- | --- | --- |
| records | both | `off`, `required` | the operations database holds, of an execution sent to the Runner and after a gateway restart, the Runner's identity as its Hello states it, the controller epoch, the workspace, the argv's digest and never the argv, the root process of a pipe execution (a PTY's is not named), the original deadline, and how the process ended once a later gateway terminated it |
| writer-lock | | `off` | a second gateway started on the database while the first serves waits a bounded time and refuses to start, naming the holder, and never reaches the Runner; one started while the first ends waits for it and then serves, claiming the Runner after it |
| schema | | `off` | a database of a newer schema version, and a file that is not a database, are refused before anything reaches the Runner and left unchanged |
| records-command | | `off` | `codespace-mcp records list` lists the records that may still hold a workspace, and `records release` releases one only while no gateway runs, only for an execution that has not ended, and not while its Runner's PID runs a process |

<a id="recovery-fence"></a>

### Claim and fence

| Scenario | I/O | Participation | Passes when |
| --- | --- | --- | --- |
| foreign-connection | both | `off`, `required` | while a gateway controls the Runner, another process of the same user reads the Runner's Hello, its Claim is refused because a controller is connected, every request it sends that would change a process, under the controller's epoch or any other, is refused as stale, and its Detach changes nothing; the gateway goes on controlling the Runner and its processes |
| stale-epoch | | `off` | after two gateways detached in turn, a Claim for another operations database is refused; one for the Runner's own database, which any process of the same user can make while no gateway controls the Runner (the [exposure](runner-isolation.md#claim-exposure) that decision D6 accepts), is granted under the next epoch and logged with its PID, and that connection's requests under an earlier epoch or none are refused as stale. The next gateway claims the Runner under the epoch after it and finds the same process, its output on from where it was |
| stopped-gateway | | `off` | a controlling gateway stopped with SIGSTOP keeps its connection, so nothing pushes it out: another process's Claim is refused and a second gateway refuses to start for the database's writer. Once the stopped gateway is killed, the next one claims the Runner under epoch 2 and finds the process running |
| concurrent-claims | | `off` | eight connections claim the Runner at once for its own database while no gateway controls it: exactly one is granted and the others are refused because a controller is connected; once it detaches, the next gateway claims the Runner under the epoch after it |
| other-database | | `off` | a gateway with another operations database than the Runner's first Claim named is refused and starts nothing, whether or not a gateway controls the Runner |
| concurrent-gateways | | `off` | of two gateways started at once on the Runner's database, one serves and claims the Runner, and the other waits for the writer and refuses to start, never reaching the Runner |

<a id="recovery-reconcile"></a>

### Reconciliation

| Scenario | I/O | Participation | Passes when |
| --- | --- | --- | --- |
| running | both | `off`, `required` | after a gateway is killed while its process runs, the next gateway refuses new work in that workspace with `RECOVERY_PENDING` until it has reconciled it, then holds the workspace for the process again (`WORKSPACE_BUSY`), while a workspace where no work was left takes new work at once; terminating the inherited process frees its workspace, and no refused command ran |
| terminal | both | `off`, `required` | a process that ends while no gateway runs: the next gateway learns how it ended from the Runner, records it and frees the workspace |
| approval | | `off`, `required` | an approval held and granted before a gateway was killed is resumed by the next gateway once, and resuming it again returns the same process; one whose resume was in flight when the gateway was killed, at moments from 0 to 160 ms after the resume was sent, is resumed by the next gateway with the process the Runner created for it, or started then when the Runner proved that it created none. Each command ran exactly once |
| authority-outage | | `required` | while the fixture authority holds its lock, a killed gateway is replaced: the inherited governed processes answer at once, their workspaces stay refused with `RECOVERY_PENDING` while the Runner cannot ask the authority about their attempts, and ungoverned work runs. Once the authority answers, the gateway reconciles them, holds each workspace for its process again, and terminating them releases their attempts |
| missing-record | | `off` | a record removed from the database while no gateway ran: the next gateway can still query the process the Runner holds, keeps its workspace refused with `RECOVERY_PENDING` for an operator rather than start anything there, and other workspaces take new work |
| kill-during-exec | both | `off`, `required` | gateways killed while `exec_command` is in flight, at moments from 0 to 160 ms: the next gateway reconciles what each left, and no command ran more than once |
| kill-during-terminate | both | `off`, `required` | gateways killed while `terminate_process` is in flight, at the same moments: the next gateway reports the process terminated, or still running and then terminates it; each process ends once, terminated, after running once, and its workspace takes new work afterwards |

<a id="recovery-observation"></a>

### Observation and Runner loss

| Scenario | I/O | Participation | Passes when |
| --- | --- | --- | --- |
| ring-overflow | both | `off` | a process writes more than the Runner's 256 KiB output ring while no gateway runs: the next gateway reads on from where the last read stopped, says that output was lost before what it returns and why (`ring_overflow`), returns the newest output whole, and its status says so too |
| retention | | `off` | of more ended processes than the Runner keeps (64, then 64 more whose handles it released), `process_status` and `read_process` answer for the oldest from their records, how each ended with no output and `retention_expired`, also through the next gateway |
| runner-loss | both | `off`, `required` | the Runner killed with SIGKILL: its gateway answers from the records, the running process as `unknown` and the ended one as it ended, both with `runner_lost`, and refuses new work with `RUNNER_DISCONNECTED`. A pipe process that does not write outlives the killed Runner; `records list` shows its record, and `records release` refuses it while a gateway runs and while the process runs. Once the operator ended it (with an authority, once DevGuard released its attempt and retired the lost Runner's instance), the record is released, and a new Runner and gateway take new work in that workspace. A PTY's root process is not named, so the operator ends it before releasing |
| runner-replaced | | `off` | another Runner answers at the socket after the first was killed: the gateway does not take it for the old one, reports the old Runner's process `unknown` with `runner_lost` and refuses new work; the next gateway keeps that workspace refused until an operator releases the record, and takes new work elsewhere |

<a id="recovery-gaps"></a>

### Gaps

These are recorded in every run as scenarios not run, with the reason, or stated here; none counts as passed.

| Gap | Why, and what shows the behavior instead |
| --- | --- |
| A host restart | it cannot be made during a run. CodeSpace's tests give a record another boot ID ([`crates/server/src/mcp.rs`](../crates/server/src/mcp.rs)), so a restart is reported with `host_rebooted`; a real restart stays unqualified |
| Restoring processes after the Runner itself restarts | unsupported and outside P1-RECOVERY's scope; `runner-replaced` shows the refusal |
| Another process claiming the Runner while no gateway controls it | possible by design for any process of the Runner's user (decision D6 accepts this exposure); `stale-epoch` shows that it is logged and fenced, not prevented |
| Output beyond the 256 KiB ring while no gateway reads | lost by design; `ring-overflow` and the measurement show that the loss is declared, never silent |
| A PTY process after the Runner's loss | its root is not named in its record, so `records release` cannot tell whether it runs; the operator ends it first |
| A replay of a preparation or launch message on the runner wire | not injected, as in CS-RG; epoch fencing refuses one from an earlier controller (`foreign-connection`, `stale-epoch`) |
| Linux `required` | unsupported until DG-LINUX |

<a id="recovery-measurement"></a>

## Gateway restart measurement

The measurement restarts the gateway again and again while governed pipe and PTY processes run for a whole interval, idle and under DG-1's development load, against the installed service, and checks after every restart what the [completion evidence](#recovery-completion) asks for. It runs governed on macOS with `required`; anywhere else the same run is a self-test of the harness, named `independent-off`, which never qualifies.

<a id="recovery-intervals"></a>

### Intervals, cycles and samples

A repetition is a 10-minute idle interval followed by a load interval of at least 30 minutes; the protocol has three repetitions. Each interval starts a fresh Runner, which registers as the `codespace` consumer's one instance (decision D12), and its first gateway starts the processes that run the whole interval: a `cat` (pipe) and a `sleep` (PTY) in governed workspaces, which request 25 mCPU, 16 MiB and 2 tasks each, and in ungoverned ones a PTY that writes a numbered line every 0.2 seconds and a pipe that writes 40,000 numbered lines at once, more than the output ring holds, whenever the harness cues it. Through whichever gateway serves, the control path is sampled on the `control` suite's schedules ([intervals and samples](cs-rg-qualification.md#control-intervals)): status every second, a fresh governed termination every 20 seconds, the I/O echo every 5 seconds, a governed refusal every 30 seconds, and the host, service and footprint (the Runner's and the gateway's). Since a gateway serves before it has reconciled every workspace, a termination's target or a refusal's execution that the serving gateway refuses with `RECOVERY_PENDING` is sent again, for up to 10 seconds; a target still refused then is an error. Both output streams are read every 2 seconds. A slot that falls while no gateway serves is recorded as `restarting`, not as a sample, and the samples expected follow the time a gateway served.

The interval is a sequence of gateway cycles. The first lasts 20 seconds; every later gateway starts its executions with a 20-second process timeout, and its cycle is timed by that cycle's timeout target. In each cycle:

1. The serving gateway claimed the Runner under the cycle's epoch, and from the second cycle on it finds every long-running process `running`.
2. Another process of the same user connects to the socket: its Claim is refused, and its requests to write, resize, terminate a process, terminate a workspace's processes and stop the Runner, under the previous, the current and no epoch, are each refused `STALE_CONTROLLER`.
3. A rival gateway starts on the same operations database. It must wait for the writer and give up, never reaching the Runner; one still waiting when the cycle ends is killed.
4. The approval granted before the restart is resumed once the gateway has reconciled its workspace, and resumed again, which must return the same process; its command runs once.
5. Each timeout target whose deadline has passed is judged: when its process was first seen gone against the deadline recorded when it started, and how the serving gateway reports it.
6. Unless the interval ends, the gateway starts the cycle's timeout target (a `sleep`, governed or not, pipe or PTY, in turn), holds an approval and grants it 2 seconds before the restart, and is ended: in turn with its input closed, SIGTERM, SIGUSR1 or SIGKILL, all with the exit intent `detach`; before a SIGKILL the approval's resume is sent and left in flight for 0 to 160 ms. In blocks of four cycles the gateway ends 2 seconds before the target's deadline and the next one starts 4 seconds later, so the deadline falls while no gateway runs, or it ends 3 seconds after the deadline and the next one starts at once. In the fourth cycle and about every ninth after it, the pipe writes its burst while no gateway reads it.

At the interval's end the last gateway reads both streams once more, resumes the last approval, terminates every long-running process, which must be effective, and ends with its input closed. Then the Runner is drained as an operator drains it ([adoption and rollback](operations.md#independent-runner-adoption)): a gateway with the exit intent `stop` claims it under the next epoch and stops it, and the Runner exits 0, removes its socket, and terminates nothing, since the harness ended its processes first; with the service, its instance retires and nothing of the consumer stays charged. The load is the `control` suite's ([load](cs-rg-qualification.md#control-load)), cold by default. The display and the system are kept awake for the run.

<a id="recovery-measurement-verdicts"></a>

### Verdicts

An interval is **inconclusive**, an invalid observation, when the Runner did not register as the consumer's instance, a long-running process could not be started, other work was charged or a `codespace` instance was active as it began, the journal could not be read, raw evidence could not be written, the service could not be observed or had another release selected, or the load did not finish within its limit.

Otherwise it **fails** when the control path fails by the `control` suite's [rules](cs-rg-qualification.md#control-verdicts) over the time a gateway served, or when:

- a gateway ended otherwise than asked, a gateway claimed the Runner under another epoch than the next, or a long-running process was not `running` for the next gateway;
- any request of the other process changed something (a stale mutation), was refused other than as stale, or its Claim was granted;
- a rival gateway reached the Runner, served, or ended other than for the writer;
- a timeout target outlived its original deadline by more than 2 seconds or its recorded deadline moved (an extension), ended more than 0.25 seconds before it, or was not reported `timeout`;
- an approval was not resumed, a second resume returned another process, or its command ran twice or never;
- a tagged command ran twice, or one that was refused ran;
- an output stream skipped or repeated a line where the Runner declared no gap;
- the drain failed, or the last gateway did not exit 0 when its input closed;
- a cycle broke with an error.

It is also **inconclusive** when too little was observed: fewer restarts than 80% of one every 25 seconds, no deadline that fell while no gateway ran, no SIGKILL with a resume in flight, nothing read from an output stream, a timeout target whose end was not observed, the `control` suite's shortfalls, or a load that was not DG-1's. Otherwise it **passes**.

A repetition, the combination and the run are judged as in `control`: the combination qualifies when all three repetitions pass, and the run qualifies only under the full protocol (three repetitions of 10 and 30 minutes, governed). `--rehearsal` (2 minutes idle, 3 minutes load, one repetition) tries the run and never qualifies. The `recovery` suite qualifies only when its functional scenarios passed and its measurement qualified.

<a id="recovery-completion"></a>

## Completion evidence

| What P1-RECOVERY must show | Where it is shown |
| --- | --- |
| The same execution is reached again after a restart | every cycle's `running` statuses and output streams through every gateway; lifecycle `exit-kinds`; fence `stale-epoch` |
| No execution runs twice | the measurement's approvals and tagged commands, and with the service the launches that the gateways logged for each execution; reconcile `approval`, `kill-during-exec`, `kill-during-terminate` |
| No stale mutation | every cycle's fence probe and rival; fence suite |
| No timeout extension | the measurement's timeout targets; lifecycle `original-timeout` |
| Explicit gaps | [gaps](#recovery-gaps), recorded in every run |
| Existing modes still pass | [regressions](#recovery-regressions) |
| Supported modes | [the table](#recovery-supported-modes); Linux `off` is CI functional verification only |

<a id="recovery-run"></a>

## The qualification run

On the qualification host, on the merged main that holds this harness:

1. **Before freezing**, the Mac checkout is fetched with `--prune` and fast-forwarded to main, memory pressure is normal, and the installed DevGuard release and policy are the ones CS-RG qualified, unchanged.
2. **Freeze the candidate** with `candidate`, which must pass.
3. **Run `recovery`** with both participations, the functional suites and then the measurement in three repetitions.
4. **Drain.** Each interval drains its Runner; afterwards DevGuard must show no active `codespace` instance and nothing charged, since the consumer allows one instance and a live Runner would keep the CS-RG suites' gateways from registering.
5. **Run the CS-RG suites** `candidate`, `matrix`, `faults` and `authority` on the same candidate.
6. **Verify** every sealed directory with `verify`.

<a id="recovery-regressions"></a>

## Regressions

The CS-RG suites in step 5 show that the qualified modes still behave as qualified on the candidate, and a failure in any of them fails the qualification. CI on the candidate's commit shows the regression stages on Linux and macOS, the independent Runner's tests included.

<a id="recovery-evidence"></a>

## Evidence

Every suite writes a new directory and seals it as for CS-RG ([evidence](cs-rg-qualification.md#evidence)). A recovery directory also holds each Runner's argv and log, each gateway's argv, log and MCP transcript, and for the measurement each interval's raw rows: the samples, every cycle (`restarts.jsonl`), the timeout targets, the approvals, every tagged command (`execs.jsonl`), the output reads, and the marker files each workspace's commands wrote, one line per run. Nothing in a directory holds a credential, a credential digest or a child's environment.

`qualify-devguard.py verify <directory>` checks the manifest and recomputes every verdict from the raw files: each scenario's verdict from its checks, each recovery interval's metrics and verdict from its rows, then each repetition, the combination and the run. It writes nothing.
