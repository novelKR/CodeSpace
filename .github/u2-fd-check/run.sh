#!/bin/bash
# Temporary: on macOS, how many descriptors the gateway's DevGuard tests hold against the
# process limit, and whether D6's `CredentialUnavailable` probes follow the limit.
# $1: the checkout of #84's head; $2: runs per limit.
set -u
cd "$1"
runs="$2"
echo "== Limits: ulimit -n $(ulimit -n), ulimit -Hn $(ulimit -Hn)"
sysctl kern.maxfilesperproc kern.maxfiles
launchctl limit maxfiles

python3 scripts/validate-upstream.py macos-core --output target/upstream-reports/fd-check
echo "macos-core stage exit: $?"
target="$PWD/target/upstream-validation"
export CARGO_TARGET_DIR="$target"
export PIN_ONLY=1
export CODESPACE_PATCH_BIN="$target/debug/codespace-patch"
export CODESPACE_RUNTIME_BIN="$target/debug/codespace-codex-runtime"
export CODESPACE_LINUX_SANDBOX_BIN="$target/debug/codespace-linux-sandbox"
export CODESPACE_DEVGUARD_RUNTIME_BIN="$target/debug/codespace-codex-runtime-devguard"
export CODESPACE_DEVGUARD_FIXTURE_BIN="$target/debug/examples/fixture_authority"
export CODESPACE_REQUIRE_DEVGUARD_BINS=1

# Diagnostics in D6 only: the most descriptors open while it runs, and the count and limit when
# a probe reports anything but Unavailable.
python3 - crates/server/src/devguard.rs <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
def sub(old, new):
    # The first occurrence: D6's. A later U2 test also ends with its summary.
    global text
    assert text.count(old) >= 1, old
    text = text.replace(old, new, 1)
sub("""        let children = Arc::new(Children::new());

        // Each spawner's descriptors with no session being opened.""",
"""        let children = Arc::new(Children::new());
        let diag_stop = Arc::new(AtomicBool::new(false));
        let diag = {
            let stop = diag_stop.clone();
            std::thread::spawn(move || {
                let mut most = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    most = most.max(diag_open());
                    std::thread::sleep(Duration::from_millis(1));
                }
                most
            })
        };

        // Each spawner's descriptors with no session being opened.""")
sub("""                        let status = codespace_devguard::probe(&settings);
                        assert_eq!(status.state, State::Unavailable, "{status:?}");""",
"""                        let status = codespace_devguard::probe(&settings);
                        if status.state != State::Unavailable {
                            eprintln!("DIAG probe {status:?}: {} descriptors open, limit {}", diag_open(), diag_limit());
                        }
                        assert_eq!(status.state, State::Unavailable, "{status:?}");""")
sub("""        eprintln!("{summary}");
    }""", """        diag_stop.store(true, Ordering::Relaxed);
        eprintln!("DIAG d6: at most {} descriptors open, limit {}", diag.join().unwrap(), diag_limit());
        eprintln!("{summary}");
    }

    fn diag_open() -> usize {
        std::fs::read_dir("/dev/fd").map(|entries| entries.count()).unwrap_or(0)
    }

    fn diag_limit() -> u64 {
        extern "C" {
            fn getrlimit(resource: std::os::raw::c_int, limit: *mut [u64; 2]) -> std::os::raw::c_int;
        }
        let mut limit = [0u64; 2];
        let resource = if cfg!(target_os = "macos") { 8 } else { 7 };
        // SAFETY: getrlimit writes the two limits into `limit`.
        unsafe { getrlimit(resource, &mut limit) };
        limit[0]
    }""")
open(path, "w").write(text)
PY
gateway=(cargo test --locked -p codespace-server --features devguard --lib)
"${gateway[@]}" --no-run > ../build.log 2>&1 || { tail -30 ../build.log; exit 1; }

# repeat LABEL LIMIT: $runs runs under `ulimit -n LIMIT`
repeat() {
  local label="$1" limit="$2" fails=0 i
  echo "== $label: $runs runs with ulimit -S -n $limit"
  for i in $(seq 1 "$runs"); do
    if ! (ulimit -S -n "$limit" && "${gateway[@]}" -- devguard --nocapture) > ../run.log 2>&1; then
      fails=$((fails + 1))
      echo "-- run $i failed:"
      grep -E -A3 "panicked at" ../run.log | grep -v "^note:" | cut -c1-240 | head -12
    fi
    grep -E "^DIAG" ../run.log | cut -c1-240
  done
  echo "== $label: $fails/$runs runs failed"
}
repeat default "$(ulimit -n)"
raised=$(ulimit -Hn)
if [ "$raised" = unlimited ] || [ "$raised" -gt 10240 ]; then raised=10240; fi
repeat raised "$raised"
exit 0
