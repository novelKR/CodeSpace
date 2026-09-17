//! Isolated Codex runtime adapter workspace. Product MCP stays in `crates/server`.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_isolated_adapter() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-codex-runtime");
    }
}
