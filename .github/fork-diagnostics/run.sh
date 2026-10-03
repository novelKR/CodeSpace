#!/bin/bash
# Temporary macOS fork diagnostics: which call first initializes libnotify, and when, in the
# fork_race harness's read path and in the production gateway and worker. Every probe runs
# under LLDB with logging-only breakpoints (lldb_probe.py); the MCP driver drives the gateway.
# Outputs go to diag-out/.
set -u

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/diag-out"
mkdir -p "$out"
bin="$CARGO_TARGET_DIR/debug"
gateway="$bin/codespace-mcp"
worker="$bin/codespace-codex-runtime"
export CODESPACE_PATCH_BIN="$bin/codespace-patch"
started="$(date +%s)"

harness="$(cd "$root" && cargo test --locked -p codespace-runner --test fork_race --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == "fork_race" and message.get("executable"):
        print(message["executable"])' | tail -1)"
echo "harness=$harness gateway=$gateway worker=$worker patch=$CODESPACE_PATCH_BIN"
ls -l "$harness" "$gateway" "$worker" "$CODESPACE_PATCH_BIN"

# wait_or_kill PID SECONDS
wait_or_kill() {
  local pid="$1" limit="$2" waited=0
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$waited" -ge "$limit" ]; then
      echo "timeout: killing $pid"
      kill -KILL "$pid" 2>/dev/null
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
  wait "$pid" 2>/dev/null
}

listener() {
  lsof -nP -iTCP:"$1" -sTCP:LISTEN -t 2>/dev/null | head -1
}

breakpoints() {
  cat <<EOF
command script import $here/lldb_probe.py
process handle SIGINT --pass true --stop false --notify true
process handle SIGTERM --pass true --stop false --notify true
process handle SIGPIPE --pass true --stop false --notify false
process handle SIGCHLD --pass true --stop false --notify false
breakpoint set --name _os_alloc_once --shlib libsystem_platform.dylib
breakpoint command add --python-function lldb_probe.alloc_once_hit
breakpoint set --func-regex ^_?notify_ --shlib libsystem_notify.dylib
breakpoint command add --python-function lldb_probe.notify_hit
breakpoint set --name fork --shlib libsystem_c.dylib
breakpoint command add --python-function lldb_probe.fork_hit
breakpoint set --name posix_spawn --name posix_spawnp
breakpoint command add --python-function lldb_probe.spawn_hit
breakpoint set --name ptrace
breakpoint command add --python-function lldb_probe.ptrace_hit
breakpoint list
EOF
}

ws="$(mktemp -d /tmp/fdws.XXXXXX)"
echo probe > "$ws/probe.txt"
cat > "$out/registry.json" <<EOF
{"workspaces": {"demo": {"root": "$ws", "profile": "workspace-write"}}}
EOF

echo "== A: harness read path (--probe-read) =="
{
  echo "target create \"$harness\""
  breakpoints
  echo "process launch -i /dev/null -- --probe-read"
} > "$out/A-read.lldb"
lldb --batch -s "$out/A-read.lldb" > "$out/A-read.log" 2>&1 &
wait_or_kill $! 300
grep -E "ALLOC-ONCE-FIRST|NOTIFY-CALL #1 |probe-read|FORK|POSIX-SPAWN|Process .* exited" "$out/A-read.log" | head -60

echo "== B: gateway, in-process runner =="
{
  echo "target create \"$gateway\""
  breakpoints
  echo "process launch -i /dev/null -o $out/B-gateway.stdout -e $out/B-gateway.stderr -- --http --port 18781 --config $out/registry.json"
} > "$out/B-gateway.lldb"
lldb --batch -s "$out/B-gateway.lldb" > "$out/B-gateway.log" 2>&1 &
lldb_pid=$!
python3 "$here/mcp_driver.py" 18781 demo > "$out/B-driver.log" 2>&1
pid="$(listener 18781)"
[ -n "$pid" ] && kill -INT "$pid"
wait_or_kill "$lldb_pid" 120
cat "$out/B-driver.log"
grep -E "ALLOC-ONCE-FIRST|NOTIFY-CALL #1 |FORK #|POSIX-SPAWN #|Process .* exited" "$out/B-gateway.log" | head -60

echo "== C: worker (under LLDB) behind a gateway connecting with --runner-socket =="
sockdir="$(mktemp -d /tmp/fdsock.XXXXXX)"
sock="$sockdir/runner.sock"
{
  echo "target create \"$worker\""
  breakpoints
  echo "process launch -i /dev/null -o $out/C-worker.stdout -e $out/C-worker.stderr -- $sock"
} > "$out/C-worker.lldb"
lldb --batch -s "$out/C-worker.lldb" > "$out/C-worker.log" 2>&1 &
lldb_pid=$!
for _ in $(seq 1 1200); do [ -S "$sock" ] && break; sleep 0.1; done
echo "worker socket: $(ls -l "$sock" 2>&1)"
"$gateway" --http --port 18782 --config "$out/registry.json" --runner uds --runner-socket "$sock" \
  > "$out/C-gateway.stdout" 2> "$out/C-gateway.stderr" &
gw=$!
python3 "$here/mcp_driver.py" 18782 demo > "$out/C-driver.log" 2>&1
kill -INT "$gw" 2>/dev/null
wait_or_kill "$gw" 30
wait_or_kill "$lldb_pid" 120
cat "$out/C-driver.log"
grep -E "PTRACE|ALLOC-ONCE-FIRST|NOTIFY-CALL #1 |FORK #|POSIX-SPAWN #|Process .* exited" "$out/C-worker.log" | head -60

echo "== D: gateway (under LLDB) spawning its worker with --runtime-bin =="
{
  echo "target create \"$gateway\""
  breakpoints
  echo "process launch -i /dev/null -o $out/D-gateway.stdout -e $out/D-gateway.stderr -- --http --port 18783 --config $out/registry.json --runner uds --runtime-bin $worker"
} > "$out/D-gateway.lldb"
lldb --batch -s "$out/D-gateway.lldb" > "$out/D-gateway.log" 2>&1 &
lldb_pid=$!
python3 "$here/mcp_driver.py" 18783 demo > "$out/D-driver.log" 2>&1
pid="$(listener 18783)"
[ -n "$pid" ] && kill -INT "$pid"
wait_or_kill "$lldb_pid" 120
cat "$out/D-driver.log"
grep -E "ALLOC-ONCE-FIRST|NOTIFY-CALL #1 |FORK #|POSIX-SPAWN #|Process .* exited" "$out/D-gateway.log" | head -60

echo "== E: stress, ${TRIALS:-100} trials =="
CODESPACE_FORK_RACE_OUT="$out/stress" "$harness" --trials "${TRIALS:-100}" > "$out/E-stress.log" 2>&1
echo "stress exit: $?"
tail -5 "$out/E-stress.log"

echo "== crash reports written during this job =="
mkdir -p "$out/diagnostic-reports"
for dir in "$HOME/Library/Logs/DiagnosticReports" /Library/Logs/DiagnosticReports; do
  [ -d "$dir" ] || continue
  for report in "$dir"/*.ips "$dir"/*.crash; do
    [ -f "$report" ] || continue
    if [ "$(stat -f %m "$report")" -ge "$started" ]; then
      cp "$report" "$out/diagnostic-reports/"
    fi
  done
done
echo "copied: $(ls "$out/diagnostic-reports" | wc -l)"
echo "naming _notify_fork_child: $(grep -l _notify_fork_child "$out"/diagnostic-reports/* 2>/dev/null | wc -l)"
exit 0
