# Spec: CATEGORY_AFFORDABILITY chat action (nels#302)

## 1. Brief (verbatim from the ticket)

> User story: As a user, when I ask an affordability question about a single budget category (e.g.
> "Can I spend $50 on Dining Out?", "Do I have $30 left for Entertainment?"), I want a direct,
> correctly-computed chat answer - not a page navigation, and not the LLM guessing at the
> arithmetic itself.
>
> Follow-up from #301: that ticket fixed the categories-page-navigation bug for existing "remaining
> balance"/"category details" questions (handled by `CATEGORY_BALANCE`). This ticket covers the
> separate, currently-missing capability: affordability questions have no dedicated backend action
> at all today and fall through to a generic path with no guaranteed direct answer.
>
> Acceptance Criteria:
> - "Can I spend $50 on Entertainment?" (enough remaining) answers in chat with the correct
>   yes/no + limit/spent/remaining/requested numbers, no page navigation.
> - Same question when the amount exceeds remaining answers "no" with the correct overage.
> - A nonexistent category name gets the same not-found message convention as `CATEGORY_BALANCE`.
> - A category-only phrase with no dollar figure gets a clarifying "how much?" question.
> - An ambiguous/garbled amount (voice dictation) gets the same confirm-before-acting flow as
>   `ADD_TRANSACTION`/`EDIT_TRANSACTION`.
> - A category with no limit configured gets an explicit "no cap to check against" answer - never
>   a false "unaffordable".
> - `cargo test -p backend` passes, including new unit tests for the comparison helper and offline
>   matcher.

## 2. Assumptions

