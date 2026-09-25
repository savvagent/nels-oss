# Spec: Retirement asset linking — Plaid Investments (nel#468)

## 1. Brief (from the ticket)

**As a** Nels Pro subscriber
**I want** my retirement accounts linked so that their balances and holdings flow into the retirement planner
**So that** the balance sheet (#464), the projection engine (#467), and the dashboard (#469) show my real money rather than a tastefully empty screen.

Part of #454 (retirement planner epic). #468 is the largest child: it corrects a premise that #454 carried from an older plan.

**The premise was wrong.** #454 assumed Plaid would provide US investment accounts. It does not — not for this product's geographic split:
- `bank_provider::Provider::for_country` maps `US` → Stripe Financial Connections (cash accounts) and `CA` → Plaid (cash accounts). `plaid.rs` hard-codes `country_codes: ["CA"]` and `products: ["transactions"]`.
- Stripe Financial Connections is hard-coded US, but it exposes investment **accounts** and **category spending** — it has **no holdings/securities/positions API** and no balances API that returns the per-HOLDING detail (`.balances` current balance only, no positions).

So neither existing connector can deliver the data #454's "link a 401(k)" storyline needs: per-holding positions with quantities and market values, keyed to a security and reconciled against a reported balance.

**Chosen path (option (c))**: a dedicated **investments-only Plaid Link** — `products: ["investments"]`, `country_codes: ["US"]` — separate from the existing cash bank-link (Plaid CA / Stripe FC). This:
- reuses Plaid's `/investments/holdings/get` and `/investments/transactions/get` for per-holding positions (which carry `security_id` → `/securities/get`), exactly what `asset_holdings` needs;
- keeps the existing cash connector untouched (no regression risk to Stripe FC US or Plaid CA);
- does NOT deliver the money out of the US via the cash path (Stripe/`investments` isn't available for CA; Plaid investments for CA is out of scope — US is the #454 vehicle market).

It borrows option (a)'s resolution shape (the same Plaid link-token → public-token → item/access-token flow §18/#321 already builds) because that plumbing is proven and testable; only the Link products/country and the sync target differ.

**Scope delineation**: #468 is the **integration** — Link, persistence, sync writers into `assets`/`asset_holdings`/`asset_balance_history`/`securities`, refresh cadence, gates, tests, docs. It is NOT the frontend (that is #469) nor the classification UI beyond the minimal server-side surface below. Chat is deliberately out of scope (see §2.16) — this is a REST-driven integration like #464.

## 2. Decisions & Rationale

### 2.1 Provider decision — option (c), dedicated investments Link
- `Provider::for_country("US")` stays Stripe FC for the **cash** path; Plaid stays CA for cash.
- A NEW explicit `LINK_INVESTMENTS` surface initiates a Plaid Link with `products: ["investments"]`, `country_codes: ["US"]` (and our own `client_id`/`secret`/env from the existing `PLAID_*` vars — one Plaid account serves both the CA cash Link and the US investments Link).
- `bank_provider::Provider` stays a closed 6-member enum with **no** new variant — investments is not a NEW country, it is a DIFFERENT PRODUCT on the SAME US/CA geography. Adding an `Investments` variant to `Provider` would imply it maps a country, which it does not. The investments Link is a separate code path in `plaid.rs` (or a sibling `plaid_investments.rs`) that reuses `plaid_post`/`plaid_api_base`/the mock-seam (`PLAID_API_BASE`).
- Production verification with live Plaid credentials is a documented manual check (no credentials in the test environment); the mock seam makes the whole flow testable.

### 2.2 Classification — user-supplied per account at link completion (the binding decision)
`assets.asset_type` and `assets.tax_treatment` are NOT NULL, and §20/#464 + §21/#465 forbid deriving them from vendor payloads: Plaid's investments API returns neither a 401(k)/Roth/taxable designation nor an account type even remotely close to one, the tax treatment is "not derivable from the balance, the institution or the vendor payload" (migration header, #464 decision 5), and #454 decision 5 makes tax treatment immutable-by-derivation.

Therefore the link **completion** flow asks the user, per account that has holdings:
1. `complete_link_session` returns the linked investment accounts (id, masked number, official_name/type for display helpers only) — and **at most one** classify step.
2. The caller submits, per account, `asset_type` (`retirement_account`/`brokerage`/`cash`/`pension`/`annuity`/`real_estate`/`other`) and `tax_treatment` (`pre_tax`/`roth`/`taxable`/`hsa`/`other`) — the strict §20 enums.
3. Only accounts that (a) have non-zero holdings or a balance AND (b) received a valid classification are written as `assets` rows. Mismatched/unknown enum strings → 400 naming the offending account and field, never a silent default. We never fabricate a fact about the user's money; an unclassified account is surfaced to the retry rather than defaulted.

The REST payload accepts the classifications as a map keyed by the same account identifier returned in step 1, so the two ends cannot drift on which account is being classified.

### 2.3 Ownership — `user_id` from the session, never from the link session's row
§20's obligation. `assets` is `user_id`-scoped, and the FK `(user_id, linked_account_id)` enforces that an asset's linked account belongs to the same user. The classification payload identifies accounts by a synthetic per-user id we assign in step 1 (we never trust a raw Plaid account id), so a cross-user insert is structurally impossible rather than merely FK-rejected. The linked account row for an investment account is `linked_accounts` with `provider = 'plaid'` (same table, provider value) plus the dedicated `plaid_item_id`/`plaid_cursor` columns (§18).

### 2.4 Sync writers — the new core of the module
No existing helper writes to `assets`, `asset_holdings`, `asset_balance_history`, or `securities`. #468 adds:
- `assets::insert_asset` already exists (from #464) — reuse for creating each classified asset row, passing `user_id`, `linked_account_id`, `asset_type`, `tax_treatment`, `status='active'`, `owner_member_id: None` (household is later), `current_balance`/`balance_as_of` from the investments snapshot.
- **NEW** `assets::upsert_security(provider, provider_security_id, name, ticker, security_type, currency)` → the `securities` row, `ON CONFLICT (provider, provider_security_id) DO UPDATE` (metadata cache — "VTSAX is a mutual fund" is the same for everyone). Returns the `id`. Must NOT touch `user_id` (none — shared cache).
- **NEW** `assets::replace_holdings_for_asset(asset_id, &[HoldingValue])` (or per-holding upsert) → `asset_holdings`: clear + insert, or upsert keyed on `(asset_id, security_id)`; sets `updated_at` in APPLICATION code per §20 (no trigger). §20's "a batch variant of `upsert_balance_history` must ensure its SELECT yields AT MOST ONE row per conflict key" applies to `asset_balance_history` only; holdings have no as-of conflict key.
- `assets::upsert_balance_history` exists (returns `BalanceWrite`). The investments sync records a balance-as-of snapshot so the balance-vs-holdings reconciliation (`allocate_reconciled`) has the reported figure. A fresh sync where the reported balance is absent stays `None` (never 0).

### 2.5 Reconciliation-ready
The sync writes holdings (per-security market value and quantity) and a reported balance when present. This is exactly the input `assets::allocate_reconciled` consumes, so `GET /api/assets` immediately reconciles real data. Multi-currency (§20's known gap) is handled by refusing to allocate a mixed-currency asset — no silent FX in the engine — but that is #467/reading-side; the sync itself stores per-holding currency faithfully.

### 2.6 Refresh cadence
A dedicated poll job on its own env-tunable ticker, mirroring `gocardless::poll_active_accounts` (§15): `INVESTMENTS_POLL_INTERVAL_HOURS` (default e.g. 12 → `/investments/holdings/get` per active investment account per cycle). The sync is idempotent (see §2.7). Plaid has `investments` webhook codes (`HOLDINGS`, `TRANSACTIONS`) — we use the poll as the v1 primary (consistent, one mechanism for initial link + refresh + catch-up) and do NOT build a separate investments webhook route in v1 (§2.13).

### 2.7 Idempotency
- `securities` upsert: `ON CONFLICT (provider, provider_security_id)`.
- `asset_holdings`: keyed on `(asset_id, security_id)`; re-sync overwrites in place (holdings are a full snapshot, not an append log). If a security disappeared between syncs, remove it (replace semantics) — a holdings snapshot is authoritative.
- `asset_balance_history`: `upsert_balance_history`'s `(asset_id, as_of)` key + `BalanceWrite` return — a re-sync of the same balance is a no-op / same-value overwrite.
- Link completion is idempotent per the existing `plaid_link_sessions` atomic-claim pattern.

### 2.8 Link sessions — separate from the CA cash flow
The existing `plaid_link_sessions` is used by the cash bank-link with `budget_id`. #468's investments link is **user-scoped and budget-less**. Two options:
- Reuse `plaid_link_sessions` with `budget_id NULL`, OR
- A dedicated `investment_link_sessions` table.
Decision: **dedicated `investment_link_sessions`** (`id`, `user_id`, `status`), keyed like `belvo_link_sessions`, because the cash table's `budget_id` is `NOT NULL` (making `NULL` a schema change) and the two flows have different completion shapes (cash: per-account tx sync; investments: classify + holdings write). Separation avoids conflating the two anti-replay keys. Same atomic-claim `UPDATE ... WHERE status='pending'` completion.

### 2.9 Routes (protected, user-scoped, `require_caller_tier(Pro)`)
- `POST /api/investments/link-token` → `{ link_token, session_id }` (creates an `investment_link_sessions` row; Link products `["investments"]`, country `["US"]`).
- `POST /api/investments/complete` `{session_id, public_token}` → claims the session, exchanges the public token, persists the linked account (`linked_accounts` provider `plaid`), returns `{ accounts: [{ id, name, masked_number }] , classifications }` awaiting the classify step.
- `POST /api/investments/classify` `{session_id, classifications: {<account_id>: {asset_type, tax_treatment}}}` → validates enums, writes the `assets` rows + initial holdings sync, reconciles the linked account. Idempotent on re-submission.
- `GET /api/investments/accounts` → the caller's investment-linked accounts (`linked_accounts` provider plaid + investments marker) and their `assets.status`. Reveals the data so #469 can render a link list and a disconnect button.
- `POST /api/investments/:account_id/refresh` → synchronous `/investments/holdings/get` + `/securities/get` + writes (Plaid has no separate refresh endpoint for holdings; the sync IS the refresh, same as §18's transactions asymmetry).
- `DELETE /api/investments/:account_id` → disconnect: Plaid `/item/remove` first (Idempotent on Plaid's side), then flip local rows to `status='disconnected'`, mirroring §14/§16 ordering. NOT Pro-gated (a lapsed user must be able to remove their own link).
- No `:budget_id` anywhere — `/api/investments/...` is a peer of `/api/assets`, not a budget child (§20).

### 2.10 Entitlement
Every write and read except **disconnect** gates on `require_caller_tier(&state.db, user_id, Tier::Pro)` — the caller's own tier (§20). Disconnect is ungated so a lapsed subscriber can remove a link (§14).

### 2.11 The `assets` linkage — one linked_account per investment account
An investment account becomes ONE `linked_accounts` row tied to ONE `asset` (the osition-bearing wrapper). §14/#321's "one Item backs multiple accounts" shape still holds (one Plaid Item → several investment accounts → several rows), and disconnect remains Item-wide: flipping `plaid_item_id`-sharing rows to `disconnected` also closes the `asset` (`status='closed'` — closed assets are excluded from projections, §20/§25) rather than deleting it.

### 2.12 No chat arm
§20's rule: retirement figures are not LLM prose. `LINK_BANK_ACCOUNT` stays a cash connector; investments linking is brought up from the (future) planner screen and its settings, driven by these REST endpoints. No `rag.rs` action arm, no `ChatResponse` field for investments in #468. (#469 drives the buttons.)

### 2.13 No webhook in v1
The poll job is the single sync mechanism (link-time initial + recurring refresh + manual refresh), so there is one sync implementation to test and no Plaid-investments webhook route with its own secret/config to stand up. A later issue can add an `HOLDINGS`/`TRANSACTIONS` webhook as an optimization; it is not required for correctness.

### 2.14 Env & testability seam
New env vars read at handler/poll time via the house `env_opt` / `env_required` helpers: none on the Plaid side (reuses `PLAID_CLIENT_ID`, `PLAID_SECRET`, `PLAID_ENV`, `PLAID_API_BASE`), plus `INVESTMENTS_POLL_INTERVAL_HOURS`. Tests drive the full Link→holdings→assets flow against a local mock server via `PLAID_API_BASE` (the existing seam), so no real Plaid credentials are needed in CI. `PLAID_API_BASE` must remain unset in production (defaults to the real API), same as `GITHUB_API_BASE`/`STRIPE_API_BASE`.

### 2.15 Unknown security type is lenient (already in §20)
`SecurityType::from_db` maps an unknown security type to `Other` with a `warn!`. The sync feeds `classify_security_type`'s output into `securities.security_type` via the typed `SecurityType` (no `_ =>` wildcard) — widening the CHECK becomes a compile error, per §20.

### 2.16 The classification UI won't be a full screen
The server returns accounts awaiting classification; #469 owns the actual pickers. #468 ships the **endpoints and the enum validation + typed storage** so the integration is complete and testable before any pixel. A minimal HTML-less PWA affordance is OUT of scope.

## 3. Goal & Success Criteria

A US Pro subscriber can link a US investment account via Plaid Investments, classify each account's `asset_type`/`tax_treatment`, and have its holdings, security metadata, and reported balance persist into `assets`/`asset_holdings`/`securities`/`asset_balance_history` — visible through `GET /api/assets` and consumed (reconciled) by the retirement projection — with a recurring poll keeping holdings fresh.

Success criteria:
- [ ] `POST /api/investments/link-token` returns a Plaid `link_token` (`products=["investments"]`, `country_codes=["US"]`) and a session row; Pro-gated; existing cash Plaid CA / Stripe FC flows untouched.
- [ ] `POST /api/investments/complete` + classify writes an `asset` per classified account with user-supplied `asset_type`/`tax_treatment`, `user_id` from the session, `linked_account_id` correctly FK'd to the same user's `linked_accounts` row.
- [ ] Full sync produces `securities` (shared cache, `ON CONFLICT`), `asset_holdings` (replace semantics per asset), and `asset_balance_history` (via `upsert_balance_history`), all with `updated_at` set in application code; idempotent across repeats.
- [ ] `GET /api/assets` returns reconciled allocations for the real holdings (exercises `allocate_reconciled`); a missing reported balance stays `None` (no false stale-balance signal).
- [ ] Poll job syncs active investment accounts on `INVESTMENTS_POLL_INTERVAL_HOURS`; disabled-running is safe idempotent.
- [ ] Disconnect: Plaid `/item/remove` first, then `linked_accounts.status='disconnected'` + `assets.status='closed'`; ungated.
- [ ] Classifications: invalid `asset_type`/`tax_treatment` → 400 naming the account/field; no silent default; never fabricate a fact.
- [ ] `#[ignore]`d DB tests: Pro gate (seed `status='trialing'`), cross-user rejection via the composite FK, idempotency, disconnect ordering, poll idempotency, enum validation (both strict types).
- [ ] Mock-server tests (unit-level, via `PLAID_API_BASE`): full Link→sync→assets on a mocked Plaid; no real credentials required.
- [ ] No `rag.rs` action, no chat field; no budget-param route on `/api/investments/...`.
- [ ] Docs: AGENTS.md §26; `.env.example` entries; the manual live-Plaid verification checklist.

## 4. Scope

**In scope**: `backend/src/plaid_investments.rs` (or additions to `plaid.rs`) — Link-token request, complete/classify/refresh/disconnect handlers, `sync_investment_holdings`, security/holdings writers; the `investment_link_sessions` migration; `assets::upsert_security` + `assets::replace_holdings_for_asset`; routes in `main.rs`; the poll job; gates; mock-server + `#[ignore]`d DB tests; `.env.example`; AGENTS.md §26; the spec.

**Out of scope**: any UI (none in this issue — it is integration only); chat/rag.rs; an investments webhook route (poll-only v1); tax bracket math; real-salary growth; per-asset-class return modeling; `owner_member_id` population (household is later); any change to #464/#465/#466/#467 types, enums, or the existing cash Plaid/Stripe FC flows.

## 5. Architecture

**New/changed files**:
- `backend/src/plaid_investments.rs` (new module `mod plaid_investments;`) — Link-token mint, complete (session claim → access token → linked account), classify (validate enums → write `assets`), sync (holdings/securities/balance writers), refresh, disconnect, poll iteration, all via `plaid_post`/`plaid_api_base`/`PLAID_API_BASE`.
- `backend/src/assets.rs` — add `upsert_security` and `replace_holdings_for_asset` (application-set `updated_at`); reuse `insert_asset`/`upsert_balance_history`.
- `backend/migrations/2026XXXX_investment_link_sessions.sql` — new table.
- `backend/src/main.rs` — `mod`; `/api/investments/...` routes; `INVESTMENTS_POLL_INTERVAL_HOURS` poll ticker (mirrors the GoCardless block).
- `backend/Cargo.toml` — no new deps (reuses `reqwest`, `serde`).
- `AGENTS.md` §26; `.env.example`.

## 6. Error Handling & Edge Cases

- Invalid classification enum → 400 naming the account + field; never a silent default (strict §20 enums).
- Cross-user linked account on classify → surfaced as a 400/403 with a real message (the composite FK's `foreign_key_violation` handled deliberately per §20, not a 500).
- A completed session re-submitted → 409/404 via the atomic-claim guard.
- Disconnect when Plaid's `/item/remove` fails → 5xx, local rows untouched (call Plaid FIRST, §14).
- Lapsed subscriber: 402 on every gate except disconnect; zero Plaid work.
- A sync where the reported balance is absent → `current_balance` stays `None` (never zero), no false stale-balance signal.
- Mixed-currency holding data → stored faithfully per-row; allocation refusing mixed-currency is the reading side's concern (§2.5), storage never coerces.
- Unknown security type → `Other` with a `warn!` (§20 leniency); unknown `tax_treatment`/`asset_type` are impossible by construction (strict enums + validation).

## 7. Testing Approach

- **Unit / mock-server (no real credentials)**: Link-token request body asserts `products=["investments"]`, `country_codes=["US"]`; `plaid_post` hits `PLAID_API_BASE` when set (seam); holdings/securities response fixtures flow into `securities` + `asset_holdings` writers; security-type classification mapping; poll iteration over active investment accounts.
- **`#[ignore]`d DB tests** (seed Pro with `status='trialing'` per the §21 test-infrastructure note): Pro gate (402), classify writes an asset with correct `user_id`/`linked_account_id`/enums, cross-user rejection (composite FK 23503), holdings replace-idempotency, balance-history idempotency + `BalanceWrite`, disconnect ordering + `assets.status='closed'`, unclassified-and-zero-holdings account writes no `assets` row.
- **Manual** (production): documented live-Plaid US checklist — link `retirement_account`/`pre_tax`, confirm holdings + reconciled allocation on `GET /api/assets`, confirm poll keeps it fresh.

## 8. Risks & Open Questions

- **Plaid Investments availability for a US household/consumer environment**: investments products may sit behind Plaid's higher-value tiers. The spec assumes the sandbox and the granted environment expose `/investments/holdings/get`; if the real environment's Plaid plan excludes it, this degrades to a documented manual gap, not a code change. Confirmed during planning against the account's enabled products.
- **`INVESTMENTS_POLL_INTERVAL_HOURS` default** (12h) is a placeholder; exact value set during planning to stay within the Plaid environment's allowed call rate while keeping holdings reasonably fresh (reused the GoCardless poll precedent, not measured against real rate limits).
- **Complete-then-classify split adds a stateful step** (accounts returned awaiting classification). If rejected as over-complex, the alternative is classify-inline in `complete`; the split is kept because encodaxing classification in the completion payload couples a stateful write to a user-input round-trip that the enums genuinely require. Flagged for review.
- **Mock-server fidelity**: the test seam drives JSON fixtures, which proves our request/response wiring but not Plaid's real error taxonomy for investments. The manual checklist covers the gap.