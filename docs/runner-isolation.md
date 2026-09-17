# Runner isolation (W05)

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

Today `codespace-mcp` is one process. `crates/runner` already hosts
`PathSandbox` and the in-process host supervisor (`InProcessRunner`).
A later Unix-socket / `ContainerRunner` worker would live in the same
crate; both sides remain Rust.
