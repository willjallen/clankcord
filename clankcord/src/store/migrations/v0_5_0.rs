use crate::Result;

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    sqlx::query("DROP INDEX IF EXISTS idx_jobs_terminal_retention")
        .execute(transaction.as_mut())
        .await?;
    Ok(())
}
