# Runner isolation

[English](runner-isolation.md) | [한국어](ko/runner-isolation.md)

There are three separate questions: which process runs the work, whether commands are sandboxed, and whether execution moves into a container. Today the first two are implemented; container dispatch is not.

<a id="later-process-split"></a>

## Host and worker execution

`in-process` runs the supervisor inside `codespace-mcp`. `uds` (Unix domain socket) runs that supervisor inside `codespace-codex-runtime` on the same host. The worker starts with Codex process hardening and binds a private Unix socket. Hardening the worker does not sandbox its commands.

The gateway creates a unique 0700 directory beneath its temporary directory or `CODESPACE_RUNNER_DIR`. The internal protocol is CodeSpace JSON with a u32 length prefix, handshake version 10, request IDs, and process-exit events. It is not Codex App Server RPC. Same-connection replay is not reconnect recovery.

Managed process lifetime is owned by the runner instance, not the MCP connection. Streamable HTTP or MCP client disconnect keeps processes running. Gateway/worker UDS disconnect or gateway shutdown (including stdio EOF) ends the owned worker and its children; process handles are lost. At shutdown the gateway closes the worker's connection, so the worker ends its processes as it exits; a worker still running 5 seconds later is killed. Restart does not restore `process_id`. An [independent Runner](#independent-runner) is the exception: its processes outlive the gateway.

