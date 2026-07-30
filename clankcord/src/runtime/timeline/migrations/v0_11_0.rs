use crate::Result;

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_timeline_recent_unforgotten
          ON timeline_events(started_at_ms DESC, sequence DESC, event_id DESC)
          WHERE forgotten = FALSE
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}
