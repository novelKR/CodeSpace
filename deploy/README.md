# Container isolation fixture

This fixture starts a non-root `sleep infinity` process. It does not install
CodeSpace, start an MCP server, or receive Runner commands. Registering a
`linux-container` environment does not make it an implemented backend.

`Dockerfile` / `compose.yml` mount only `${CODESPACE_WORKSPACE}` at `/workspace`.
Do not add host home directories, SSH agent or Docker sockets, or gateway secrets.

For an operational MCP server and the optional Linux sandbox helper, follow
[installation and operations](../docs/operations.md). The distinction between
host execution, the UDS worker, and command isolation is explained in
[runner isolation](../docs/runner-isolation.md).