- **A1 (overlap with #308)**: `savvagent/nels#308` was filed as a non-committal "starting point"
  draft of this exact capability during #301's review. Its suggested shape (a dedicated action
  mirroring `CATEGORY_BALANCE`'s deterministic pattern, fund/rollover-aware, answering inline) is
  fully subsumed by this ticket's formal AC. This PR implements #302's AC; #308 is closed as a
  duplicate pointing at this PR rather than tracked separately, since building both would mean
  building the same action twice under two different names.
- **A2 (action name)**: The action is `CATEGORY_AFFORDABILITY`, matching the ticket title exactly.
- **A3 (remaining-balance semantics — parity with CATEGORY_BALANCE, not #308's broader draft)**:
  #308's draft suggested "respecting rollover/fund effective-limit semantics." Looking at the
  actual `CATEGORY_BALANCE` implementation (`resolve_category_balance` /
  `build_categories_table_html` / `push_category_row` in `budget.rs`), its displayed "remaining"
  is `fund_effective_limit(is_fund, limit, fund_balance) - spent` — fund-aware, but **not**
  per-category-rollover-carry-aware (`category_carried`/#49 carry is computed only for the LLM's
  free-text "ACTIVE BUDGET DETAILS" prompt context, a separate code path, not for the
  deterministic table/balance answer). `CATEGORY_AFFORDABILITY` reuses the exact same
  effective-limit computation `CATEGORY_BALANCE` uses (via the same `CategoryTableRow` row and the
  same `fund_effective_limit` call), so a user can never get a `CATEGORY_BALANCE` answer and a
  `CATEGORY_AFFORDABILITY` answer that disagree about the same category's remaining balance on the
  same turn. This is a deliberate, existing scope boundary (not a regression introduced here); a
  rollover-carry-aware balance/affordability answer is a natural follow-up if ever requested, out
  of scope for #302.
- **A4 (ambiguous-amount confirmation is prompt-only, not offline-router logic)**: Reviewing
  `ADD_TRANSACTION_RULE`/`EDIT_TRANSACTION_RULE` (the existing "confirm-before-acting" precedent
  for garbled voice-dictation amounts, #257) shows the entire behavior lives in the **system
  prompt text** read by the live LLM — there is no dedicated Rust ambiguity-detection function, and
  the **offline** router does no ambiguity detection at all (it just parses whatever numeric token
  it finds). `CATEGORY_AFFORDABILITY` follows the identical precedent: a new prompt rule instructs
  the live model to flag an ambiguous amount, set `action` to `NONE`, state its best interpretation,
  and ask for confirmation — resolved the same way rule 1/21 already are (a plain affirmative next
  message uses the proposed amount; a restated amount overrides it). This AC point is therefore
  verified by inspecting the new prompt rule's text (mirroring the existing
  `add_transaction_rule_flags_ambiguous_dictated_amounts`-style tests that assert on rule text), not
  by new offline-router branching.
- **A5 (offline-matcher ordering vs. CATEGORY_BALANCE and LIST_TRANSACTIONS)**:
  `offline_category_balance_action` already claims any message containing "remain"/"left"/
  "balance". The ticket's own example — "Do I have $30 left for Entertainment?" — contains "left",
  so the new `offline_category_affordability_action` must be tried **before**
  `offline_category_balance_action` in `chat_endpoint`'s offline if/else chain. Separately,
  `offline_list_transactions_category` (tried even earlier, right after the privacy branch) claims
  any message containing "spend"/"spent"/"spending"/"transaction" that isn't itself an
  ADD_TRANSACTION/EDIT_TRANSACTION/"recategorize" phrasing — the ticket's OTHER example, "Can I
  spend $50 on Entertainment?", contains "spend" and trips none of that function's exclusion
  guards, so it resolves via `extract_trailing_category_name` and would dispatch as
  `LIST_TRANSACTIONS` unless the new matcher runs even earlier than that. So
  `offline_category_affordability_action` must be inserted **immediately after the privacy branch
  and before `offline_list_transactions_category`** — the first content-matching branch in the
  chain — not merely before `offline_category_balance_action`. It must itself require a stronger
  affordability signal — not just the same generic balance/spend nouns — so it claims exactly the
  intended message class without swallowing plain balance or transaction-listing questions that
  must still reach `CATEGORY_BALANCE`/`LIST_TRANSACTIONS`. Concretely: an explicit afford verb/phrase
  ("can i spend", "can i afford", "afford", "enough for", "enough left", "enough to spend") is
  itself sufficient (these never appear in a plain balance question). But "do i have" + "left" —
  the ticket's own example pattern — is only a reliable affordability signal when a dollar amount
  is ALSO present in the message; "Do I have $30 left for Entertainment?" has one, so it correctly
  routes here, but a bare "Do I have anything left in Entertainment?" (no dollar figure) does NOT
  have one and must fall through to `CATEGORY_BALANCE` instead of being misrouted into this
  action's "how much?" clarification (that would regress a question `CATEGORY_BALANCE` already
  answers directly into an unnecessary extra turn). So the matcher's "do i have"+"left" branch is
  additionally gated on finding a parseable dollar amount in the message; the "can i spend/afford/
  enough..." branch is not so gated (a category-only affordability phrase like "Can I afford
  Entertainment?" legitimately reaches the "how much?" clarification per A6/AC).
- **A6 (missing-amount clarification, both online and offline)**: The AC's "category-only phrase
  with no dollar figure" case must be reachable through the offline path too (there is no live
  Gemini key in the default test/dev environment), so the offline matcher returns the resolved
  category name with `amount: None` when no dollar figure is present, and the shared match-arm
  (below) asks the clarifying question regardless of whether the LLM or the offline router
  produced the dispatch. Because category extraction is entirely delegated to the existing
  `extract_trailing_category_name` (A7 below), a category-only offline test phrase must itself use
  one of that function's recognized prepositions ("in"/"on"/"for") — e.g. "Can I afford to spend on
  Entertainment?", not a preposition-less "Can I afford Entertainment?" (which
  `extract_trailing_category_name` cannot parse and would return `None` from, an existing,
  pre-#302 limitation shared by `CATEGORY_BALANCE`'s own offline matcher, not a new one introduced
  here).
- **A7 (missing-category clarification is LLM-only, mirroring CATEGORY_BALANCE)**: Like
  `CATEGORY_BALANCE`'s existing `None` category_name branch, the "which category?" clarifying
  outcome is only reachable when the live LLM dispatches the action with no `category_name` — the
  offline matcher never enters the CATEGORY_AFFORDABILITY branch without first extracting a
  category name, so this arm has no offline-triggerable test, exactly mirroring
  `CATEGORY_BALANCE`'s existing asymmetry.
