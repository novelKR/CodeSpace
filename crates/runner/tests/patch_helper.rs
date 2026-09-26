//! The test helper reuses an exported `CODESPACE_PATCH_BIN` instead of building one.

#[test]
fn exported_helper_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("codespace-patch");
    std::fs::write(&bin, b"").unwrap();
    std::env::set_var("CODESPACE_PATCH_BIN", &bin);
    assert_eq!(codespace_runner::ensure_helper_for_tests(), bin);
}
