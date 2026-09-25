//! One-shot backfill of semantic embeddings for transactions created before the
//! embedding-on-insert path existed (#195).
//!
//! Idempotent: only rows with `embedding IS NULL` are considered, so re-running
//! after a full pass updates zero rows and returns 0. Tolerant per row: an
//! embedding the API declines to produce (or a failed UPDATE) is logged and
//! skipped, leaving that row NULL for a later run — one bad row never aborts the
//! whole backfill. Returns the total number of rows whose embedding was filled.
//!
//! nels-oss#3: each row is embedded with its budget OWNER's resolved provider.
//! Only owners whose provider can embed into the shared 768-d Gemini space are
//! selected: Nels-hosted owners (no `user_ai_providers` row) when
//! `llm::nels_gemini_key()` is set, and BYO Gemini owners always (with their own key). BYO
//! OpenAI/Anthropic owners are skipped — their rows stay NULL (spec A6), and a
//! BYO key never falls back to the Nels key.

use std::collections::HashMap;

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Batch size per SELECT. Each iteration grabs up to this many still-NULL rows,
/// embeds them, and writes the results before fetching the next batch.
const BATCH_SIZE: i64 = 100;

/// Eligibility predicate shared by the batch SELECT and the final remaining-NULL
/// count: a Nels-hosted owner (no BYO row) when Nels has a Gemini key (`$flag`),
/// or a BYO Gemini owner. `{flag}` is substituted with the bind placeholder.
fn eligible_owner_clause(flag: &str) -> String {
    format!("((p.user_id IS NULL AND {flag}) OR p.provider = 'gemini')")
}

pub async fn backfill_transaction_embeddings(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
) -> Result<usize, sqlx::Error> {
    // Whether Nels-hosted owners can be embedded at all this run.
    let nels_can_embed = !crate::llm::nels_gemini_key().is_empty();

    let mut total_updated: usize = 0;

    // Per-run cache of each owner's resolved provider, so a batch of one owner's
    // rows resolves (and decrypts) once rather than per row.
    let mut resolved_cache: HashMap<Uuid, crate::llm::Resolved> = HashMap::new();

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
        //
        // Bind numbering differs per variant and must match the bind order:
        // no-cursor binds (LIMIT=$1, flag=$2); cursor binds (LIMIT=$1, cursor=$2,
        // flag=$3). The LEFT JOIN is 1:1 (user_ai_providers.user_id is the PK).
        let rows = match last_id {
            None => {
                let sql = format!(
                    "SELECT t.id, t.description, b.owner_id \
                     FROM transactions t \
                     JOIN budgets b ON b.id = t.budget_id \
                     LEFT JOIN user_ai_providers p ON p.user_id = b.owner_id \
                     WHERE t.embedding IS NULL AND {} \
                     ORDER BY t.id \
                     LIMIT $1 \
                     FOR UPDATE OF t SKIP LOCKED",
                    eligible_owner_clause("$2")
                );
                sqlx::query(&sql)
                    .bind(BATCH_SIZE)
                    .bind(nels_can_embed)
                    .fetch_all(&mut *txn)
                    .await?
            }
            Some(id) => {
                let sql = format!(
                    "SELECT t.id, t.description, b.owner_id \
                     FROM transactions t \
                     JOIN budgets b ON b.id = t.budget_id \
                     LEFT JOIN user_ai_providers p ON p.user_id = b.owner_id \
                     WHERE t.embedding IS NULL AND {} AND t.id > $2 \
                     ORDER BY t.id \
                     LIMIT $1 \
                     FOR UPDATE OF t SKIP LOCKED",
                    eligible_owner_clause("$3")
                );
                sqlx::query(&sql)
                    .bind(BATCH_SIZE)
                    .bind(id)
                    .bind(nels_can_embed)
                    .fetch_all(&mut *txn)
                    .await?
            }
        };

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

            if !resolved_cache.contains_key(&owner_id) {
                let ai = crate::llm::resolve_for_user(pool, cipher, owner_id)
                    .await
                    .unwrap_or(crate::llm::Resolved::Offline);
                resolved_cache.insert(owner_id, ai);
            }
            let ai = &resolved_cache[&owner_id];

            let embedding =
                match crate::llm::embed_for(pool, owner_id, ai, &description).await {
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
    // Only rows this run was eligible to embed are counted: rows owned by a
    // BYO OpenAI/Anthropic user are NULL by design (spec A6), not "incomplete".
    let remaining_sql = format!(
        "SELECT COUNT(*) FROM transactions t \
         JOIN budgets b ON b.id = t.budget_id \
         LEFT JOIN user_ai_providers p ON p.user_id = b.owner_id \
         WHERE t.embedding IS NULL AND {}",
        eligible_owner_clause("$1")
    );
    match sqlx::query_scalar::<_, i64>(&remaining_sql)
        .bind(nels_can_embed)
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
            "backfill: finished; no eligible transactions left without an embedding"
        ),
        Err(e) => tracing::warn!(error = %e, "backfill: could not count remaining NULL embeddings"),
    }

    Ok(total_updated)
}

