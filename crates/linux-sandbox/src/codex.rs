//! Linux `codex_linux_sandbox::run_main`. Non-Linux compiles this module
//! but must not call the upstream entry (it panics). After `run --plan`,
//! this process `exec`s itself with Codex argv so the managed PID is
//! unchanged.

use std::path::PathBuf;

pub fn run_main() {
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

/// Replace this process with the same binary and `argv` (Codex flags).
/// Does not spawn an extra child. Inherits the env the runner applied.
pub fn exec_self(argv: &[String]) -> ! {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codespace-linux-sandbox"));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(exe).args(argv).exec();
        eprintln!("exec linux sandbox helper: {err}");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let _ = argv;
        eprintln!("codespace-linux-sandbox exec is Unix-only");
        std::process::exit(1);
    }
}
