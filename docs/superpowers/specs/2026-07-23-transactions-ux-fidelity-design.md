# Transactions UX — Visual Fidelity, Detail View & Bidirectional Duplicate Matching (P5)

**Date:** 2026-07-23
**Epic:** follow-up to #403 (P1–P4 shipped in #404/#405/#417/#420)
**Status:** design — awaiting review

## Problem

The transactions redesign (#403) shipped its four functional phases, but the in-app
`TransactionsView.svelte` does not resemble the approved design mockup
(`https://claude.ai/code/artifact/b6a57337-3dd1-443b-917c-bcea38f47509`), and two behaviors
are effectively missing in practice. Concretely, from user review:

1. **Filter chips have no "All".** The three chips (Needs Review, Pending, Uncategorized) are
   independent `badge badge-lg` toggles with no reset/"All" option and no live counts.
2. **Chips are not responsive — too big.** `badge-lg` in a `flex-wrap` row grows and wraps
   awkwardly on narrow screens.
3. **No alert banner** summarizing how many rows need review / look like duplicates (the
   mockup's attention strip was never ported).
4. **"Pending" excludes uncategorized rows.** An imported row with no category is arguably the
   clearest "not yet settled" case, but today `isPending === needsReview` only.
5. **Duplicate reconciliation never fires in practice.** The matcher runs *only* at bank-import
   time and *only* finds a Nels-logged row that already exists. Because bank sync is automatic
   and usually lands the charge **before** the user logs it in chat, the common ordering never
   links — so the "possible duplicate" UI has nothing to show and the budget still double-counts.
   Root cause verified: `link_duplicate_for_import` is called from the 6 sync paths only; the
   `ai`/`manual` creation path (`budget.rs::create_transaction` / `finalize_transaction`,
   `rag.rs` chat finalize) never runs a reverse match.
6. **No detail for imported rows.** The list truncates the (raw) description and hides account /
   date / provider id, forcing the user to open their bank app to see what a charge was.
7. **Overall the row looks unpolished** vs. the mockup (no provenance rail, no source-tinted
   avatar tile, amounts not ledger-styled).

## Goal

Bring `TransactionsView` up to the mockup's visual quality using the app's daisyUI theme tokens
(so it stays light/dark-aware), make matching actually produce pairs, and surface enough per-row
detail that a user rarely needs their bank app. No new epic phases invented beyond this; #403
acceptance criteria remain satisfied and are strengthened.

## Approved decisions (from brainstorming)

- **Visual fidelity:** full visual port, inside the existing app shell (no marketing
  masthead/kicker/legend chrome).
- **Pending semantics:** `isPending = needs_review OR uncategorized`.
- **Duplicate UI:** stitched pair (both twins bracketed together), as in the mockup.
- **Detail:** inline expandable panel per row (not a modal/route).

## Design

### 1. Row visual system (frontend)

Rebuild the transaction row to the mockup's structure, mapping the mockup's bespoke palette onto
daisyUI semantic tokens so the app theme (and light/dark) still governs color:

- **Provenance rail** — a 3px colored left border per row keyed to `provenanceKind(tx)`:
  - `ai` → primary (indigo family) — "Logged by Nels"
  - `imported` → secondary/accent (teal family) — bank-synced
  - `manual`/none → neutral/base — "You logged it"
- **Avatar tile** — 30px rounded square, source-tinted background + source icon
  (`Sparkles` / bank glyph / `Pencil`). Purely decorative (`aria-hidden`).
- **Amount** — right-aligned, tabular-nums, monospace (`font-mono`), income positive-toned with a
  leading `+`, expense muted, excluded struck-through. Reuses existing `displayAmount` /
  `amountClass` / `formatAmount` / `amountTone` logic unchanged.
- **Sub-line** — category pill (with a dashed "Uncategorized" variant), source chip (the bank
  chip carries `account_label`, e.g. `Apple Card ••7793`), and state tags (Needs review /
  Pending / Excluded).
- **Collapsed row** shows merchant/description (truncated), sub-line, amount, and — when
  `needsReview` — the inline Approve button. A chevron/tap target toggles the detail panel (§4).

All of this is presentation; the existing data flow, `load()`, `reassign()`, `approve()`, and
`resolveDuplicate()` are unchanged.

### 2. Filter chips + alert strip (frontend)

- Chips become **compact pills** (~32px height, `rounded-full`, smaller text) in a **single
  horizontally-scrollable row** (`overflow-x-auto` + hidden scrollbar) so they never wrap or
  overflow on mobile.
- Add an **"All"** chip that clears every active filter (the default/reset state).
- Each chip shows a **live count** of the rows it would match, computed over the loaded
  `transactions` (not the already-filtered set).
- Active chip uses a filled/inverted treatment; inactive chips are outlined.
- **Pending semantics change:** `isPending(tx) = needsReview(tx) || isUncategorized(tx)` in
  `transactionsView.js`. `filterPending` and the Pending chip follow automatically. The
  Uncategorized chip remains as the narrower "no category" filter. Update the `isPending`
  docblock and its unit tests accordingly.
- **Alert strip** rendered above the list when there is anything to act on:
  - `"{n} imported transactions need review"` where `n = transactions.filter(needsReview).length`;
  - append `" · {m} look like duplicates"` when `m = transactions.filter(possibleDuplicate).length > 0`;
  - hidden entirely when `n === 0 && m === 0`.
  - Clicking the strip activates the Needs Review filter (sets `showNeedsReviewOnly = true`).
  - New i18n keys: `transactions.reviewSummary`, `transactions.duplicateSummary` (or a single
    ICU-pluralized key per locale), across all 6 locales.

### 3. Bidirectional duplicate matching (backend) + stitched UI (frontend)

**Backend — make matching fire regardless of order.** The link always lives on the *imported*
row (`matched_transaction_id` → the `ai`/`manual` twin), per the existing schema and
`resolve-match` endpoint; only the *trigger* is missing on the logging side.

- Add `link_duplicate_for_logged(pool, budget_id, logged_id)` to `duplicate_match.rs`: load the
  just-created `ai`/`manual` row, and find the closest **imported** row in the same budget that it
  duplicates (same criteria as `find_duplicate_candidate`, with the roles reversed — imported
  candidate must not already be linked). On a hit,
  `UPDATE transactions SET matched_transaction_id = <logged_id> WHERE id = <imported_id>` and
  ensure that imported row is surfaced as needs-review-if-still-pending (it keeps its existing
  `review_status`; no forced re-flag). Fail-safe/error-swallowing like
  `link_duplicate_for_import` — never abort a create.
- To avoid query duplication, factor the shared WHERE/ORDER logic so both directions share one
  candidate query parameterized by "which side is the anchor." Amount/currency/date-window/
  category-type guards are identical; the `source` filter differs (`imported` candidate vs.
  `ai`/`manual` candidate) and the "not already claimed" guard applies to whichever row would
  receive the link.
- Call `link_duplicate_for_logged` at the `ai`/`manual` creation entry points, mirroring how the
  6 sync sites call `link_duplicate_for_import`: `budget.rs::create_transaction`,
  `budget.rs::finalize_transaction`, and the chat add-transaction finalize path in `rag.rs`. The
  plan phase pins the exact insert sites (many `INSERT INTO transactions` matches are in
  `#[cfg(test)]` modules and are out of scope).

**Match rule.** Amount must be equal within half a cent and currency compatible (unchanged).
For the date rule, the spec keeps the existing **±4-day window** (`DEFAULT_WINDOW_DAYS = 4`)
rather than a strict same-calendar-day equality, because a bank posts a charge 1–3 days *after*
the day the user actually spent and logged it, so the two rows' `transaction_date`s routinely
differ; a window catches those, strict same-day would silently miss them.

> **DECISION FLAGGED FOR REVIEW.** During brainstorming the user described the rule as "same
> amount on the same day." This spec interprets that as "same amount, essentially the same day"
> and keeps the ±4-day window for the posting-lag reason above. If the user wants strict same-day
> equality, change `find_duplicate_candidate`'s window comparison to a `date_trunc('day', …)`
> equality and drop `window_days`. Confirm at spec review.

**Frontend — stitched pair.** When an imported row is a `possibleDuplicate`, render **both** it
and its `matched_transaction_id` twin bracketed inside one card: a header ("you logged this, then
your bank imported it"), the two rows with a connector, and **Merge (keep bank copy) / Keep
both** actions wired to the existing `resolveDuplicate(tx, 'merge'|'dismiss')`. The twin is
located client-side by id from the already-loaded `transactions` and pulled out of its normal
date-group position so the pair reads together. Merge → excludes the Nels twin (de-double-counts
via the existing `excluded_from_budget` path); Keep both → clears the link. Both non-destructive.
Reuse existing i18n keys (`possibleDuplicate`, `mergeKeepImported`, `keepBoth`, `resolveError`)
plus one new header key `transactions.duplicateStitchHeader`.

### 4. Transaction detail (backend + frontend)

- **Backend:** add `provider_transaction_id: Option<String>` to `TransactionResponse` (it already
  exists on the `Transaction` struct) and include it in the `list_transactions` mapping and the
  single-row response builders. No new column, no migration.
- **Frontend:** tapping a row toggles an **inline detail panel** (one open at a time is fine;
  simple per-row boolean or an `openId` in state). The panel shows the fields we actually have —
  and only those, so we never imply data we don't store:
  - full **untruncated** `description` (the raw bank string for imports),
  - `account_label` (imports),
  - exact `transaction_date` (date + time),
  - `amount` + `currency`,
  - category (name/type),
  - `source` / provenance,
  - `provider_transaction_id` (imports) — the bank's own id, for cross-referencing.
  - The category-reassign `<select>`, Approve, and any exclude affordance move into this panel so
    the collapsed row stays clean.
- New i18n keys for the detail labels across 6 locales (e.g. `transactions.detailAccount`,
  `transactions.detailDate`, `transactions.detailProviderId`, `transactions.detailRawDescription`).

## Component / file breakdown

- `frontend/src/lib/transactionsView.js` — update `isPending`; add any small pure helpers needed
  for chip counts and locating a duplicate's twin (`findTwin(transactions, tx)`); keep all helpers
  pure and unit-tested.
- `frontend/src/lib/TransactionsView.svelte` — the visual rebuild (rail, avatar, ledger amount,
  compact chips + All + counts, alert strip, stitched-pair card, inline detail panel). Consider
  extracting a `TransactionRow` and `DuplicateStitch` subcomponent if the file grows unwieldy, to
  keep each unit focused.
- `frontend/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json` — new keys above.
- `backend/src/budget.rs` — `TransactionResponse.provider_transaction_id` + mappings; call the
  reverse matcher in `create_transaction` / `finalize_transaction`.
- `backend/src/rag.rs` — call the reverse matcher in the chat add-transaction finalize path.
- `backend/src/duplicate_match.rs` — `link_duplicate_for_logged` + shared candidate query.
- Migrations: **none** (all fields already exist).

## Testing

- **`transactionsView.test.js` (Vitest):** `isPending` now true for needs_review OR
  uncategorized; chip counts; `findTwin`; grouping/search still compose. Existing tests updated
  where `isPending` semantics changed.
- **Svelte component tests where present:** All chip resets filters; alert strip visibility and
  click-to-filter; detail panel toggles and renders provider id/raw description; stitched pair
  renders both rows and merge/dismiss call `resolveDuplicate` with the right action.
- **Backend `#[ignore]` DB tests (pgvector, podman 6153):** reverse matcher links when the Nels
  row is logged *after* the import (the previously-unsupported ordering); still respects
  amount/currency/window/category-type guards; does not link an already-claimed import; error in
  the matcher never aborts the create. Add `provider_transaction_id` to the list-response
  assertion.
- Run `cargo test` (backend) and `vitest` (frontend); both green before PR.

## Out of scope

- Transaction splitting / multi-category and the full swipe-gesture system (already deferred by
  #403).
- A provider-level "pending/hold" flag distinct from needs-review (its own follow-up); until it
  exists, Pending = needs_review OR uncategorized as decided.
- Storing bank data we don't already have (merchant logo/address, running balance).
- Marketing chrome from the mockup (masthead, kicker, lede, legend, toast).

## Reachability / deploy

Unchanged surface: `TransactionsView` is reached via the chat `LIST_TRANSACTIONS` action
(`open_transactions_list` → `navigate('transactions')`) and the `/transactions-list` slash
command. Touches both backend and frontend → dual release-please PRs; merge one, hand-resolve the
`.release-please-manifest.json` conflict on the other (keep both versions), per project norms.
