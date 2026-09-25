# Spec: Admin — surface account-syncing subscription status per user (nels#340)

## 1. Brief (verbatim from the ticket)

**As an** admin
**I want** to see each user's subscription/Pro status in the admin console
**So that** I can answer support questions about a user's account-syncing entitlement without querying the database directly

**Acceptance Criteria**
- The admin Users view (`admin/.../UsersTable.svelte`) shows each user's current subscription status (e.g. free / trialing / active / canceled) alongside existing user info.
- Admins can tell at a glance whether a user's account-syncing feature (#338) is currently entitled (Pro) or gated (free).
- The status is sourced from the existing `subscriptions` table (#25) via a small addition to the existing admin users endpoint — no new endpoint or schema/migration is needed.
- Layout leaves room for a future "billing country / price" column once per-country pricing (#338) expands beyond the US, without committing to that design now.

**Notes**: Companion to #338 (dedicated Accounts page, Pro-gated at $3/mo US). Read-only visibility only — admins do not initiate or modify billing from this view; subscription management stays in Stripe (portal/dashboard), consistent with how `billing.rs` already works.

## 2. Assumptions

1. **Extend the existing `GET /api/admin/users` query, not a new endpoint or join table.** `backend/src/admin.rs::list_users` already does one query with a `LEFT JOIN` sub-select for `llm_usage`; this ticket adds a second `LEFT JOIN` against `subscriptions` the same way (one row per user max, per `subscriptions.user_id PRIMARY KEY` — no fan-out risk). Matches the AC's explicit "small addition to the existing admin users endpoint — no new endpoint."
2. **Reuse `billing::user_is_pro` for the at-a-glance Pro/free signal, not a re-implemented rule.** `billing::user_is_pro(status: Option<&str>) -> bool` is already the single source of truth for what counts as entitled (`trialing`/`active` only). The admin endpoint computes `is_pro` server-side with this exact helper rather than duplicating the `trialing`/`active` match in SQL or in the frontend, so if the entitlement rule ever changes, the admin view can't silently drift from the real gate.
3. **New `AdminUserRow` fields: `subscription_status: Option<String>` and `is_pro: bool`.** `subscription_status` is the raw Stripe status column (`NULL` when the user has never started checkout — surfaced as "free" in the UI, not as an error state) so admins see the literal value (`trialing`, `active`, `past_due`, `canceled`, etc.) for support triage, not just a collapsed boolean. `is_pro` is the derived entitlement boolean (assumption 2) so the frontend doesn't need to re-derive it or import backend logic. `price_id`/`current_period_end`/`cancel_at_period_end` are NOT added — out of scope per the AC (no billing-management surface here) and not needed for "status at a glance"; a future ticket can add them if support needs more detail.
4. **No `SubscriptionUpsert`/`Subscription` struct reuse for the row shape.** The admin query selects the two needed subscription columns directly in the same `SELECT`/`FromRow` struct as the rest of `AdminUserRow` (mirroring how `input_tokens` etc. already flow from a sub-select), rather than a second query or the full `db::Subscription` row — keeps this a single round trip, consistent with the existing handler's one-query shape.
5. **Status display: a small colored badge, not a plain text cell.** The existing table already uses daisyUI classes (`table`, `badge`-free today, but daisyUI ships `badge`/`badge-success`/`badge-neutral`/`badge-warning`/`badge-error`). A `statusBadgeClass(row)` helper maps `is_pro` (and, for nuance, the raw `subscription_status`) to a badge: **Pro** (green, `active`/`trialing`) vs **Free** (neutral, no row/`NULL` status) vs **Past due / Canceled** (amber/red, present-but-not-entitled statuses) — "at a glance" per the AC means a color/weight signal, not just another sortable text column indistinguishable from Email/Name.
6. **New "Subscription" column is sortable like every other column**, using the existing `columns`/`colType`/`toComparable` machinery already in `UsersTable.svelte` — sorts by `subscription_status` string (nulls/"free" sort together) rather than by `is_pro`, since admins scanning for a specific Stripe state (e.g. all `past_due` for dunning follow-up) is a more useful sort than a binary grouping the badge color already conveys visually.
7. **"Room for a future billing country / price column" is satisfied by table layout, not a placeholder column or new schema.** The AC explicitly says "without committing to that design now" — this ticket does NOT add an empty/disabled column, a TODO comment naming specific future field names, or any `country`/`price` field to the response. It satisfies the requirement structurally: the new Subscription column is appended at the end of the existing `columns` array (not inserted mid-table), so a future column is a pure append with no reflow of existing columns, and the table already sits in an `overflow-x-auto` wrapper, so adding one more column later is a non-breaking, already-supported layout operation. No new field, no dead UI.
8. **No test-framework addition for the admin frontend.** `admin/package.json` has no test runner today (`dev`/`build`/`preview` only) and this is a small, visually-verifiable presentational change (a new column + badge) built on already-fetched data — consistent with the rest of `UsersTable.svelte`, which also has no existing frontend unit tests. Verification is manual (local `pnpm build`/`preview` against a real/seeded backend) plus the existing Rust-side `#[ignore]`-gated integration test pattern for the query change. Adding a frontend test harness for one file is out of scope and would be a disproportionate, unrequested change.
9. **Backend test coverage extends the existing `list_users_aggregates_usage_and_activity` integration test** (Postgres-backed, `#[ignore]`) rather than adding a parallel test file — it already seeds two users and asserts on `AdminUserRow` fields; this ticket adds a `subscriptions` row for one seeded user (a `trialing` or `active` status) and asserts `subscription_status`/`is_pro` on both the with-subscription and without-subscription (`NULL`/`false`) rows, in the same spirit as the existing token/activity assertions. A second, narrower test doesn't hurt but the existing fixture is the natural place to extend.
10. **No migration.** Confirmed by inspecting `backend/migrations/20260630120000_subscriptions.sql`: `status`, `price_id`, `current_period_end`, `cancel_at_period_end` already exist on `subscriptions`; nothing new is read that isn't already there.

## 3. Goal & Success Criteria

Admins viewing the Users table in the admin console can see, per user, whether they are currently Pro-entitled (account-syncing unlocked) or on the free tier, plus the underlying Stripe subscription status for support triage — sourced live from the existing `subscriptions` table via one additional `LEFT JOIN` on the existing `GET /api/admin/users` endpoint, with zero new endpoints, schema, or migrations.

Success criteria:
- [ ] `GET /api/admin/users` response rows include `subscription_status` (raw Stripe status, `null` if the user never started checkout) and `is_pro` (boolean, computed via `billing::user_is_pro`).
- [ ] A user with an `active`/`trialing` subscription row shows `is_pro: true`; a user with `past_due`/`canceled`/no row shows `is_pro: false`.
- [ ] `UsersTable.svelte` renders a new, sortable "Subscription" column showing a Pro/Free (plus finer past-due/canceled) badge, appended after the existing columns.
- [ ] The new column requires no reflow of existing columns and sits inside the table's existing horizontal-scroll wrapper, leaving a clean append point for a future billing-country/price column.
- [ ] No new REST endpoint, DB migration, or schema change is introduced.
- [ ] Existing `list_users` tests (admin-gate + aggregation) continue to pass unmodified in their existing assertions; new assertions cover the subscription fields.

## 4. Scope

**In scope**: `backend/src/admin.rs` (`AdminUserRow` gains `subscription_status`/`is_pro`; `list_users`'s SQL gains a `LEFT JOIN subscriptions`; extend the existing Postgres-backed test); `admin/src/lib/UsersTable.svelte` (new "Subscription" column definition, badge-rendering helper, sort support via existing machinery).

**Out of scope**: any new REST endpoint or route; any DB migration or new column/table; `price_id`/`current_period_end`/`cancel_at_period_end` surfacing; any billing-management action from the admin console (upgrade/downgrade/cancel — stays in Stripe per the ticket notes); a literal "billing country / price" column or any new field/placeholder for it; #338 (Accounts page) and #339 (marketing copy) — separate tickets, no file overlap; frontend test-framework setup for `admin/`.

## 5. Architecture

- **Backend** (`backend/src/admin.rs`): `list_users`'s query gains `LEFT JOIN subscriptions sub ON sub.user_id = u.id`, selecting `sub.status AS subscription_status`. `AdminUserRow` gains:
  ```rust
  pub subscription_status: Option<String>,
  pub is_pro: bool,
  ```
  `is_pro` is NOT a SQL-computed column — it's set in Rust after the query via `billing::user_is_pro(row.subscription_status.as_deref())` in a small post-processing map over the fetched rows (keeps the entitlement rule in exactly one place, per Assumption 2, rather than re-encoding `IN ('trialing','active')` in SQL). This means `AdminUserRow` can no longer derive straight off `sqlx::FromRow` for `is_pro` alone — the simplest shape is: keep `#[derive(sqlx::FromRow)]` for a row that includes `subscription_status` (nullable, decodes fine as `Option<String>`), fetch with `query_as`, then map each row to attach `is_pro` — OR add `#[sqlx(skip)] pub is_pro: bool` defaulted false and set it in the post-fetch loop. The post-fetch-map approach (fetch `Vec<AdminUserRow>` with a temporary field, then `.into_iter().map(...)`) is simplest and keeps `#[derive(sqlx::FromRow)]` fully mechanical; final call left to the implementer, either is acceptable as long as `billing::user_is_pro` is the single source of truth and no `trialing`/`active` string literal is duplicated in `admin.rs`.
  `crate::billing` must be importable from `admin.rs` (`billing::user_is_pro` — already `pub fn`, no visibility change needed).
- **Frontend** (`admin/src/lib/UsersTable.svelte`): append a column to the existing `columns` array:
  ```js
  { key: "subscription_status", label: "Subscription", type: "string" }
  ```
  (sorts on the raw string; `null`/missing sorts via the existing `toComparable` null-to-`""` fallback, which already sorts blanks predictably). Add a small helper, e.g.:
  ```js
  function subscriptionBadge(user) {
    if (user.is_pro) return { text: user.subscription_status === "trialing" ? "Trialing" : "Pro", cls: "badge-success" };
    if (user.subscription_status) return { text: user.subscription_status, cls: "badge-warning" };
    return { text: "Free", cls: "badge-neutral" };
  }
  ```
  and render it in the Subscription cell instead of running the value through the generic `fmtCell` string path (`fmtCell` stays unchanged for every other column — this one column gets its own cell markup, matching how the table already special-cases nothing else but is the natural extension point). The column is appended at the END of the `columns` array (Assumption 7) so today's column order (Email … Last Activity) is unchanged and the new column is the rightmost, natural append point for any later billing-country/price column.

## 6. Error Handling & Edge Cases

- **User with no `subscriptions` row at all** (never started checkout): `LEFT JOIN` yields `subscription_status: NULL` → `is_pro: false` → badge renders "Free". This is the majority case for most users and must not render as an error/blank.
- **`status` present but not `trialing`/`active`** (`past_due`, `unpaid`, `incomplete`, `incomplete_expired`, `canceled`): `is_pro: false`, badge shows the raw status text (not collapsed to "Free") so support can distinguish "never subscribed" from "lapsed" at a glance, per the AC's "free / trialing / active / canceled" example list.
- **Existing `list_users` callers/consumers**: the response is additive (two new fields); nothing existing is removed or renamed, so no breaking change for any other consumer of this endpoint (there are none besides `UsersTable.svelte` per the codebase).
- **Sort correctness**: `toComparable` already lowercases and defaults nullish values to `""` for string columns, so `NULL` statuses sort together (before any real status alphabetically) — no special-case needed beyond what's already there.
- **DB query failure**: unchanged — the existing `.map_err(...) -> 500` behavior on the whole query covers the joined query too; no new failure mode introduced (an absent `subscriptions` row is a `NULL` via `LEFT JOIN`, not a query error).

## 7. Testing Approach

- **Backend**: extend `backend/src/admin.rs`'s existing `#[ignore]` Postgres-backed test `list_users_aggregates_usage_and_activity` (run via `cargo test -- --ignored` against the local pgvector Postgres from `podman-compose up -d`) to seed a `subscriptions` row (`status = 'trialing'`) for `user_a` and leave `user_b` with none, then assert `row_a.subscription_status == Some("trialing")`, `row_a.is_pro == true`, `row_b.subscription_status == None`, `row_b.is_pro == false`. Clean up the seeded `subscriptions` row alongside the test's existing cleanup block.
- **Backend, non-DB**: no new pure function is introduced (the ticket deliberately reuses `billing::user_is_pro`, already unit-tested where it's defined), so no additional unit test is needed beyond the integration-test extension above.
- **Frontend**: manual verification — `cd admin && pnpm install && pnpm run dev` (or `build && preview`) against a locally running backend (`podman-compose up -d` + `cd backend && cargo run`) with at least one seeded Pro user (via a real Stripe test-mode checkout, or a direct `INSERT INTO subscriptions ...` in the dev DB) and one free user; confirm the Subscription column renders, sorts, and the badge text/color matches Assumption 5's mapping. No new automated frontend test (Assumption 8).
- **Regression**: run `cd backend && cargo test` (non-`--ignored` suite) to confirm nothing else in the crate breaks from the `admin.rs` change (struct/import changes can affect compilation elsewhere); run `cd backend && cargo check` for a fast compile-sanity pass before the full test run.

## 8. Risks & Open Questions

- **Badge color/label taxonomy is a judgment call** (Assumption 5) — "Pro"/"Trialing" both green, any other present status amber, absent green→neutral "Free". If a future admin need arises for e.g. distinguishing `canceled` (red) from `past_due` (amber), that's a natural, low-risk follow-up refinement to `subscriptionBadge`, not a blocker for this ticket, since the AC only asks for "entitled (Pro) or gated (free)" at a glance plus the raw status being visible.
- **`is_pro` computed in Rust vs SQL** (Architecture section) — computing it in SQL (`status IN ('trialing','active')`) would be marginally simpler but duplicates `billing::user_is_pro`'s rule in a second place; this spec deliberately picks the single-source-of-truth version. Flagged for the implementer/reviewer in case the team prefers the SQL-only shape for this narrow read-only view; either is low-risk, but Assumption 2's reasoning is why Rust-side reuse is preferred here.
