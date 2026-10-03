#!/usr/bin/env python3
"""Drive a CodeSpace gateway over Streamable HTTP for the temporary fork diagnostics.

Logs each step with the wall clock that the LLDB callbacks also print:
initialize, a read, a pipe exec, a PTY exec and a patch, each waited for.
"""

import json
import socket
import sys
import time
import urllib.request


def log(step, detail=""):
    print("DRIVER t=%.6f step=%s %s" % (time.time(), step, detail), flush=True)


class Client:
    def __init__(self, port):
        self.url = "http://127.0.0.1:%d/mcp" % port
        self.session = None
        self.next_id = 1

    def post(self, method, params=None, notify=False):
        body = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            body["params"] = params
        if not notify:
            body["id"] = self.next_id
            self.next_id += 1
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        request = urllib.request.Request(self.url, data=json.dumps(body).encode(), headers=headers, method="POST")
        with urllib.request.urlopen(request, timeout=120) as response:
            self.session = response.headers.get("Mcp-Session-Id") or self.session
            text = response.read().decode()
        if notify or not text.strip():
            return None
        if text.lstrip().startswith("data:") or "\ndata:" in text:
            text = "\n".join(line[5:].strip() for line in text.splitlines() if line.startswith("data:"))
        return json.loads(text)

    def tool(self, name, arguments):
        reply = self.post("tools/call", {"name": name, "arguments": arguments})
        result = reply.get("result") or {}
        structured = result.get("structuredContent")
        if structured is None:
            for item in result.get("content") or []:
                if item.get("type") == "text":
                    try:
                        structured = json.loads(item["text"])
                    except ValueError:
                        structured = item["text"]
                    break
        return reply.get("error"), result.get("isError", False), structured


def wait_listening(port, seconds):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def wait_exit(client, process_id):
    # Exited and drained: the workspace lease is released only once the output reaches EOF.
    for _ in range(600):
        error, is_error, status = client.tool("process_status", {"process_id": process_id})
        if error or is_error:
            return "status error %s %s" % (error, status)
        if status.get("state") == "exited" and status.get("eof"):
            return json.dumps(status, sort_keys=True)
        time.sleep(0.05)
    return "still running"


def tool_when_free(client, name, arguments):
    # A process of an earlier step may hold the workspace lease for a moment longer.
    for _ in range(100):
        error, is_error, result = client.tool(name, arguments)
        busy = is_error and isinstance(result, dict) and result.get("code") == "WORKSPACE_BUSY"
        if not busy:
            break
        time.sleep(0.1)
    return error, is_error, result


def main():
    port = int(sys.argv[1])
    workspace = sys.argv[2]
    log("waiting", "port=%d" % port)
    if not wait_listening(port, 600):
        log("not-listening")
        return 1
    log("listening")
    client = Client(port)
    reply = client.post("initialize", {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "fork-diagnostics", "version": "0"},
    })
    log("initialized", json.dumps((reply or {}).get("result", {}).get("protocolVersion")))
    client.post("notifications/initialized", notify=True)

    log("read-start")
    error, is_error, result = client.tool("read", {"workspace_id": workspace, "path": "probe.txt"})
    log("read-done", "error=%s is_error=%s content=%r" % (error, is_error, (result or {}).get("content") if isinstance(result, dict) else result))

    for label, tty in (("pipe", False), ("pty", True)):
        log("exec-%s-start" % label)
        error, is_error, result = tool_when_free(client, "exec_command", {"workspace_id": workspace, "command": ["/usr/bin/true"], "tty": tty})
        log("exec-%s-returned" % label, "error=%s is_error=%s result=%s" % (error, is_error, json.dumps(result)))
        if isinstance(result, dict) and result.get("process_id"):
            log("exec-%s-exited" % label, wait_exit(client, result["process_id"]))

    patch = "*** Begin Patch\n*** Add File: added.txt\n+added\n*** End Patch\n"
    log("patch-start")
    error, is_error, result = tool_when_free(client, "apply_patch", {"workspace_id": workspace, "patch": patch})
    log("patch-done", "error=%s is_error=%s result=%s" % (error, is_error, json.dumps(result)))

    log("read2-start")
    error, is_error, result = client.tool("read", {"workspace_id": workspace, "path": "added.txt"})
    log("read2-done", "error=%s is_error=%s" % (error, is_error))
    log("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
