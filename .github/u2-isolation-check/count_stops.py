# Temporary: make the two `past_the_credential` test helpers of #84 append a line naming the
# thread to $DIAG_STOPS each time a session stops at the credential and is run again.
import sys

OLD = """            if !stopped(&outcome) {
                return outcome;
            }
"""
NEW = OLD + """            if let Ok(path) = std::env::var("DIAG_STOPS") {
                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)
                    .unwrap();
                writeln!(file, "{}", std::thread::current().name().unwrap_or("?")).unwrap();
            }
"""

for path in sys.argv[1:]:
    text = open(path).read()
    assert text.count(OLD) == 1, path
    open(path, "w").write(text.replace(OLD, NEW))
    print(f"counting stops in {path}")
