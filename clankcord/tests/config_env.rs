use std::env;
use std::fs;

#[test]
fn codex_bypass_sandbox_uses_environment_override() {
    let tempdir = tempfile::tempdir().expect("create temp config dir");
    fs::write(
        tempdir.path().join("config.toml"),
        include_str!("../../config.ex.toml"),
    )
    .expect("write config");

    let original_dir = env::current_dir().expect("read current dir");
    env::set_current_dir(tempdir.path()).expect("enter temp config dir");
    unsafe {
        env::set_var("CLANKCORD_CODEX_BYPASS_SANDBOX", "true");
    }

    assert!(clankcord::config::codex_bypass_sandbox());

    env::set_current_dir(original_dir).expect("restore current dir");
}
