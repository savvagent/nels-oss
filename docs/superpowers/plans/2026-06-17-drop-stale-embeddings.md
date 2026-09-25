# Drop Stale Embeddings on Old Messages — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reclaim `chat_messages` storage by periodically nulling the `embedding` column on messages older than a retention window, while always keeping embeddings for the most recent N messages of each conversation.

**Architecture:** A new pure-config helper + an async purge function live in `backend/src/rag.rs`, mirroring `auth::purge_expired_auth_state`. The purge runs on the **existing** hourly background ticker in `backend/src/main.rs` — no new task. The purge is a single idempotent windowed `UPDATE` (the `embedding IS NOT NULL` guard means already-cleared rows are never touched again). `message_text` and all other columns are never modified, so transcripts and the last-8-message window are unaffected; only pgvector semantic recall of old messages in quiet threads is given up.

**Tech Stack:** Rust, `sqlx` (Postgres), `tokio`, `pgvector` (`vector(768)` column, hnsw cosine index), config via `std::env::var`.

**Configuration (env vars, read once at startup):**
- `EMBEDDING_RETENTION_DAYS` — default `90`, clamped to `>= 1`
- `EMBEDDING_RETENTION_KEEP_RECENT` — default `50`, clamped to `>= 0`

**Schema note (verified):** `chat_messages.conversation_id` is nullable but is backfilled by migration `20260610010000_conversations.sql` and is always set on every INSERT (`rag.rs:1393` and `rag.rs:1415`). `PARTITION BY conversation_id` therefore groups real threads correctly; any stray legacy NULLs collapse into a single partition group, which is acceptable (worst case those rows keep their embeddings). **No schema migration is required** — this change only writes NULL into an existing nullable column.

**Testing strategy:** The repo currently has *no* DB-touching test harness (tests are inline `#[test]` logic-only units, e.g. `main.rs:246-283`). To stay consistent and still get real TDD value:
- The config-parsing logic is extracted into pure functions and unit-tested (always run under `cargo test`).
- The DB behavior is covered by an `#[ignore]`d async integration test that runs on demand against the local `docker-compose` Postgres (`cargo test -- --ignored`). It seeds its own isolated user/conversation with unique UUIDs and deletes them on completion, so it never pollutes real data and is not run in normal CI.

---

## File Structure

- **Modify** `backend/src/rag.rs` — add `DEFAULT_RETENTION_DAYS`/`DEFAULT_KEEP_RECENT` consts, `parse_retention_days`, `parse_keep_recent`, `embedding_retention_config`, `purge_stale_embeddings`, plus a `#[cfg(test)] mod` for unit + ignored integration tests.
- **Modify** `backend/src/main.rs:88-97` — read retention config before the spawn, and call `rag::purge_stale_embeddings` inside the existing hourly loop.

---

### Task 1: Config-parsing helpers (pure, unit-tested)

