<a id="chatgpt-connector-experiment"></a>

# ChatGPT connection status

[English](chatgpt-connector.md) | [한국어](ko/chatgpt-connector.md)

First establish a working local MCP integration using [installation](operations.md) and [Agent Loop integration](agent-integration.md). CodeSpace implements stdio and Streamable HTTP, but a live ChatGPT account connection is not verified by this repository's local tests.

<a id="local-stdio-verified-by-cargo-test"></a>
<a id="local-streamable-http-experiment"></a>

## Local MCP clients

Configure a stdio-capable client to launch the built `codespace-mcp` executable with absolute paths for `CODESPACE_CONFIG`, `CODESPACE_PATCH_BIN`, and optionally `CODESPACE_OPERATIONS_DB`. The exact client configuration wrapper varies by client; the executable, arguments, and environment are the portable parts.

For an HTTP-capable client, start the configured server with `--http` and connect to `http://127.0.0.1:8787/mcp`. If `CODESPACE_HTTP_TOKEN` is set, send the matching Bearer header. Complete initialization and call `workspace_info` for a registered workspace; a successful server startup alone is insufficient.

<a id="chatgpt-account-connection"></a>

## ChatGPT-specific requirements

OpenAI's [developer-mode documentation](https://developers.openai.com/api/docs/guides/developer-mode#how-to-use), checked on 2026-09-19, describes remote MCP apps with streaming HTTP and supported authentication modes including OAuth. It does not establish that CodeSpace's static Bearer configuration is a compatible ChatGPT authentication flow.

CodeSpace has no OAuth authorization server. It also validates HTTP Host values from its bind configuration rather than providing a separate public-host setting. Treat public HTTPS reachability, Host handling, authentication, tool discovery, and a harmless test call as separate integration checks. Do not publish an unauthenticated writable workspace just to bypass an authentication mismatch.

## Verification status

| Check | Evidence or remaining work |
| --- | --- |
| Local stdio initialization and tool calls | Repository transport and protocol tests |
| Local Streamable HTTP initialization and tool calls | Repository HTTP and protocol tests |
| Real ChatGPT account connection | Not verified; requires testing against the intended account and deployment |
| Public HTTPS/proxy configuration | Not established by loopback tests |
| ChatGPT acceptance of static Bearer | Not established; do not assume support |

See [protocol compatibility](protocol-compatibility.md) for repository-tested versions. Do not infer ChatGPT's negotiated version or required optional MCP features from those tests.

<a id="secrets"></a>

## Credentials

Keep tokens out of commands shared in issues, logs, and committed examples. The HTTP authentication failure response contains only `{"error":"unauthorized"}`. Account secrets and public exposure are operator responsibilities; this guide does not configure them automatically.
