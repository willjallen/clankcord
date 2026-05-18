use clankcord::config::{AppConfig, CodexReasoningEffort};

#[test]
fn codex_invocation_options_are_loaded_from_config_toml() {
    let config =
        toml::from_str::<AppConfig>(include_str!("../../config.ex.toml")).expect("config parses");

    assert_eq!(config.codex.model, "gpt-5.5");
    assert_eq!(config.codex.reasoning_effort, CodexReasoningEffort::Medium);
    assert_eq!(config.codex.reasoning_effort.as_str(), "medium");
    assert!(!config.codex.fast_mode);
}

#[test]
fn codex_reasoning_effort_and_fast_mode_are_typed_config_values() {
    let config_text = include_str!("../../config.ex.toml")
        .replace(
            "reasoning_effort = \"medium\"",
            "reasoning_effort = \"xhigh\"",
        )
        .replace("fast_mode = false", "fast_mode = true");

    let config = toml::from_str::<AppConfig>(&config_text).expect("config parses");

    assert_eq!(config.codex.reasoning_effort, CodexReasoningEffort::XHigh);
    assert_eq!(config.codex.reasoning_effort.as_str(), "xhigh");
    assert!(config.codex.fast_mode);
}

#[test]
fn stale_codex_task_model_key_is_rejected() {
    let config_text = include_str!("../../config.ex.toml")
        .replace("model = \"gpt-5.5\"", "task_model = \"gpt-5.5\"");

    let error =
        toml::from_str::<AppConfig>(&config_text).expect_err("config must reject task_model");

    assert!(error.to_string().contains("unknown field `task_model`"));
}

#[test]
fn invalid_codex_reasoning_effort_is_rejected() {
    let config_text = include_str!("../../config.ex.toml").replace(
        "reasoning_effort = \"medium\"",
        "reasoning_effort = \"minimal\"",
    );

    let error =
        toml::from_str::<AppConfig>(&config_text).expect_err("config must reject invalid effort");

    assert!(error.to_string().contains("unknown variant `minimal`"));
}