#[cfg(test)]
mod tests {
    // nels-oss#3: the backfill embeds a BYO Gemini owner's rows with that
    // owner's own key even when Nels has no GEMINI_API_KEY, and skips a BYO
    // OpenAI owner (whose provider cannot embed into the 768-d Gemini space).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn backfill_embeds_byo_gemini_owner_and_skips_byo_openai_owner() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        struct Restore(Vec<(&'static str, Option<String>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (k, v) in &self.0 {
                    match v {
                        Some(v) => std::env::set_var(k, v),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
        let _r = Restore(
            ["GEMINI_API_KEY", "GEMINI_API_BASE"]
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect(),
        );

        let server = wiremock::MockServer::start().await;
        std::env::remove_var("GEMINI_API_KEY");
        std::env::set_var("GEMINI_API_BASE", server.uri());
        wiremock::Mock::given(wiremock::matchers::header("x-goog-api-key", "gk"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": {"values": vec![0.0f32; 768]}
            })))
            .expect(1..)
            .mount(&server)
            .await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = crate::auth::test_cipher();

        // (owner, budget, transaction) for A (BYO openai) and B (BYO gemini).
        let mut seeded = Vec::new();
        for (provider, key) in [("openai", "sk-a"), ("gemini", "gk")] {
            let owner = uuid::Uuid::new_v4();
            let budget = uuid::Uuid::new_v4();
            let tx = uuid::Uuid::new_v4();
            sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
                .bind(owner)
                .bind(format!("backfill-{provider}-{owner}@example.test"))
                .execute(&pool)
                .await
                .expect("seed user");
            sqlx::query(
                "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
                 VALUES ($1, $2, 'backfill byo', 'monthly', 100.0, FALSE)",
            )
            .bind(budget)
            .bind(owner)
            .execute(&pool)
            .await
            .expect("seed budget");
            sqlx::query(
                "INSERT INTO user_ai_providers (user_id, provider, encrypted_key, key_last4, last_verified_at) \
                 VALUES ($1, $2, $3, 'last', NOW())",
            )
            .bind(owner)
            .bind(provider)
            .bind(cipher.encrypt(key).expect("encrypt"))
            .execute(&pool)
            .await
            .expect("seed provider");
            sqlx::query(
                "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
                 VALUES ($1, $2, NULL, 5.0, NOW(), 'needs embedding')",
            )
            .bind(tx)
            .bind(budget)
            .execute(&pool)
            .await
            .expect("seed transaction");
            seeded.push((owner, budget, tx));
        }

        super::backfill_transaction_embeddings(&pool, &cipher)
            .await
            .expect("backfill runs");

        let is_null = |tx: uuid::Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, bool>("SELECT embedding IS NULL FROM transactions WHERE id = $1")
                    .bind(tx)
                    .fetch_one(&pool)
                    .await
                    .expect("read embedding")
            }
        };
        let (_, _, tx_a) = seeded[0];
        let (_, _, tx_b) = seeded[1];
        assert!(!is_null(tx_b).await, "BYO gemini owner's row is embedded with their key");
        assert!(is_null(tx_a).await, "BYO openai owner's row stays NULL (spec A6)");

        for (owner, _, _) in &seeded {
            let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await;
        }
        // wiremock verifies .expect(1..) on the `gk` mock when `server` drops.
    }
}
