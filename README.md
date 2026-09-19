# CodeSpace

[English](README.md) | [한국어](README.ko.md)

CodeSpace is an MCP (Model Context Protocol) server. It gives an external coding agent a workspace it can inspect, edit, and run commands in through MCP. Your agent plans the work and interprets results; CodeSpace checks workspace permissions, performs operations, and keeps their execution state. It does not call a model or run an agent loop.

<a id="run"></a>

## Start with a registered workspace

Follow [installation and first connection](docs/operations.md) to build the server and patch helper, register a project directory, and connect over stdio or Streamable HTTP. Starting the binary without a workspace registry leaves it with no accessible projects.

Then use the [Agent Loop integration guide](docs/agent-integration.md) for the read → patch → run → inspect cycle, including cancellation and uncertain results. The [documentation overview](docs/index.md) points to reference material.

## Available tools

| Purpose | Tools |
| --- | --- |
| Inspect the environment and files | `workspace_info`, `find`, `read` |
| Apply a patch and retrieve its recorded state | `apply_patch`, `operation_status` |
| Run and control a process | `exec_command`, `read_process`, `write_stdin`, `terminate_process` |
| Track a logical job and queued user instructions | `work_open`, `steer_status`, `steer_claim_next`, `steer_complete`, `work_finish` |

Both transports expose the same tools. The HTTP `/inbox` API lets a user-facing client manage instruction drafts; it is a JSON API, not a browser inbox application.

<a id="status"></a>

## Execution and current limits

The default runner executes on the server host. An optional Unix-socket worker moves execution into a separate process on that same host. On Linux, a successful sandbox-helper probe enables command isolation and network enforcement. These are separate choices: UDS alone does not provide sandboxing. See [runner isolation](docs/runner-isolation.md).

CodeSpace reuses pinned Codex execution libraries for patches, terminal sessions, filesystem operations, and Linux sandboxing. [Codex reuse](docs/codex-reuse.md) explains which components are connected and which responsibilities stay in CodeSpace.

For agent integrations, account for these limits:

- Process results expose output and EOF, but no exit code. EOF alone cannot establish that a test passed.
- Process output is bounded; dropped output has no explicit flag in the MCP result.
- A live command blocks another command or patch in the same workspace. A development server cannot remain running while that workspace is patched.
- Process handles do not survive server restart. Patch-operation records persist only when a database path is configured.
- Container dispatch and a full OAuth server are not implemented. A live ChatGPT account connection remains unverified.

## License

Apache License 2.0. See [LICENSE](LICENSE) and [dependency attribution](NOTICE).