**Files:**
- Modify: `backend/src/rag.rs` (add consts + functions near the top of the file, after the existing `use`/const declarations)
- Test: `backend/src/rag.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests**

Add (or extend, if a `#[cfg(test)] mod tests` already exists) at the bottom of `backend/src/rag.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_days_defaults_when_absent() {
        assert_eq!(parse_retention_days(None), 90);
    }

    #[test]
    fn retention_days_defaults_when_unparseable() {
        assert_eq!(parse_retention_days(Some("abc".to_string())), 90);
    }

    #[test]
    fn retention_days_defaults_when_below_minimum() {
        // 0 and negatives are nonsensical -> fall back to default
        assert_eq!(parse_retention_days(Some("0".to_string())), 90);
        assert_eq!(parse_retention_days(Some("-5".to_string())), 90);
    }

    #[test]
    fn retention_days_parses_valid_value() {
        assert_eq!(parse_retention_days(Some("30".to_string())), 30);
        assert_eq!(parse_retention_days(Some("  180 ".to_string())), 180);
    }

    #[test]
    fn keep_recent_defaults_when_absent_or_invalid() {
        assert_eq!(parse_keep_recent(None), 50);
        assert_eq!(parse_keep_recent(Some("nope".to_string())), 50);
        assert_eq!(parse_keep_recent(Some("-1".to_string())), 50);
    }

    #[test]
    fn keep_recent_allows_zero_and_parses_valid() {
        // 0 is meaningful: drop embeddings on ALL old messages
        assert_eq!(parse_keep_recent(Some("0".to_string())), 0);
        assert_eq!(parse_keep_recent(Some("10".to_string())), 10);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd backend && cargo test parse_ retention_ keep_recent`
Expected: FAIL to **compile** — `cannot find function parse_retention_days`/`parse_keep_recent` in this scope.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `backend/src/rag.rs` (after existing top-level `const`/`use` lines):

```rust
/// Messages older than this (in days) are eligible to have their embedding
/// dropped, unless protected by the per-conversation keep-recent floor.
const DEFAULT_RETENTION_DAYS: i64 = 90;
/// Always retain embeddings for the most recent N messages of each
/// conversation, regardless of age, so active threads keep semantic recall.
const DEFAULT_KEEP_RECENT: i64 = 50;

/// Parse `EMBEDDING_RETENTION_DAYS`. Falls back to the default for missing,
/// unparseable, or non-positive values.
fn parse_retention_days(raw: Option<String>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&d| d >= 1)
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Parse `EMBEDDING_RETENTION_KEEP_RECENT`. Falls back to the default for
/// missing, unparseable, or negative values. Zero is allowed and means
/// "no per-conversation floor".
fn parse_keep_recent(raw: Option<String>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(DEFAULT_KEEP_RECENT)
}

/// Read embedding-retention configuration from the environment, returning
/// `(retention_days, keep_recent)`.
pub fn embedding_retention_config() -> (i64, i64) {
    (
        parse_retention_days(std::env::var("EMBEDDING_RETENTION_DAYS").ok()),
        parse_keep_recent(std::env::var("EMBEDDING_RETENTION_KEEP_RECENT").ok()),
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test parse_ retention_ keep_recent`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(retention): add embedding-retention config parsing"
```

---

### Task 2: `purge_stale_embeddings` function + ignored DB integration test

**Files:**
- Modify: `backend/src/rag.rs` (add the async function after `embedding_retention_config`)
- Test: `backend/src/rag.rs` (add the ignored async test to the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing (ignored) integration test**

Add inside the `#[cfg(test)] mod tests` block in `backend/src/rag.rs`:

```rust
// Runs only on demand against the local docker-compose Postgres:
//   docker compose up -d db
//   cd backend && cargo test -- --ignored
// Seeds an isolated user/conversation with unique UUIDs and cleans up after.
#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn purge_stale_embeddings_drops_old_keeps_recent_and_text() {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nels:nels@localhost:5432/nels".to_string());
    let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

    let user_id = uuid::Uuid::new_v4();
    let convo_id = uuid::Uuid::new_v4();

    // Seed an isolated user + conversation (FKs require both to exist).
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
        .bind(user_id)
        .bind(format!("retention-{user_id}@example.test"))
        .execute(&pool)
        .await
        .expect("seed user");
    sqlx::query("INSERT INTO conversations (id, user_id, title) VALUES ($1, $2, 'retention test')")
        .bind(convo_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed conversation");

    // Helper to insert one embedded message at a chosen age (days ago).
    // A 768-dim zero vector satisfies the vector(768) column.
    async fn seed_msg(pool: &sqlx::PgPool, convo: uuid::Uuid, user: uuid::Uuid,
                      text: &str, age_days: i64) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO chat_messages
                (id, user_id, budget_id, conversation_id, sender, message_text, embedding, created_at)
             VALUES ($1, $2, NULL, $3, 'user', $4,
                     array_fill(0::real, ARRAY[768])::vector,
                     NOW() - make_interval(days => $5::int))",
        )
        .bind(id).bind(user).bind(convo).bind(text).bind(age_days as i32)
        .execute(pool).await.expect("seed message");
        id
    }

    // 4 old messages (200 days) + 1 recent (1 day). With keep_recent = 2,
    // the 2 newest of the conversation are protected regardless of age.
    let old1 = seed_msg(&pool, convo_id, user_id, "old-1", 200).await;
    let old2 = seed_msg(&pool, convo_id, user_id, "old-2", 199).await;
    let old3 = seed_msg(&pool, convo_id, user_id, "old-3", 198).await; // protected (within recent 2)
    let recent = seed_msg(&pool, convo_id, user_id, "recent", 1).await; // protected (within recent 2)
    let _ = (old2, old3); // ordering documented below

    // Ranked newest-first: recent(rn1), old3(rn2), old2(rn3), old1(rn4).
    // keep_recent=2 protects rn<=2 (recent, old3). days=90 drops old2 & old1.
    let cleared = purge_stale_embeddings(&pool, 90, 2).await.expect("purge");

    // Assertion helpers.
    async fn has_embedding(pool: &sqlx::PgPool, id: uuid::Uuid) -> bool {
        sqlx::query_scalar::<_, bool>("SELECT embedding IS NOT NULL FROM chat_messages WHERE id = $1")
            .bind(id).fetch_one(pool).await.expect("fetch embedding state")
    }
    async fn text_of(pool: &sqlx::PgPool, id: uuid::Uuid) -> String {
        sqlx::query_scalar::<_, String>("SELECT message_text FROM chat_messages WHERE id = $1")
            .bind(id).fetch_one(pool).await.expect("fetch text")
    }

    assert_eq!(cleared, 2, "exactly old-1 and old-2 should be cleared");
    assert!(!has_embedding(&pool, old1).await, "old-1 embedding dropped");
    assert!(has_embedding(&pool, recent).await, "recent embedding retained");
    assert!(has_embedding(&pool, old3).await, "old-3 retained by keep_recent floor");
    assert_eq!(text_of(&pool, old1).await, "old-1", "message_text never altered");

    // Idempotency: a second run clears nothing.
    let cleared_again = purge_stale_embeddings(&pool, 90, 2).await.expect("purge again");
    assert_eq!(cleared_again, 0, "second run is a no-op");

    // Cleanup: deleting the user cascades to conversation + messages.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id).execute(&pool).await.expect("cleanup");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test purge_stale_embeddings_drops_old_keeps_recent_and_text -- --ignored`
Expected: FAIL to **compile** — `cannot find function purge_stale_embeddings in this scope`.
(If `uuid` is not already a dependency, the error will instead name `uuid`; confirm with `grep '^uuid' backend/Cargo.toml`. It is used pervasively in `rag.rs`, so it is already present.)

- [ ] **Step 3: Write minimal implementation**

Add to `backend/src/rag.rs` immediately after `embedding_retention_config`:

```rust
/// Null out embeddings on messages older than `days` that are NOT among the
/// most recent `keep_recent` messages of their conversation. Idempotent: the
/// `embedding IS NOT NULL` guard means already-cleared rows are never rewritten,
/// so re-running affects zero rows. Only the `embedding` column is touched —
/// `message_text` and all metadata are preserved. Returns the rows updated.
pub async fn purge_stale_embeddings(
    db: &sqlx::PgPool,
    days: i64,
    keep_recent: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE chat_messages SET embedding = NULL
        WHERE id IN (
            SELECT id FROM (
                SELECT id,
                       row_number() OVER (
                           PARTITION BY conversation_id ORDER BY created_at DESC
                       ) AS rn,
                       created_at
                FROM chat_messages
                WHERE embedding IS NOT NULL
            ) ranked
            WHERE ranked.rn > $2
              AND ranked.created_at < NOW() - make_interval(days => $1::int)
        )
        "#,
    )
    .bind(days as i32)
    .bind(keep_recent)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
```

- [ ] **Step 4: Run the integration test to verify it passes**

Ensure the DB is up: `docker compose up -d db` (from repo root).
Run: `cd backend && cargo test purge_stale_embeddings_drops_old_keeps_recent_and_text -- --ignored`
Expected: PASS (1 test). Also confirm the normal suite still passes and skips the ignored test: `cargo test` → reports `... 1 ignored`.

- [ ] **Step 5: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(retention): add purge_stale_embeddings windowed purge"
```

---

### Task 3: Wire the purge into the hourly background ticker

**Files:**
- Modify: `backend/src/main.rs:88-97`

- [ ] **Step 1: Read the current ticker block**

Confirm the current code at `backend/src/main.rs:88-97`:

```rust
    // 6. Background task: hourly purge of expired sessions / auth flows. The
    //    first tick fires immediately, cleaning up any leftovers on startup.
    let cleanup_pool = state.db.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            auth::purge_expired_auth_state(&cleanup_pool).await;
        }
    });
```

- [ ] **Step 2: Add the embedding purge to the loop**

Replace that block with:

```rust
    // 6. Background task: hourly purge of expired sessions / auth flows and
    //    stale chat embeddings. The first tick fires immediately, cleaning up
    //    any leftovers on startup.
    let cleanup_pool = state.db.clone();
    let (retention_days, keep_recent) = rag::embedding_retention_config();
    tracing::info!(
        retention_days,
        keep_recent,
        "Embedding retention configured"
    );
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            auth::purge_expired_auth_state(&cleanup_pool).await;
            match rag::purge_stale_embeddings(&cleanup_pool, retention_days, keep_recent).await {
                Ok(n) if n > 0 => tracing::info!(cleared = n, "Purged stale chat embeddings"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to purge stale chat embeddings"),
            }
        }
    });