**How children start.** Pipe commands, `tty: true` commands, the patch helper, the Linux sandbox helper and the worker are started with fork and exec, because each runs a step in the child before exec: pipe, patch-helper, sandbox-helper and worker children keep only their standard descriptors ([#79](https://github.com/novelKR/CodeSpace/issues/79)), and a PTY child becomes the leader of a new session. On macOS, a child forked while another thread of the same process is initializing the system's notification library (libnotify) is killed by a signal inside `fork`, before the command runs. `process_status` reports a pipe command killed this way as `signaled`, and a `tty: true` command as an exit with code 1 ([Agent Loop integration](agent-integration.md)). So `codespace-mcp` and `codespace-codex-runtime` complete that initialization before they start any thread, and every one of these spawns completes it before it forks. This removes that one cause; it does not make forking from a multithreaded process safe in general.

<a id="independent-runner"></a>

## Independent Runner

> **Not a supported mode yet.** The independent Runner, the records of its executions and the Claim that hands its control from one gateway to the next are the first steps of Gateway recovery. A gateway claims the Runner again when its connection is lost, but a gateway that controls it after one that started work, the same gateway after a lost connection included, cannot start new work. A later step adds reconciliation, and the mode is supported only once it is qualified.

An independent Runner outlives the gateways that connect to it: its processes, their output and their timeouts carry on when a gateway exits, restarts or is killed. An operator or a harness starts it, and a gateway only connects to it:

```bash
codespace-codex-runtime "$RUNNER_DIR/runner.sock" --lifecycle independent
codespace-mcp --runner uds --runner-socket "$RUNNER_DIR/runner.sock" \
  --runner-lifecycle independent --runner-exit-intent detach \
  --operations-db "$STATE_DIR/operations.sqlite"
```

The Runner creates its socket's directory with mode 0700, or accepts an existing one only when it is a directory of the same user with mode 0700 and not a symbolic link. It never changes an existing directory, and it refuses `/`, `/tmp`, `/var/tmp` and `$HOME`. It serves any number of connections at once, and one of them at a time [controls it](#claim). When it stops, it removes its socket, and the directory if it created it.

A gateway does not start when its `--runner-lifecycle` differs from the Runner's lifecycle, when the Runner does not state its identity, or when `--runner-lifecycle independent` comes without `--runner uds`, `--runner-socket`, `--runner-exit-intent` or `--operations-db`, or with `--runtime-bin` or `--runner-dir`. `--runner-exit-intent` is accepted only with `--runner-lifecycle independent`.

| Event | Managed worker (default) | Independent Runner |
| --- | --- | --- |
| The gateway shuts down: its stdin ends, or it receives SIGTERM or SIGINT | The worker ends its processes and exits | With `--runner-exit-intent detach` the gateway detaches, and the processes carry on. With `stop` the Runner ends its processes and exits |
| The gateway receives SIGUSR1 to be restarted | Not handled: as when the gateway is killed | The gateway detaches whatever its exit intent, and the processes carry on |
| The gateway is killed, or its connection ends without a detach or a stop | The worker ends its processes and exits | A loss, never a stop: the processes carry on, and the Runner waits for the next Claim, which comes from the same gateway when only its connection was lost |
| A call to the Runner passes its 120-second deadline | The gateway closes the connection, and the worker ends its processes and exits | The gateway closes the connection and claims the Runner again, and the processes carry on |
| A gateway asks the Runner to stop, or the Runner receives SIGTERM or SIGINT | A managed worker refuses the request and does not handle these signals | The Runner ends its processes, waits up to 5 seconds for them, and exits |
| A process reaches its timeout | The process is killed | The process is killed at its original deadline, whether or not a gateway is connected |

Ending a process signals a tty process's process group, but a pipe process alone: a pipe process has no process group of its own, so the processes it started may outlive it, a stop included.

<a id="claim"></a>

**Claim and controller epoch.** A gateway takes control of the Runner with a `Claim` that names its operations database. The Runner grants it only when the connection's peer runs as the Runner's own user, as the socket's peer credentials state, and when the Runner was not claimed before for another operations database: it keeps the identity of the database of its first Claim and refuses a gateway of another one. That identity guards against a gateway started with the wrong database. It is not a credential, since the user who owns the database can read it. The Runner refuses a Claim while another connection controls it and is still open, even one whose gateway is stopped: nothing pushes a live controller out. Once that connection has ended, a Claim waits up to 10 seconds for the requests it sent, preparations and launches included. When they run longer, the Claim is refused and changes nothing.

Each granted Claim raises the Runner's controller epoch by one. The epoch never goes down while the Runner lives; a new Runner starts again from 1. The Claim's answer lists the Runner's processes, each with its workspace and status, and states the number of the last exit event that the list reflects. A connection that does not control the Runner gets answers to `Hello`, `Claim` and `Detach` only, and the Runner sends exit events only to its controlling connection. Each request that changes a process or the workspace, or stops the Runner, carries the epoch of the Claim it was sent under: an execution, a patch, writing to stdin, resizing, terminating a process or a workspace's processes, a stop, and with DevGuard a preparation or a launch. The Runner refuses such a request from an earlier epoch with `STALE_CONTROLLER`, and nothing it asked for is done. The gateway records the epoch with each execution, and the Runner logs each Claim that it grants or refuses with the process ID of the peer that made it.

A gateway that starts while another connection controls the Runner, or while that connection's requests run, asks again with a growing pause for up to 30 seconds, and then refuses to start. A gateway refused for another reason, such as another operations database, refuses to start at once and leaves the Runner as it was.

**A lost connection.** A gateway whose connection to the Runner is lost, or that closed it after a call passed its deadline, connects and claims the same Runner again. It first checks that the Runner at the socket has the identity it found at its start. It tries at once, waits 100 milliseconds after the first failed try, and twice as long after each further one, up to 5 seconds. Meanwhile it refuses each request that needs the Runner with `RUNNER_RECONNECTING`: nothing is sent, reserved or recorded for the request, and an approval it would resume is not consumed. When nothing accepts connections at the socket any more, another Runner answers there, or the Runner refuses the Claim for a reason that waiting cannot end, the gateway stops trying and refuses every such request with `RUNNER_DISCONNECTED`; it has to be restarted to reach the Runner's processes again. A request that was already sent when the connection was lost keeps the uncertain result it has today, such as `dispatch_status: unknown` for an execution. An exit event that the Runner could not deliver goes to its next controller, and the gateway hands each exit on once.

**After an earlier controller.** The first gateway that claims a newly started Runner works as it does with a managed worker. Once a controller has started work there (an execution, a patch, or with DevGuard a preparation or a launch), every later controller inherits the Runner's processes, the same gateway included once it claimed the Runner again after a lost connection. It can query, read, write to, resize and terminate them with their `process_id`, but it refuses new executions, patches and approval resumes with `RECOVERY_PENDING`, and leaves the approvals as they are. This holds after the inherited processes have ended too; to start work again, stop the Runner and start a new one. The gateway logs the Runner's identity (its process ID, boot and start time), the epoch of its Claim, whether a controller before it started work, how many processes it inherited, and the number of the last exit event that list reflects.

<a id="claim-exposure"></a>

**Who can take control.** The Runner trusts any process of its own user that names the operations database. No other process can take control while a gateway controls the Runner. While no gateway does, between one gateway's end and the next one's Claim or while a gateway reconnects, any process of the same user that can reach the socket can claim the Runner. That process then holds the Runner's processes, their input, output and termination, and it can start work when no controller before it did. The gateway takes control again only once that connection ends, and asks again meanwhile. Such a process may be a command that a gateway runs without a sandbox: on macOS, and on Linux without the sandbox helper. A command that the Linux sandbox runs cannot connect to the socket: the sandbox denies it the connection with a restricted network, and Unix sockets with an enabled network. To keep such processes out, run the gateway and the Runner as a user that runs nothing else you do not trust, and on Linux keep the sandbox helper available, so that the gateway's commands run sandboxed.

<a id="process-records"></a>

**Process records.** The gateway records each execution in its operations database before it sends the execution to the Runner, and follows it to its end, so that a gateway that lost its memory can identify the same execution again. A record holds the `process_id`, the identity of the Runner it was sent to (its process ID, boot, start time and a random nonce), the [controller epoch](#claim) it was sent under, the workspace, the approval it resumes, its DevGuard attempt, whether it takes part in DevGuard, whether it has a tty, and the SHA-256 digest of its argv, never the argv, its environment or a secret. Its phase is `dispatching` until the Runner answers that it started the execution, and `running` after that. It becomes `terminal` once the Runner reports that the execution ended, with the termination, exit code and signal the Runner reported, or once the Runner answers that it did not start it. The record also keeps the deadline the Runner set, the root process the Runner created, by its process ID and the start time that tells it apart from a later process with the same ID (a `tty: true` root is not named), and how far a gateway read the output.

An execution that resumes an approval is recorded in the transaction that marks the approval resuming. When the record cannot be written, the execution is refused with `INTERNAL` and nothing is sent. The next controller records the exits that happened while no gateway controlled the Runner, which the Runner keeps for it, and `process_status` and `read_process` record what the Runner reports. A record never revives a handle, and never by itself proves that an execution started or how it ended: the Runner is the authority, and the record of an execution whose answer was lost stays `dispatching`. Recovering work from the records comes in later steps.

**One writer.** A gateway with an independent Runner is its operations database's only writer. Before it opens the database it takes an exclusive lock on a file beside it, `<database>.gateway.lock`, which names the process ID of the gateway that holds it, and it keeps the lock until it exits, however it exits. A gateway that finds the lock held, such as one started while the gateway before it still detaches, logs that it waits, waits up to 15 seconds, and then refuses to start. Only this mode takes the lock. See [Operations](operations.md#operations-database) for the database's schema version and earlier CodeSpace versions.

**DevGuard.** With the `devguard` feature, the independent Runner registers itself with the worker's DevGuard settings and reads the consumer secret from the private file given with `--devguard-credential-file`; its gateway, started with `--devguard register`, hands it no secret. Workspaces that require resource participation run only when the Runner states that it registers and launches through a launch helper, and are refused with `RESOURCE_POLICY_UNSUPPORTED` otherwise. See [DevGuard integration](devguard-integration.md#resource-gateway-recovery).

<a id="macos-no-docker"></a>

## Linux command sandbox

Both pipe and PTY execution use the same Linux helper path. On Linux, the runner probes `CODESPACE_LINUX_SANDBOX_BIN` or a sibling `codespace-linux-sandbox` binary. A successful probe enables preparation and sandboxed execution. The probe result is cached for that process.

```text
Runner → helper prepare (CodeSpace JSON, protocol 1)
       ← opaque plan pathname
Runner → managed helper run --plan
       → Codex sandbox setup → requested command
```

Codex permission translation and sandbox arguments stay inside the binary-only helper. The plan is a private 0600 file consumed by the helper. The runner depends on the small protocol crate, not the sandbox implementation library. Restricted execution uses self-exec; enabled execution keeps a helper-owned proxy while waiting for its sandbox child.

A failed initial probe permits unsandboxed host execution for `restricted`; the contract reports `command_sandbox: none` and no OS network enforcement. `enabled` requires the helper and fails without it. Once a probe succeeds, later prepare/protocol/spawn errors never fall back to unsandboxed execution. They report `PROCESS_SPAWN_FAILED`. Failure inside an already-started helper is observed as process termination through `process_status`. That wait status belongs to the managed child (helper argv) and is not documented as identical to the user argv.

## Network and filesystem scope

Restricted mode uses network namespace separation and restricted seccomp rules. Enabled mode uses an isolated network namespace with a managed HTTP proxy. Proxy bypass environment entries are cleared so loopback HTTP also follows the proxy. Direct host networking is not the fallback. Destination domain restrictions and compatibility with every network client are not implemented guarantees.

Read the effective `workspace_info.execution` fields. File tools stay workspace-scoped independently of command sandboxing. `codespace-fs` provides no-follow I/O behind logical path checks. The patch helper has its own path checks and Codex patch options; command sandboxing does not automatically wrap all file tools.

Sandboxed commands use a limited system PATH rather than mounting host toolchains from the user's home. Prepare the required compilers, package caches, and binaries in the execution environment. A command that works on the host may fail in the sandbox because its executable or dependency is unavailable.

<a id="what-the-compose-fixture-does"></a>

## Container fixture and verification

`deploy/compose.yml` runs a non-root sleeper with only the selected workspace mounted at `/workspace`. It does not install the server, launch a Runner, or receive `exec_command` calls. Never describe starting this fixture as activating the MCP command sandbox.

CI has a Linux isolation job that installs bubblewrap and requires helper availability for isolation tests. macOS checks cover host behavior, not Linux enforcement. Kernel escapes, Docker Desktop differences, and arbitrary remote deployments need separate assessment. See [operations](operations.md) for setup and [security model](security-model.md) for trust assumptions.
