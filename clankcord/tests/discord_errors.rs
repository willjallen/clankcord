use clankcord::errors::{
    DiscordToolError, discord_api_error, discord_error_channel_id,
    discord_error_is_unavailable_channel, discord_error_text_channel_id,
    discord_error_text_is_unavailable_channel,
};

#[test]
fn discord_channel_unavailable_errors_are_classified_by_code() {
    for code in [10003, 50001, 50013] {
        let error = discord_api_error(
            "POST",
            "/channels/150000000000000001/typing",
            if code == 10003 { 404 } else { 403 },
            format!(r#"{{"message":"channel unavailable","code":{code}}}"#),
        );

        assert!(discord_error_is_unavailable_channel(&error));
        assert_eq!(discord_error_channel_id(&error), "150000000000000001");
        let typed = error.downcast_ref::<DiscordToolError>().unwrap();
        assert_eq!(typed.discord_code(), Some(code));
    }
}

#[test]
fn persisted_discord_channel_errors_are_classified_from_text() {
    let text = "discord api POST /channels/150000000000000001/messages failed (404): {\"message\":\"Unknown Channel\",\"code\":10003}";

    assert!(discord_error_text_is_unavailable_channel(text));
    assert_eq!(discord_error_text_channel_id(text), "150000000000000001");
}