```

- [ ] **Step 3: Verify it compiles**

Run: `cd backend && cargo build`
Expected: builds cleanly. (If `rag` is not in scope in `main.rs`, confirm the existing `mod rag;` / `use` declaration — `rag` functions like the chat handler are already routed from `main.rs`, so the module is in scope.)

- [ ] **Step 4: Run the full test suite**

Run: `cd backend && cargo test`
Expected: PASS, with the integration test reported as ignored.

- [ ] **Step 5: Commit**

```bash
git add backend/src/main.rs
git commit -m "feat(retention): purge stale embeddings on the hourly ticker"
```

---

### Task 4: Document the new env vars

**Files:**
- Modify: `backend/.env.example` if it exists (check with `ls backend/.env.example`); otherwise the project README that lists env vars (find with `grep -rl "GEMINI_API_KEY" backend/*.md README.md docs 2>/dev/null`).

- [ ] **Step 1: Locate the canonical env-var documentation**

Run: `ls backend/.env.example 2>/dev/null; grep -rl "GEMINI_API_KEY" backend README.md docs 2>/dev/null`
Use the file that documents env vars (e.g. `backend/.env.example`). If no such file exists, skip to Step 3 and document in the deploy topology notes instead.

- [ ] **Step 2: Add the two variables with their defaults**

Append to the located env file (matching its existing comment style):

```bash
# Embedding retention: drop pgvector embeddings on messages older than this many
# days, keeping readable transcript text. Default 90.
EMBEDDING_RETENTION_DAYS=90
# Always keep embeddings for the most recent N messages of each conversation,
# regardless of age, preserving semantic recall in active threads. Default 50.
EMBEDDING_RETENTION_KEEP_RECENT=50
```

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "docs(retention): document embedding-retention env vars"
```

---

## Out of Scope (YAGNI)

- No schema migration, no `VACUUM FULL` in the hot path — disk is reclaimed by autovacuum; the hnsw index naturally excludes NULL rows.
- No partial index `WHERE embedding IS NOT NULL` — the existing `idx_chat_messages_conversation (conversation_id, created_at)` covers the window scan; add one only if profiling shows the hourly purge is slow at scale.
- No retention for `audit_logs` / `notifications` — tracked separately; this plan covers only chat embeddings.
- No backfill/one-shot CLI — the hourly ticker fires on startup, so existing stale embeddings are cleared on the next deploy automatically.
