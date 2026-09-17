# Runner isolation

[English](runner-isolation.md) | [한국어](ko/runner-isolation.md)

**Target** execution isolation OS is a Linux container. **Current**
`exec_command` is a host process (`tokio::process::Command`, workspace
cwd, `env_clear`). Gateway unit tests may run on macOS. That is not a
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

There is no runner control socket today. Do not add a host Docker
socket or a future control socket to this fixture by accident.

## macOS / no Docker

`codespace-runner::PathSandbox` applies the same relative-path + symlink
+ special-file rules for unit tests and for `read` / `find` / versions.
If Docker is not used, **Linux container isolation is unverified**. Host
seccomp/AppArmor and Docker Desktop vs Linux engine differences are also
unverified.

## Later process split

Today `codespace-mcp` is one process. `crates/runner` hosts the in-process
`Runner` (`PathSandbox`, one `apply_patch` transaction, host supervisor).
A later Unix-socket / `ContainerRunner` worker would live in the same
crate; both sides remain Rust. That **transport** split is the next
implementation WP; it is not this document. Prefer `codex-uds` as the
socket primitive; the Runner RPC stays a CodeSpace contract.

Linux isolation is still the target OS. Landlock, seccomp, PTY helpers,
UDS, filesystem mechanics, and network isolation are **not** a default
homegrown stack. Prefer upstream execution subgraphs
([codex-reuse.md](codex-reuse.md)), staged
process-hardening → PTY → UDS/path → filesystem → linux-sandbox →
network. `codex-linux-sandbox` can sit beside a container; keep its
`codex-core` **dev-dep** out of the product graph. `codex-exec` stays
rejected. `codex-exec-server` is a reference / future backend, not a
forever reject. Gateway policy remains the only allow path. Do not
mount a host Docker socket or a future control socket on the compose
fixture by accident.
