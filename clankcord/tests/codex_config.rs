use clankcord::config::{AppConfig, CodexReasoningEffort};

#[test]
fn codex_invocation_options_are_loaded_from_config_toml() {
    let config =
        toml::from_str::<AppConfig>(include_str!("../../config.ex.toml")).expect("config parses");

    assert_eq!(config.codex.model, "gpt-6-astra");
    assert_eq!(config.codex.reasoning_effort, CodexReasoningEffort::High);
    assert_eq!(config.codex.reasoning_effort.as_str(), "high");
    assert!(!config.codex.fast_mode);
    assert!(config.codex.linear_mcp.enabled);
    assert_eq!(config.codex.linear_mcp.url, "https://mcp.linear.app/mcp");
}

#[test]
fn codex_reasoning_effort_and_fast_mode_are_typed_config_values() {
    let config_text = include_str!("../../config.ex.toml")
        .replace(
            "reasoning_effort = \"high\"",
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
        .replace("model = \"gpt-6-astra\"", "task_model = \"gpt-6-astra\"");

    let error =
        toml::from_str::<AppConfig>(&config_text).expect_err("config must reject task_model");

    assert!(error.to_string().contains("unknown field `task_model`"));
}

#[test]
fn codex_linear_mcp_config_is_required() {
    let config_text = include_str!("../../config.ex.toml").replace(
        r#"
[codex.linear_mcp]
enabled = true
url = "https://mcp.linear.app/mcp"
"#,
        "",
    );

    let error =
        toml::from_str::<AppConfig>(&config_text).expect_err("config must require linear_mcp");

    assert!(error.to_string().contains("missing field `linear_mcp`"));
}

#[test]
fn stale_codex_linear_mcp_api_key_config_is_rejected() {
    let config_text = include_str!("../../config.ex.toml").replace(
        "url = \"https://mcp.linear.app/mcp\"",
        "url = \"https://mcp.linear.app/mcp\"\napi_key_secret = \"stale-token-secret\"",
    );

    let error = toml::from_str::<AppConfig>(&config_text)
        .expect_err("config must reject bearer-token Linear authentication");

    assert!(error.to_string().contains("unknown field `api_key_secret`"));
}

#[test]
fn invalid_codex_reasoning_effort_is_rejected() {
    let config_text = include_str!("../../config.ex.toml").replace(
        "reasoning_effort = \"high\"",
        "reasoning_effort = \"minimal\"",
    );

    let error =
        toml::from_str::<AppConfig>(&config_text).expect_err("config must reject invalid effort");

    assert!(error.to_string().contains("unknown variant `minimal`"));
}
