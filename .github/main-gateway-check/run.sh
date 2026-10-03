#!/bin/bash
# Temporary: repeat on macOS the gateway DevGuard test command of the macos-core stage, which
# failed in main's post-merge CI run 37114872355, and print every failure to the job log.
# $1: the checkout to test; $2: how many runs.
set -u
cd "$1/crates/server"
runs="$2"
export CARGO_TARGET_DIR="$1/target/upstream-validation"
export PIN_ONLY=1
gateway=(cargo test --locked -p codespace-server --features devguard --lib)

"${gateway[@]}" --no-run > ../../build.log 2>&1 || { tail -30 ../../build.log; exit 1; }
echo "== Tests the filter selects"
"${gateway[@]}" -- devguard --list 2>/dev/null | grep ': test$'

echo "== $runs runs of: ${gateway[*]} -- devguard"
: > ../../failed-tests.txt
fails=0
for i in $(seq 1 "$runs"); do
  if ! "${gateway[@]}" -- devguard > ../../run.log 2>&1; then
    fails=$((fails + 1))
    echo "-- run $i failed:"
    sed -n '/^failures:$/,$p' ../../run.log
    grep -E '^test .* \.\.\. FAILED$' ../../run.log | awk '{print $2}' >> ../../failed-tests.txt
  fi
done
echo "== $fails/$runs runs failed; failures per test:"
sort ../../failed-tests.txt | uniq -c

# The inheritable-socket test with k inheritable descriptors from 3 up, so that the test's socket
# lands on a different descriptor number in each run.
bin=$("${gateway[@]}" --no-run --message-format=json 2>/dev/null | python3 -c '
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message.get("executable") \
            and message["target"]["name"] == "codespace_server" and message["profile"]["test"]:
        print(message["executable"])')
test=devguard::tests::the_child_check_sees_an_inheritable_socket_that_codespace_spawners_exclude
echo "== $test with k inherited descriptors ($bin)"
for k in $(seq 0 14); do
  if bash -c 'k=$1; shift; for ((i = 3; i < 3 + k; i++)); do eval "exec $i</dev/null"; done; exec "$@"' \
      _ "$k" "$bin" --exact "$test" --test-threads=1 > ../../sweep.log 2>&1; then
    echo "k=$k: passed"
  else
    echo "k=$k: FAILED"
    sed -n '/^failures:$/,$p' ../../sweep.log
  fi
done
exit 0
