//! One-shot backfill of semantic embeddings for transactions created before the
//! embedding-on-insert path existed (#195).
//!
//! Idempotent: only rows with `embedding IS NULL` are considered, so re-running
//! after a full pass updates zero rows and returns 0. Tolerant per row: an
//! embedding the API declines to produce (or a failed UPDATE) is logged and
//! skipped, leaving that row NULL for a later run — one bad row never aborts the
//! whole backfill. Returns the total number of rows whose embedding was filled.

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Batch size per SELECT. Each iteration grabs up to this many still-NULL rows,
/// embeds them, and writes the results before fetching the next batch.
const BATCH_SIZE: i64 = 100;

pub async fn backfill_transaction_embeddings(
    pool: &PgPool,
    api_key: &str,
) -> Result<usize, sqlx::Error> {
    if api_key.is_empty() {
        tracing::warn!("backfill_transaction_embeddings: GEMINI_API_KEY is empty; nothing to do");
        return Ok(0);
    }

    let mut total_updated: usize = 0;

    // Cursor over `t.id`: each batch scans strictly past the last id seen so the
    // scan always moves forward. A batch where every row fails to embed still
    // advances the cursor (set below before the embed loop), so later still-NULL
    // rows are always reached; failed rows remain NULL and are retried by a
    // future run (idempotency preserved).
    let mut last_id: Option<Uuid> = None;

    loop {
        // Each batch runs in its own transaction so the row locks taken by the
        // SELECT are held through the matching UPDATEs and the commit. This makes
        // the backfill safe to run on two backend instances at once: FOR UPDATE OF
        // t SKIP LOCKED hands each instance a disjoint set of still-NULL rows, so
        // they never double-embed the same transaction (and never double-bill the
        // embedding API). Locking only `t` keeps the read-only JOIN to budgets from
        // taking pointless locks there.
        let mut txn = pool.begin().await?;

        // The owner_id is needed so llm_usage is attributed to the budget owner,
        // matching the insert path. Budget scoping via the JOIN keeps each
        // embedding tied to a real, owned budget. The cursor (t.id > $2) advances
        // the scan past rows already seen so a batch of all-failing rows cannot
        // wedge the loop.
        let query = match last_id {
            None => sqlx::query(
                "SELECT t.id, t.description, b.owner_id \
                 FROM transactions t \
                 JOIN budgets b ON b.id = t.budget_id \
                 WHERE t.embedding IS NULL \
                 ORDER BY t.id \
                 LIMIT $1 \
                 FOR UPDATE OF t SKIP LOCKED",
            )
            .bind(BATCH_SIZE),
            Some(id) => sqlx::query(
                "SELECT t.id, t.description, b.owner_id \
                 FROM transactions t \
                 JOIN budgets b ON b.id = t.budget_id \
                 WHERE t.embedding IS NULL AND t.id > $2 \
                 ORDER BY t.id \
                 LIMIT $1 \
                 FOR UPDATE OF t SKIP LOCKED",
            )
            .bind(BATCH_SIZE)
            .bind(id),
        };
        let rows = query.fetch_all(&mut *txn).await?;

        if rows.is_empty() {
            break;
        }

        // Advance the cursor BEFORE embedding so a batch where every row fails to
        // embed/update still moves forward (rows are ordered by id).
        last_id = rows.last().map(|row| row.get("id"));

        let mut updated_this_batch = 0usize;

        for row in &rows {
            let id: Uuid = row.get("id");
            let description: String = row.get("description");
            let owner_id: Uuid = row.get("owner_id");

            let embedding =
                match crate::rag::get_gemini_embedding(&description, api_key, pool, owner_id).await {
                    Some(v) => v,
                    None => {
                        tracing::warn!(
                            transaction_id = %id,
                            "backfill: no embedding produced; leaving NULL for a later run"
                        );
                        continue;
                    }
                };
            let embedding_str = crate::rag::vector_to_string(&embedding);

            match sqlx::query("UPDATE transactions SET embedding = $1::vector WHERE id = $2")
                .bind(&embedding_str)
                .bind(id)
                .execute(&mut *txn)
                .await
            {
                Ok(_) => {
                    updated_this_batch += 1;
                    total_updated += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        transaction_id = %id,
                        error = %e,
                        "backfill: failed to write embedding; leaving NULL for a later run"
                    );
                }
            }
        }

        // Commit to release the row locks and persist this batch's writes before
        // fetching the next set of rows.
        txn.commit().await?;

        // Report progress so a long initial backfill is observable in the logs.
        tracing::info!(
            updated_this_batch,
            total_updated,
            "backfill: completed a batch"
        );

        // Forward progress is guaranteed by the cursor (last_id), so the loop only
        // terminates when the SELECT returns no more still-NULL rows (handled by
        // the rows.is_empty() break above). A short batch is not a stop condition:
        // SKIP LOCKED can return fewer rows than BATCH_SIZE mid-scan.
    }

    // The cursor + SKIP LOCKED loop exits on the first empty batch, which cannot
    // distinguish "no NULL rows remain" from "the remaining NULL rows are locked
    // by a concurrent instance". Surface any rows still NULL so an incomplete run
    // (e.g. a row that never embeds, or work in flight on another instance) is
    // visible in the logs without manual inspection. Those rows are picked up by a
    // later run (idempotent). Best-effort: a failure here must not fail the job.
    match sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM transactions WHERE embedding IS NULL",
    )
    .fetch_one(pool)
    .await
    {
        Ok(remaining) if remaining > 0 => tracing::warn!(
            total_updated,
            remaining_null = remaining,
            "backfill: finished with rows still lacking an embedding; rerun to retry them"
        ),
        Ok(_) => tracing::info!(
            total_updated,
            "backfill: finished; no transactions left without an embedding"
        ),
        Err(e) => tracing::warn!(error = %e, "backfill: could not count remaining NULL embeddings"),
    }

    Ok(total_updated)
}
