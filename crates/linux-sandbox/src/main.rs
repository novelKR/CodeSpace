//! Isolated Codex Linux sandbox helper. Binary-only: the runner talks
//! `probe` / `prepare` / `run` over this process. Codex argv never
//! leaves this binary. Not an MCP tool.

mod codex;
mod plan;
mod prepare;
mod probe;
mod proxy;

fn main() {
    let mut args = std::env::args();
    let _exe = args.next();
    match args.next().as_deref() {
        Some("probe") => probe::main(),
        Some("prepare") => prepare::main(),
        Some("run") => {
            let rest: Vec<String> = args.collect();
            run_plan(&rest);
        }
        _ => codex::run_main(),
    }
}

fn run_plan(args: &[String]) {
    let plan_path = match parse_plan_flag(args) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    match plan::load_and_unlink(&plan_path) {
        Ok(argv) => {
            if argv.iter().any(|arg| arg == "--allow-network-for-proxy") {
                proxy::run_codex_with_proxy(&argv);
            } else {
                codex::exec_self(&argv);
            }
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

fn parse_plan_flag(args: &[String]) -> Result<std::path::PathBuf, String> {
    match args {
        [flag, path] if flag == "--plan" && !path.is_empty() => Ok(std::path::PathBuf::from(path)),
        _ => Err("usage: codespace-linux-sandbox run --plan <path>".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_plan_flag;
    use std::path::PathBuf;

    #[test]
    fn crate_is_isolated_helper_binary() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-linux-sandbox");
    }

    #[test]
    fn run_requires_plan_flag() {
        assert!(parse_plan_flag(&[]).is_err());
        assert!(parse_plan_flag(&["--plan".into()]).is_err());
        assert_eq!(
            parse_plan_flag(&["--plan".into(), "/tmp/x.plan".into()]).unwrap(),
            PathBuf::from("/tmp/x.plan")
        );
    }
}
