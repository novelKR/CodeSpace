//! Isolated Codex Linux sandbox helper. The runner wraps user argv with
//! this process. It is not an MCP tool. `run_main` is Linux-only; other
//! targets compile the binary but must not invoke the upstream entry
//! (it panics).

fn main() {
    #[cfg(target_os = "linux")]
    {
        codex_linux_sandbox::run_main();
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("codespace-linux-sandbox is only supported on Linux");
        std::process::exit(1);
    }
}