- **A8 (response shape: text, not a table)**: `CATEGORY_BALANCE`'s answer is inherently
  tabular (`categories_table_html`, a single-row table). An affordability question's answer is a
  yes/no sentence, not a table — no acceptance criterion asks for a table — so
  `CATEGORY_AFFORDABILITY` renders a plain-text/markdown answer through a new addendum variable
  (`affordability_md`, mirroring `category_balance_md`'s existing "friendly clarifying/not-found
  prose" append pattern) rather than reusing `categories_table_html`. This also means it can never
  accidentally trip the `open_categories_list` navigation gate `CATEGORY_BALANCE`/#301 had to be
  fixed to avoid — there's nothing here for that gate to key off in the first place.
- **A9 (no new `ChatResponse` field, no new `AiActionParams` field)**: `category_name` and `amount`
  already exist on `AiActionParams` (used by `ADD_TRANSACTION` et al.), so the action needs no new
  request-side field. `affordability_md` is a local variable merged into `final_response_text`
  exactly like `category_balance_md`/`transactions_list_md`/`search_results_md` — none of those are
  `ChatResponse` fields either, so no response-shape change is needed.
- **A10 (no frontend change)**: Because the answer is plain text merged into `response`
  (`ChatResponse.response`) and no navigation/advisory flag is set, the existing frontend chat
  transcript renders it with zero changes — mirroring `category_balance_md`'s not-found/clarifying
  text today, which already renders as plain chat prose with no frontend-side handling.

## 3. Goal & Success Criteria

Add a `CATEGORY_AFFORDABILITY` chat action that deterministically answers "can I spend $X on
category Y" by comparing the requested amount against that category's actual current-period
effective remaining balance — never letting the LLM compute the arithmetic itself.

- A well-formed request (existing category + limit + a parseable dollar amount) gets a
  deterministic yes/no answer stating the limit, spent, remaining, and requested figures.
- An over-the-remaining request gets "no" plus the correct overage amount.
- An unresolvable category name gets `category_not_found_message`, byte-identical to
  `CATEGORY_BALANCE`'s convention.
- A category named with no dollar amount gets a "how much?" clarifying question, both via the
  live LLM and the offline router.
- A category with `category_limit = None` gets an explicit "no cap" answer regardless of `is_fund`
  or `fund_balance` — never a false "you can't afford this."
- The live-LLM prompt instructs the same ambiguous-amount confirm-before-acting flow
  `ADD_TRANSACTION`/`EDIT_TRANSACTION` already use, textually verifiable via a rule-content test.
- `cargo test -p backend` passes, with new unit tests covering the pure comparison helper and the
  offline matcher (both DB-free, per the AC).

## 4. Scope

**In scope:**
- New `CATEGORY_AFFORDABILITY` action: JSON-schema enum entry, system-prompt rule (20d), match arm
  in `chat_endpoint`, offline router entry.
- Pure comparison helper in `budget.rs` (`evaluate_affordability`), unit-tested.
- Pure offline matcher in `rag.rs` (`offline_category_affordability_action`), unit-tested,
  ordered ahead of `offline_category_balance_action`.
- Pure message-formatting helper in `rag.rs` (`format_affordability_message`), unit-tested,
  covering the "no limit configured" branch without a database.
- Closing `#308` as a duplicate once this PR is open.

**Out of scope (explicitly, matching CATEGORY_BALANCE's own boundary):**
- Per-category rollover-carry-aware remaining balance (see A3) — reusing CATEGORY_BALANCE's
  existing scope, not widening it.
- Any frontend change (see A10).
- New `ChatResponse`/`AiActionParams` fields (see A9).
- Multi-category or whole-budget affordability questions ("can I afford my whole shopping list") —
  the ticket is scoped to a single named category, mirroring `CATEGORY_BALANCE`.

## 5. Architecture

- `backend/src/budget.rs`: add `pub fn evaluate_affordability(requested: f64, effective_limit: f64,
  spent: f64) -> AffordabilityCheck` next to `fund_effective_limit`/`carried_amount`. Pure,
  numeric, unit-tested. `AffordabilityCheck { can_afford: bool, remaining: f64, overage: f64 }`.
- `backend/src/rag.rs`:
  - JSON schema `action` enum: append `"CATEGORY_AFFORDABILITY"` after `"CATEGORY_BALANCE"`.
  - New system-prompt rule `20d` (inline literal, matching `20c`'s style — not a separate
    `const` — since no test needs to assert on its exact text beyond what a normal review reads;
    see A4 above for how the ambiguity clause is verified).
  - New local variable `affordability_md: Option<String>`, declared beside `category_balance_md`.
  - New match arm `"CATEGORY_AFFORDABILITY"` (after the existing `"CATEGORY_BALANCE"` arm),
    reusing `resolve_category_balance` and `category_not_found_message`, delegating the actual
    yes/no arithmetic to `format_affordability_message`. That function matches on
    `row.category_limit: Option<f64>` FIRST — a `None` short-circuits to the "no cap configured"
    text and returns without ever calling `fund_effective_limit` (mirroring
    `push_category_row`'s `r.category_limit.map(|l| fund_effective_limit(...))` pattern exactly;
    it must NOT `unwrap_or(0.0)` a missing limit, which would silently diverge from
    `CATEGORY_BALANCE`'s "—" behavior and break A3's parity invariant). Only a `Some(base)` calls
    `budget::fund_effective_limit(is_fund, base, fund_balance)` followed by
    `budget::evaluate_affordability`.
  - Append `affordability_md` to `final_response_text` alongside the existing
    `category_balance_md` append.
  - New offline matcher `offline_category_affordability_action(msg: &str) -> Option<(String,
    Option<f64>)>`, placed and unit-tested next to `offline_category_balance_action`, and wired
    into `chat_endpoint`'s offline if/else chain **before** `offline_category_balance_action`.

## 6. Error Handling & Edge Cases

- No active budget: arm no-ops (mirrors `CATEGORY_BALANCE`'s `if let Some(bid) = active_budget_id`
  guard) — no crash, no confusing message beyond whatever the LLM/offline default `response_text`
  already says.
- Category name blank/whitespace-only: treated as absent (mirrors `CATEGORY_BALANCE`'s
  `.filter(|cn| !cn.trim().is_empty())`).
- `amount` present but negative or zero: not specially rejected — `evaluate_affordability` handles
  it arithmetically (a $0 request is always affordable unless already over budget); no ticket AC
  calls for a dedicated rejection message, so out of scope.
- DB failure resolving the category: `mutation_error`, matching `CATEGORY_BALANCE`'s convention
  (never silently swallowed, never conflated with "not found").
- Category has `category_limit = Some(0.0)` (not `None`) and is not a fund: a real, explicit $0
  cap — any positive request answers "no" with the correct overage. Distinct from the "no limit
  configured" (`None`) case, which is a "no cap at all" answer.

## 7. Testing Approach

- `budget.rs` unit tests: `evaluate_affordability` — affordable with room, affordable exactly at
  the boundary (`requested == remaining`), unaffordable with a computed overage, an already-negative
  remaining (fund deficit) makes any positive request unaffordable.
- `rag.rs` unit tests:
  - `offline_category_affordability_action`: extracts `(name, Some(amount))` for "Can I spend $50
    on Entertainment?" and "Do I have $30 left for Dining Out?"; extracts `(name, None)` for a
    category-only affordability phrase with no dollar figure (e.g. "Can I afford Entertainment?");
    returns `None` for a plain balance question with no afford verb AND no dollar figure (e.g.
    "Do I have anything left in Entertainment?" — must fall through to `CATEGORY_BALANCE`, per
    A5); returns `None` for a plain balance question with no afford verb even when a dollar figure
    happens to be present but no afford/enough phrasing exists; excludes "budget" as a resolved
    name (mirroring `offline_category_balance_action`'s existing guard).
  - Ordering: a message containing both an afford verb/pattern and a balance noun ("left") routes
    through the affordability matcher's own test, not by re-deriving `chat_endpoint`'s full
    if/else chain (that ordering is asserted by the source order of the two `else if` arms, reviewed
    directly rather than re-implemented in a test).
  - `format_affordability_message`: a fabricated `CategoryTableRow` (no DB) for each of: no-limit
    (`category_limit: None`) → "no cap" text; affordable; unaffordable with overage; a fund
    category whose `fund_effective_limit` differs from its bare `category_limit`; a fund category
    with `category_limit: None` and a nonzero `fund_balance` → still "no cap" text (the fund
    balance must never be consulted once the limit itself is absent — see Risks).
  - `category_not_found_message` is already tested; reused, not re-tested.
- Run `cargo test -p backend` (offline/pure tests only; no local Postgres needed for the new
  tests) and `cargo clippy -p backend --all-targets` before opening the PR.

## 8. Risks & Open Questions

- The exact response wording for "yes"/"no"/"no cap" is not mandated by the AC beyond "correct
  yes/no + limit/spent/remaining/requested numbers" — wording is this implementation's choice, kept
  close to `CATEGORY_BALANCE`'s existing tone.
- #308's draft suggested a possibly different action name (`AFFORDABILITY_CHECK`); #302's own title
  says `CATEGORY_AFFORDABILITY` — using the ticket's own name (A2) resolves this without asking.
- A fund category with `category_limit: None` but a nonzero `fund_balance` (a fund enabled before
  any limit was ever set — rare, but a real reachable data shape) reports "no cap configured" per
  the `None`-first-match rule above, ignoring the fund balance entirely. This exactly matches
  `CATEGORY_BALANCE`'s own existing "—" rendering for the same row shape (see `push_category_row`),
  so it's parity, not a new gap — flagged here for visibility, not as an open question.
