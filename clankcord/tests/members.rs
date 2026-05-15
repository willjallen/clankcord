use serde_json::json;

mod common;
use common::test_store;

#[tokio::test(flavor = "current_thread")]
async fn member_search_matches_spaced_name_to_camel_name() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store
        .upsert_discord_members(
            "guild",
            &[json!({
                "id": "284362763386617857",
                "username": "mysterymanchien",
                "global_name": "MysteryManChien",
                "nick": null,
                "display_name": "MysteryManChien"
            })],
        )
        .await
        .unwrap();

    let members = store
        .search_discord_members("guild", "Mystery Man Chien", 10)
        .await
        .unwrap();

    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["id"], json!("284362763386617857"));
    assert_eq!(members[0]["score"], json!(1.0));
}

#[tokio::test(flavor = "current_thread")]
async fn ambiguous_member_queries_remain_ranked_candidates() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store
        .upsert_discord_members(
            "guild",
            &[
                json!({"id": "1", "global_name": "MysteryManChien"}),
                json!({"id": "2", "global_name": "MysteryGuest"}),
            ],
        )
        .await
        .unwrap();

    let members = store
        .search_discord_members("guild", "mystery", 10)
        .await
        .unwrap();

    assert_eq!(members.len(), 2);
    assert!(
        members
            .iter()
            .all(|member| member["score"].as_f64().unwrap() > 0.0)
    );
}
