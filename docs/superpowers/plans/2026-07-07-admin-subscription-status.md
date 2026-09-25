# Admin subscription status column Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface each user's subscription/Pro status in the admin console's Users table, sourced from the existing `subscriptions` table via a small addition to the existing `GET /api/admin/users` endpoint — no new endpoint, schema, or migration.

**Architecture:** `backend/src/admin.rs`'s `list_users` query gains a second `LEFT JOIN` (against `subscriptions`, alongside its existing `llm_usage` join) and returns two new fields (`subscription_status`, `is_pro`) computed via the existing `billing::user_is_pro` helper. `admin/src/lib/UsersTable.svelte` appends one new sortable "Subscription" column rendering a daisyUI badge.

**Tech Stack:** Rust (`axum`, `sqlx`/Postgres) backend; Svelte 5 + daisyUI v5 admin frontend.

Ref: savvagent/nels#340. Spec: `docs/superpowers/specs/2026-07-07-admin-subscription-status-design.md`.

---

### Task 1: Backend — join `subscriptions` into the admin users query

**Files:**
- Modify: `backend/src/admin.rs:1-107` (imports, `AdminUserRow`, `list_users`)
- Modify: `backend/src/admin.rs:225-409` (extend existing test `list_users_aggregates_usage_and_activity`)

- [ ] **Step 1: Write the failing test (extend the existing Postgres-backed test)**

Open `backend/src/admin.rs` and locate `list_users_aggregates_usage_and_activity` (starts at line 229 in the current file). Add subscription seeding for `user_a` right after the `chat_messages`/`sessions` seeding block (after the future-session insert, before the `list_users(...)` call), and add assertions after the existing `row_a`/`row_b` assertions. The full modified test body (only the additive parts shown — insert them at the marked points, everything else in the test stays unchanged):

Insert this seeding block immediately before `let resp = list_users(State(state.clone())).await`:

```rust
        // Seed a `trialing` subscription for user_a only; user_b stays a free user
        // (no subscriptions row) to exercise the LEFT JOIN's NULL arm.
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) \
             VALUES ($1, $2, $3)",
        )
        .bind(user_a)
        .bind(format!("cus_test_{user_a}"))
        .bind("trialing")
        .execute(&pool)
        .await
        .expect("seed subscription for user_a");
```

Insert this assertion block immediately after the existing `assert_eq!(row_b.total_tokens, 0, ...)` / `first_activity <= last_activity` assertions for `row_b`, before the "Cleanup" comment:

```rust
        assert_eq!(
            row_a.subscription_status,
            Some("trialing".to_string()),
            "user_a has a trialing subscription"
        );
        assert!(row_a.is_pro, "trialing status must be Pro-entitled");
        assert_eq!(
            row_b.subscription_status, None,
            "user_b has no subscriptions row"
        );
        assert!(!row_b.is_pro, "no subscription must not be Pro-entitled");
```

Add a subscription cleanup line to the existing cleanup block (alongside the existing `llm_usage`/`chat_messages`/`sessions`/`users` deletes), placed BEFORE the `users` delete (FK references `users(id)`):

```rust
        let _ = sqlx::query("DELETE FROM subscriptions WHERE user_id = ANY($1)")
            .bind(vec![user_a, user_b])
            .execute(&pool)
            .await;
```

- [ ] **Step 2: Run test to verify it fails**

Ensure Postgres is running first: `podman-compose up -d` (from repo root).

Run: `cd backend && cargo test admin::tests::list_users_aggregates_usage_and_activity -- --ignored`
Expected: FAIL to compile — `AdminUserRow` has no field `subscription_status`/`is_pro` yet.

- [ ] **Step 3: Add `subscription_status`/`is_pro` to `AdminUserRow` and join `subscriptions` in the query**

In `backend/src/admin.rs`, add the import near the top (alongside the existing `use crate::auth::AppState;`):

```rust
use crate::billing::user_is_pro;
```

Replace the `AdminUserRow` struct (currently lines 51-63) with:

