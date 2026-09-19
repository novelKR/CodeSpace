# CodeSpace documentation

[English](index.md) | [한국어](ko/index.md)

Use CodeSpace as the execution layer beneath your own coding agent. Start with a local workspace, establish an MCP connection, then add the execution and recovery rules your agent needs.

<a id="find-your-next-step"></a>

## Choose a guide

| Your goal | Start here |
| --- | --- |
| Understand the product and its limits | [Getting started](../README.md) |
| Install and connect a workspace | [Operations](operations.md) |
| Build a read, edit, run, and recovery loop | [Agent Loop integration](agent-integration.md) |
| Evaluate a ChatGPT connection | [ChatGPT connection status](chatgpt-connector.md) |
| Understand module responsibilities | [Architecture](architecture.md) and [execution contracts](execution-substrate.md) |
| Check permissions and isolation | [Security model](security-model.md) and [runner isolation](runner-isolation.md) |
| Handle protocol and tool results | [Protocol compatibility](protocol-compatibility.md), [patch behavior](behavior-differences.md), [error codes](error-codes.md) |
| Maintain Codex dependencies | [Reuse scope](codex-reuse.md), [pinned revision](upstream-lock.md), [update procedure](upstream-update.md) |
| Edit or publish these guides | [Documentation maintenance](documentation.md) |

The implementation defines supported behavior. A test description identifies coverage; it is not a claim that a particular installation or external account was tested. Read the execution contract returned by `workspace_info` for the workspace you actually use.
