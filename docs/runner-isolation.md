# Runner isolation

[English](runner-isolation.md) | [한국어](ko/runner-isolation.md)

**Target** execution isolation OS is a Linux container. **Current**
`exec_command` is a host process. When the Linux helper probe succeeds,
pipe and PTY spawn the same `codespace-linux-sandbox run --plan` argv
(bubblewrap + `no_new_privs`/seccomp; Codex translation stays inside
that process). When the probe fails (macOS, no
bwrap), spawn is unsandboxed and `workspace_info` advertises `none`.
Gateway unit tests may run on macOS. That is not a
claim that Linux isolation was verified on the development laptop.

## What the compose fixture does

[`deploy/compose.yml`](../deploy/compose.yml) is an **isolation fixture**.
It runs an unprivileged user (`uid 10001`), bind-mounts **only** the
workspace at `/workspace`, and sleeps. It does not ship
`codespace-mcp` / `codespace-patch`, and it is **not** connected to
`exec_command`.

It does not mount:

- host `$HOME`
- SSH agent socket
- `/var/run/docker.sock`
- gateway `.env`, Bearer files, or SQLite

There is no runner control socket on the compose fixture. Do not add a
host Docker socket or a future control socket to this fixture by
accident. Opt-in `CODESPACE_RUNNER=uds` uses a **private** gateway↔worker
Unix socket off this fixture. The gateway creates a unique 0700 leaf
(`$TMPDIR/codespace-runner-<pid>-<rand>/` or
`$CODESPACE_RUNNER_DIR/run-<pid>-<rand>/`) and binds `$dir/runner.sock`.
Live sockets are probed with `connect`; only `ConnectionRefused`
leftovers are unlinked. `/tmp` itself is never chmodded.

## macOS / no Docker

`codespace-runner::PathSandbox` applies the same relative-path + symlink
+ special-file rules for unit tests and for `read` / `find` / versions.
That is workspace authorization, not race-proof I/O. File bytes,
metadata, mkdir, chmod, remove, and bounded walks go through isolated
`crates/file-system` (`codespace-fs`, no-follow `LOCAL_FS`). Live
processes may mutate the tree between PathSandbox's lstat and the
adapter open; no-follow I/O is the safety boundary. If Docker is not
used, **Linux container isolation is unverified**. Host
seccomp/AppArmor and Docker Desktop vs Linux engine differences are also
unverified.

## Later process split

Today `codespace-mcp` is one process by default. `crates/runner` hosts
`InProcessRunner` (`PathSandbox`, one `apply_patch` transaction, host
supervisor) and the opt-in Unix-socket `UdsRunner` client. The
worker is isolated `crates/codex-runtime` (`codespace-codex-runtime`):
`codex_process_hardening::pre_main_hardening()` stays the first line of
`main` (process hardening of the worker/helper, **not** a command
sandbox; no `ctor`). Then bind `$dir/runner.sock` (no parent chmod),
then **one** `InProcessRunner` for the process. Wire format is **u32
length-prefix + CodeSpace JSON** (`protocol: 3`, Hello handshake,
`request_id` `rrpc-…`, events include `ProcessExited`), not App Server.
P0 UDS is 1:1: the gateway owns the worker child (`kill_on_drop`);
disconnect or gateway shutdown kills the worker and host children;
`process_id` does not survive; there is no reconnect. Runner `Replay`
is same-connection only. That **transport** is implemented; it
is opt-in (`CODESPACE_RUNNER=uds` / `CODESPACE_RUNTIME_BIN`) on the
**same host**. Linux command sandbox is a wrap of that same
`InProcessRunner` spawn (`helper probe` / `prepare` / `run --plan`), not a second transport rewrite.
Prefer `codex-uds` as the socket primitive; the Runner RPC stays a
CodeSpace contract.

Linux isolation is still the target OS. Landlock, seccomp, PTY helpers,
UDS, filesystem mechanics, and network isolation are **not** a default
homegrown stack. Prefer upstream execution subgraphs
([codex-reuse.md](codex-reuse.md)), staged
process-hardening → PTY → UDS/path → filesystem → linux-sandbox →
network (filesystem is taken via `crates/file-system`; linux-sandbox
via the `crates/linux-sandbox` binary and `crates/linux-sandbox-protocol`).
`codex-linux-sandbox` can sit beside a
container; keep its `codex-core` **dev-dep** out of the product graph.
The next WP is network (`Enabled` + proxy). `codex-exec` stays
rejected. `codex-exec-server` is a reference / future backend, not a
forever reject. Gateway policy remains the only allow path. Do not
mount a host Docker socket or a future control socket on the compose
fixture by accident.