```rust
#[derive(serde::Serialize, sqlx::FromRow)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub thinking_tokens: i64,
    pub total_tokens: i64,
    pub first_activity: chrono::DateTime<chrono::Utc>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
    // Raw Stripe subscription status (#25's `subscriptions` table), NULL when the
    // user has never started checkout. Surfaced as-is for admin support triage
    // (distinguishing "never subscribed" from "lapsed"), not collapsed to a bool.
    pub subscription_status: Option<String>,
    // Derived entitlement flag. NOT selected by SQL — computed in Rust via
    // `billing::user_is_pro` after the fetch (see `list_users`) so the
    // trialing/active entitlement rule lives in exactly one place in the
    // codebase. Always overwritten before the row is returned; the `#[sqlx(skip)]`
    // default keeps `#[derive(sqlx::FromRow)]` mechanical for every other field.
    #[sqlx(skip)]
    pub is_pro: bool,
}
```

Replace the `list_users` function body's query + return (currently lines 67-107) with:

```rust
pub async fn list_users(
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminUserRow>>, StatusCode> {
    let mut rows = sqlx::query_as::<_, AdminUserRow>(
        r#"
        SELECT
          u.id, u.email, u.name, u.created_at,
          COALESCE(usage.input_tokens, 0)::bigint    AS input_tokens,
          COALESCE(usage.output_tokens, 0)::bigint   AS output_tokens,
          COALESCE(usage.thinking_tokens, 0)::bigint AS thinking_tokens,
          COALESCE(usage.total_tokens, 0)::bigint    AS total_tokens,
          LEAST(
            u.created_at,
            (SELECT MIN(cm.created_at) FROM chat_messages cm WHERE cm.user_id = u.id)
          ) AS first_activity,
          GREATEST(
            u.created_at,
            (SELECT MAX(cm.created_at) FROM chat_messages cm WHERE cm.user_id = u.id),
            (SELECT MAX(s.created_at)  FROM sessions s      WHERE s.user_id = u.id)
          ) AS last_activity,
          sub.status AS subscription_status
        FROM users u
        LEFT JOIN (
          SELECT user_id,
                 SUM(input_tokens)    AS input_tokens,
                 SUM(output_tokens)   AS output_tokens,
                 SUM(thinking_tokens) AS thinking_tokens,
                 SUM(total_tokens)    AS total_tokens
          FROM llm_usage GROUP BY user_id
        ) usage ON usage.user_id = u.id
        LEFT JOIN subscriptions sub ON sub.user_id = u.id
        ORDER BY u.created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("list_users query failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Derive `is_pro` from the raw status via the single source of truth
    // (billing::user_is_pro), rather than duplicating the trialing/active rule
    // in SQL — see AdminUserRow's `is_pro` doc comment.
    for row in rows.iter_mut() {
        row.is_pro = user_is_pro(row.subscription_status.as_deref());
    }

    Ok(Json(rows))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test admin::tests::list_users_aggregates_usage_and_activity -- --ignored`
Expected: PASS

Also run the other two existing admin tests to confirm nothing else broke:
Run: `cd backend && cargo test admin::tests -- --ignored`
Expected: PASS (3 tests: `admin_middleware_blocks_non_admin_allows_admin`, `list_users_aggregates_usage_and_activity`, `admin_middleware_rejects_missing_user_id_extension`)

- [ ] **Step 5: Run the full non-ignored suite + compile check**

Run: `cd backend && cargo check`
Expected: clean compile, no warnings about unused imports/fields.

Run: `cd backend && cargo test`
Expected: PASS (existing non-`--ignored` suite unaffected by this change).

- [ ] **Step 6: Commit**

```bash
git add backend/src/admin.rs
git commit -m "feat(#340): surface subscription status in admin users endpoint"
```

---

### Task 2: Frontend — render a sortable Subscription column with a status badge

**Files:**
- Modify: `admin/src/lib/UsersTable.svelte:1-157`

- [ ] **Step 1: Add the column definition**

In `admin/src/lib/UsersTable.svelte`, append a new entry to the `columns` array (after the existing `last_activity` entry, so it renders as the rightmost column — leaves a clean append point for a future billing-country/price column per the spec):

```js
  const columns = [
    { key: "email", label: "Email", type: "string" },
    { key: "name", label: "Name", type: "string" },
    { key: "created_at", label: "Created", type: "date" },
    { key: "input_tokens", label: "Input Tokens", type: "number" },
    { key: "output_tokens", label: "Output Tokens", type: "number" },
    { key: "total_tokens", label: "Total Tokens", type: "number" },
    { key: "first_activity", label: "First Activity", type: "date" },
    { key: "last_activity", label: "Last Activity", type: "date" },
    { key: "subscription_status", label: "Subscription", type: "string" },
  ];
```

- [ ] **Step 2: Add the badge helper**

Add this function near `fmtCell` (after it, before `async function load()`):

```js
  // Maps a user row to a { text, cls } badge for the Subscription column.
  // Pro (trialing/active, per billing::user_is_pro) renders green; a present
  // but non-entitled status (past_due, canceled, unpaid, etc.) renders amber
  // with the raw status text so support can distinguish "lapsed" from "never
  // subscribed"; no subscriptions row at all renders a neutral "Free".
  function subscriptionBadge(user) {
    if (user.is_pro) {
      return {
        text: user.subscription_status === "trialing" ? "Trialing" : "Pro",
        cls: "badge-success",
      };
    }
    if (user.subscription_status) {
      return { text: user.subscription_status, cls: "badge-warning" };
    }
    return { text: "Free", cls: "badge-neutral" };
  }
```

- [ ] **Step 3: Render the badge in the Subscription cell**

Replace the row-rendering block:

```svelte
          {#each sortedUsers as user (user.id ?? user.email)}
            <tr>
              {#each columns as col}
                <td>{fmtCell(col, user[col.key])}</td>
              {/each}
            </tr>
          {/each}
```

with:

```svelte
          {#each sortedUsers as user (user.id ?? user.email)}
            <tr>
              {#each columns as col}
                {#if col.key === "subscription_status"}
                  <td>
                    <span class="badge {subscriptionBadge(user).cls}">
                      {subscriptionBadge(user).text}
                    </span>
                  </td>
                {:else}
                  <td>{fmtCell(col, user[col.key])}</td>
                {/if}
              {/each}
            </tr>
          {/each}
```

- [ ] **Step 4: Build to verify no errors**

Run: `cd admin && pnpm install && pnpm run build`
Expected: build succeeds with no Svelte compile errors or warnings about the new markup.

- [ ] **Step 5: Manual verification against a running backend**

Ensure Postgres + backend are running (`podman-compose up -d` from repo root, then `cd backend && cargo run`). Optionally seed one Pro user directly for a quick visual check:

```sql
INSERT INTO subscriptions (user_id, stripe_customer_id, status)
VALUES ('<a real user id from your dev DB>', 'cus_manual_test', 'active');
```

Run: `cd admin && pnpm run dev` (serves on `http://localhost:5174`), log in as an admin user, and confirm:
- The Users table shows a rightmost "Subscription" column.
- A user with an `active`/`trialing` subscriptions row shows a green "Pro"/"Trialing" badge.
- A user with no subscriptions row shows a neutral "Free" badge.
- Clicking the "Subscription" column header sorts by the raw status text and toggles asc/desc, consistent with every other column.

Clean up the manually-inserted row afterward: `DELETE FROM subscriptions WHERE stripe_customer_id = 'cus_manual_test';`

- [ ] **Step 6: Commit**

```bash
git add admin/src/lib/UsersTable.svelte
git commit -m "feat(#340): render subscription status column in admin Users table"
```

---

### Task 3: Update AGENTS.md

**Files:**
- Modify: `AGENTS.md` (append a new numbered sub-bullet under §9 "Admin Authorization & Token Usage (#140)", or a new short entry — see below)

- [ ] **Step 1: Document the change**

`AGENTS.md` documents every shipped feature's surfacing in its own dated/numbered section (see §9 for the admin console, §11 for Stripe billing). Add one new bullet at the end of section 9 (find the line starting `- **`is_admin` gate**:` and the `- **Per-call token usage**:` bullet immediately after it — both under the `### 9. Admin Authorization & Token Usage (#140)` heading):

```markdown
- **Subscription status surfaced (#340)**: `GET /api/admin/users` (`admin.rs::list_users`) additionally `LEFT JOIN`s `subscriptions` and returns `subscription_status` (raw Stripe status, `NULL` if the user never started checkout) and `is_pro` (derived via the existing `billing::user_is_pro`, computed in Rust post-fetch rather than duplicated in SQL — single source of truth for the trialing/active entitlement rule). `UsersTable.svelte` renders this as a rightmost, sortable "Subscription" badge column (green Pro/Trialing, amber for a present-but-non-entitled status, neutral "Free" for no row) — read-only, no billing action is initiated from the admin console.
```

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#340): document admin subscription status surfacing"
```

---

## Spec Coverage Check

- AC "shows each user's current subscription status … alongside existing user info" → Task 1 (backend field) + Task 2 (frontend column).
- AC "tell at a glance whether … entitled (Pro) or gated (free)" → Task 2's badge (green Pro vs neutral Free vs amber lapsed).
- AC "sourced from the existing `subscriptions` table … via a small addition to the existing admin users endpoint — no new endpoint or schema/migration" → Task 1 (one `LEFT JOIN`, no migration, no new route).
- AC "Layout leaves room for a future billing country / price column … without committing to that design now" → Task 2 Step 1 (column appended at the end of the array, inside the existing `overflow-x-auto` wrapper; no placeholder field/column added).
