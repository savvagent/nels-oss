# CATEGORY_AFFORDABILITY Chat Action Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `CATEGORY_AFFORDABILITY` chat action that deterministically answers "can I spend $X
on category Y" by comparing the requested amount against that category's actual current-period
effective remaining balance, per `savvagent/nels#302`.

**Architecture:** A pure numeric comparison helper (`budget::evaluate_affordability`) plus a pure
message formatter (`rag::format_affordability_message`) compute the yes/no answer from a
`CategoryTableRow` already fetched via the existing `resolve_category_balance` helper
(`CATEGORY_BALANCE`'s own category-resolution path, reused unchanged). A new offline-router matcher
(`rag::offline_category_affordability_action`) and a new JSON-schema/system-prompt rule wire the
action into `chat_endpoint`'s existing dispatch, mirroring `CATEGORY_BALANCE`'s (#282) structure
throughout.

**Tech Stack:** Rust, axum, sqlx (Postgres) — `backend/src/budget.rs`, `backend/src/rag.rs`. No
frontend, no migration, no new schema.

Spec: `docs/superpowers/specs/2026-07-04-category-affordability-design.md`

---

### Task 1: Pure comparison helper (`budget::evaluate_affordability`)

**Files:**
- Modify: `backend/src/budget.rs` (add the struct + function near `fund_effective_limit`, line
  ~575; add tests near the existing `fund_effective_limit_*` tests, line ~5317)

- [ ] **Step 1: Write the failing tests**

Find the existing `fund_effective_limit_*` test block in `backend/src/budget.rs` (search for
`fn fund_effective_limit_non_fund_ignores_balance`, around line 5317) and add these tests
immediately after that block, inside the same `mod tests` block:

```rust
    #[test]
    fn evaluate_affordability_affordable_with_room() {
        // limit 200, spent 80 -> remaining 120; requesting 50 fits with room to spare.
        let check = evaluate_affordability(50.0, 200.0, 80.0);
        assert!(check.can_afford);
        assert_eq!(check.remaining, 120.0);
        assert_eq!(check.overage, 0.0);
    }

    #[test]
    fn evaluate_affordability_affordable_at_exact_boundary() {
        // remaining is exactly 120; requesting exactly 120 must still be affordable (<=, not <).
        let check = evaluate_affordability(120.0, 200.0, 80.0);
        assert!(check.can_afford);
        assert_eq!(check.remaining, 120.0);
        assert_eq!(check.overage, 0.0);
    }

    #[test]
    fn evaluate_affordability_unaffordable_reports_overage() {
        // limit 200, spent 160 -> remaining 40; requesting 50 is 10 over.
        let check = evaluate_affordability(50.0, 200.0, 160.0);
        assert!(!check.can_afford);
        assert_eq!(check.remaining, 40.0);
        assert_eq!(check.overage, 10.0);
    }

    #[test]
    fn evaluate_affordability_negative_remaining_from_fund_deficit() {
        // A fund category already in deficit (effective_limit already negative, e.g. -20 from a
        // sustained overspend) makes ANY positive request unaffordable, with overage = requested
        // minus the (negative) remaining.
        let check = evaluate_affordability(10.0, -20.0, 0.0);
        assert!(!check.can_afford);
        assert_eq!(check.remaining, -20.0);
        assert_eq!(check.overage, 30.0);
    }

    #[test]
    fn evaluate_affordability_zero_request_is_always_affordable_unless_already_over() {
        let check = evaluate_affordability(0.0, 100.0, 100.0);
        assert!(check.can_afford);
        assert_eq!(check.remaining, 0.0);
        assert_eq!(check.overage, 0.0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cd backend && cargo test evaluate_affordability 2>&1 | tail -30`
Expected: compile error — `evaluate_affordability`/`AffordabilityCheck` not found in this scope.

- [ ] **Step 3: Write the minimal implementation**

In `backend/src/budget.rs`, immediately after the `fund_effective_limit` function (ends around
line 581, right before the `/// Guard a budget against mutation...` doc comment for
`ensure_not_closed`), add:

```rust
/// The result of comparing a requested spend amount against a category's
/// ACTUAL effective remaining balance this period (CATEGORY_AFFORDABILITY,
/// #302). `remaining` and `overage` are always computed (even when
/// `can_afford` is true, `overage` is 0.0 — never negative), so a caller can
/// render both branches of the answer from one value without re-deriving the
/// arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffordabilityCheck {
    pub can_afford: bool,
    pub remaining: f64,
    pub overage: f64,
}

/// Compare a requested spend amount against a category's effective remaining
/// balance (#302). `effective_limit` must already be the FUND-AWARE effective
/// limit (the caller resolves `category_limit: Option<f64>` — including the
/// "no limit configured" case — BEFORE calling this; see
/// `rag::format_affordability_message`, which mirrors
/// `push_category_row`'s `category_limit.map(|l| fund_effective_limit(...))`
/// pattern). `remaining` can legitimately be negative (an already-overspent
/// or fund-deficit category) — in that case any positive `requested` is
/// unaffordable, and `overage` is exactly `requested - remaining` (larger
/// than `requested` itself). Affordability is inclusive at the boundary:
/// `requested == remaining` is affordable (`<=`, not `<`).
pub fn evaluate_affordability(requested: f64, effective_limit: f64, spent: f64) -> AffordabilityCheck {
    let remaining = effective_limit - spent;
    let can_afford = requested <= remaining;
    let overage = if can_afford { 0.0 } else { requested - remaining };
    AffordabilityCheck { can_afford, remaining, overage }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test evaluate_affordability 2>&1 | tail -30`
Expected: `test result: ok. 5 passed`

- [ ] **Step 5: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#302): add evaluate_affordability comparison helper"
```

---

### Task 2: Pure offline matcher + message formatter (`rag.rs`)

**Files:**
- Modify: `backend/src/rag.rs`
  - Add `format_affordability_message` near `category_not_found_message` (line ~3739) and
    `resolve_category_balance` (line ~3756).
  - Add `offline_category_affordability_action` near `offline_category_balance_action`
    (line ~5367).
  - Add tests near the existing `offline_category_balance_action_*` tests (line ~6788) and near
    `category_not_found_message_names_the_category` (line ~12864).

- [ ] **Step 1: Write the failing tests for `offline_category_affordability_action`**

Find the `offline_category_balance_action_excludes_budget_level_questions` test (search for that
exact name, around line 6807-6813) in `backend/src/rag.rs`'s `mod tests` block, and add these
tests immediately after it:

```rust
    #[test]
    fn offline_category_affordability_action_extracts_name_and_amount_for_core_phrasings() {
        assert_eq!(
            offline_category_affordability_action("Can I spend $50 on Entertainment?"),
            Some(("Entertainment".to_string(), Some(50.0)))
        );
        assert_eq!(
            offline_category_affordability_action("Do I have $30 left for Dining Out?"),
            Some(("Dining Out".to_string(), Some(30.0)))
        );
    }

    #[test]
    fn offline_category_affordability_action_missing_amount_returns_name_with_none() {
        // `extract_trailing_category_name` (reused unchanged) only recognizes the "in"/"on"/"for"
        // prepositions — a preposition-less "Can I afford Entertainment?" cannot resolve a name
        // (see the existing `extract_trailing_category_name("no preposition here") == None` test);
        // this is an existing, pre-#302 limitation CATEGORY_BALANCE's own offline matcher already
        // has, not a new one. So the category-only phrasing here uses " on " like the ticket's own
        // "Can I spend $50 on Entertainment?" example.
        assert_eq!(
            offline_category_affordability_action("Can I afford to spend on Entertainment?"),
            Some(("Entertainment".to_string(), None))
        );
    }

    #[test]
    fn offline_category_affordability_action_defers_to_category_balance_without_afford_signal() {
        // No afford verb and no dollar figure: a plain balance question must fall through to
        // CATEGORY_BALANCE's own offline matcher, not be misrouted here (nels#302 spec A5).
        assert_eq!(
            offline_category_affordability_action("Do I have anything left in Entertainment?"),
            None
        );
        assert_eq!(
            offline_category_affordability_action("how much is left in Food"),
            None
        );
    }

    #[test]
    fn offline_category_affordability_action_excludes_budget_level_questions() {
        // Same preposition constraint as the missing-amount test above: "on my budget" gives
        // extract_trailing_category_name something to resolve ("budget", after stripping the "my "
        // article), which is what actually exercises the eq_ignore_ascii_case("budget") guard.
        assert_eq!(
            offline_category_affordability_action("Can I afford to spend on my budget?"),
            None
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cd backend && cargo test offline_category_affordability_action 2>&1 | tail -30`
Expected: compile error — `offline_category_affordability_action` not found in this scope.

- [ ] **Step 3: Write the minimal implementation for the offline matcher**

In `backend/src/rag.rs`, immediately BEFORE the `offline_category_balance_action` function
(search for `/// Offline-router intent match for CATEGORY_BALANCE (nels#282)`, around line 5356),
insert:

```rust
/// Offline-router intent match for CATEGORY_AFFORDABILITY (nels#302). Returns
/// `(category_name, requested_amount)` when the message expresses a
/// single-category affordability question ("can I spend $X on Y", "can I
/// afford Y", "do I have $X left for Y"), else None. `requested_amount` is
/// `None` when the phrase names a category but no dollar figure ("can I
/// afford Entertainment?") — the caller asks a clarifying "how much?"
/// question rather than guessing.
///
/// MUST be routed BEFORE `offline_category_balance_action` in
/// `chat_endpoint`'s offline chain: "do I have $30 left for Entertainment"
/// contains "left" (a CATEGORY_BALANCE trigger word) but also states a
/// dollar figure, so the affordability question must claim it first or
/// CATEGORY_BALANCE's balance-noun matcher would swallow it.
///
/// An explicit afford verb/phrase ("can i spend", "can i afford", "afford",
/// "enough for", "enough left", "enough to spend") is sufficient on its own
/// — none of these appear in a plain balance question. But "do i have" +
/// "left" is only a reliable affordability signal when a dollar amount is
/// ALSO present in the message; without one, a bare "Do I have anything left
/// in Entertainment?" must fall through to CATEGORY_BALANCE instead of being
/// misrouted into this action's "how much?" clarification (nels#302 spec A5).
pub(crate) fn offline_category_affordability_action(msg: &str) -> Option<(String, Option<f64>)> {
    let lower = msg.to_lowercase();
    let amount = msg.split_whitespace().find_map(|w| {
        let cleaned = w.trim_start_matches('$').trim_matches(['?', '.', ',', '!']);
        cleaned.parse::<f64>().ok()
    });

    let has_afford_verb = lower.contains("can i spend")
        || lower.contains("can i afford")
        || lower.contains("afford")
        || lower.contains("enough for")
        || lower.contains("enough left")
        || lower.contains("enough to spend");
    let has_do_i_have_left_with_amount =
        lower.contains("do i have") && lower.contains("left") && amount.is_some();

    if !has_afford_verb && !has_do_i_have_left_with_amount {
        return None;
    }

    let name = extract_trailing_category_name(msg)?;
    if name.eq_ignore_ascii_case("budget") {
        return None;
    }
    Some((name, amount))
}

```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test offline_category_affordability_action 2>&1 | tail -30`
Expected: `test result: ok. 4 passed`

- [ ] **Step 5: Write the failing tests for `format_affordability_message`**

Find `category_not_found_message_names_the_category` (search for that exact name, around line
12864) in `backend/src/rag.rs`'s `mod tests` block, and add these tests immediately after it:

```rust
    #[test]
    fn format_affordability_message_no_limit_configured_never_says_unaffordable() {
        let row = crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Entertainment".to_string(),
            category_type: "expense".to_string(),
            category_limit: None,
            spent: 40.0,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let msg = format_affordability_message("Entertainment", &row, 50.0);
        assert!(msg.contains("no cap"), "expected a 'no cap' message, got: {}", msg);
        assert!(!msg.to_lowercase().contains("unaffordable"));
        assert!(!msg.starts_with("No"), "a no-limit category must never answer 'No', got: {}", msg);
    }

    #[test]
    fn format_affordability_message_fund_with_no_limit_still_says_no_cap() {
        // A fund enabled before any limit was ever set: category_limit is None even though
        // fund_balance is nonzero. The limit's absence must short-circuit BEFORE
        // fund_effective_limit is ever consulted (nels#302 spec A3/Risks).
        let row = crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Entertainment".to_string(),
            category_type: "expense".to_string(),
            category_limit: None,
            spent: 0.0,
            is_fund: true,
            fund_balance: 500.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let msg = format_affordability_message("Entertainment", &row, 50.0);
        assert!(msg.contains("no cap"), "expected a 'no cap' message, got: {}", msg);
    }

    #[test]
    fn format_affordability_message_affordable_says_yes_with_numbers() {
        let row = crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Entertainment".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(200.0),
            spent: 80.0,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let msg = format_affordability_message("Entertainment", &row, 50.0);
        assert!(msg.starts_with("Yes"), "got: {}", msg);
        assert!(msg.contains("200.00"), "must state the limit, got: {}", msg);
        assert!(msg.contains("80.00"), "must state spent, got: {}", msg);
        assert!(msg.contains("120.00"), "must state remaining, got: {}", msg);
        assert!(msg.contains("50.00"), "must state the requested amount, got: {}", msg);
    }

    #[test]
    fn format_affordability_message_unaffordable_says_no_with_overage() {
        let row = crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Entertainment".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(200.0),
            spent: 160.0,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let msg = format_affordability_message("Entertainment", &row, 50.0);
        assert!(msg.starts_with("No"), "got: {}", msg);
        assert!(msg.contains("10.00"), "must state the $10 overage, got: {}", msg);
        assert!(msg.contains("40.00"), "must state remaining ($40), got: {}", msg);
    }

    #[test]
    fn format_affordability_message_fund_uses_effective_limit() {
        // limit 100 + fund_balance 50 = effective 150; spent 100 -> remaining 50; requesting 50
        // is exactly affordable only because the fund balance was added in.
        let row = crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Groceries".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(100.0),
            spent: 100.0,
            is_fund: true,
            fund_balance: 50.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let msg = format_affordability_message("Groceries", &row, 50.0);
        assert!(msg.starts_with("Yes"), "got: {}", msg);
        assert!(msg.contains("150.00"), "must state the fund-effective limit, got: {}", msg);
    }
```

- [ ] **Step 6: Run the tests to verify they fail to compile**

Run: `cd backend && cargo test format_affordability_message 2>&1 | tail -30`
Expected: compile error — `format_affordability_message` not found in this scope.

- [ ] **Step 7: Write the minimal implementation for the message formatter**

In `backend/src/rag.rs`, immediately AFTER `resolve_category_balance` (search for the function
ending `Ok(rows.into_iter().find(|r| r.name.to_lowercase() == needle))`, around line 3763-3764),
insert:

```rust

/// Render the CATEGORY_AFFORDABILITY (nels#302) deterministic yes/no answer
/// for a resolved category row. `row.category_limit` is checked FIRST: a
/// `None` means no limit is configured at all, so there is nothing to check
/// the requested amount against — this returns an explicit "no cap" message
/// WITHOUT ever calling `fund_effective_limit` (mirroring
/// `push_category_row`'s `category_limit.map(|l| fund_effective_limit(...))`
/// pattern; a `None` limit must never fall back to 0.0, which would produce a
/// false "you can't afford this"). Only a `Some(base)` limit computes the
/// fund-aware effective limit and delegates the actual yes/no arithmetic to
/// `budget::evaluate_affordability` — the SAME effective-limit computation
/// CATEGORY_BALANCE renders for the same row, so the two actions can never
/// disagree about a category's remaining balance.
fn format_affordability_message(
    category_name: &str,
    row: &crate::budget::CategoryTableRow,
    requested: f64,
) -> String {
    let base = match row.category_limit {
        None => {
            return format!(
                "'{}' has no limit configured, so there's no cap to check your ${:.2} against — go ahead.",
                category_name, requested
            );
        }
        Some(l) => l,
    };
    let effective_limit = crate::budget::fund_effective_limit(row.is_fund, base, row.fund_balance);
    let check = crate::budget::evaluate_affordability(requested, effective_limit, row.spent);
    if check.can_afford {
        format!(
            "Yes — you can spend ${:.2} on {}. Limit ${:.2}, spent ${:.2}, remaining ${:.2} (requested ${:.2}).",
            requested, category_name, effective_limit, row.spent, check.remaining, requested
        )
    } else {
        format!(
            "No — spending ${:.2} on {} would put you ${:.2} over. Limit ${:.2}, spent ${:.2}, remaining ${:.2} (requested ${:.2}).",
            requested, category_name, check.overage, effective_limit, row.spent, check.remaining, requested
        )
    }
}
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cd backend && cargo test format_affordability_message 2>&1 | tail -30`
Expected: `test result: ok. 5 passed`

- [ ] **Step 9: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#302): add CATEGORY_AFFORDABILITY offline matcher and message formatter"
```

---

### Task 3: Wire the action into the schema, prompt, match arm, and offline chain

**Files:**
- Modify: `backend/src/rag.rs`
  - JSON schema `action` enum (line ~1334).
  - New system-prompt rule `20d` (after rule `20c`, line ~1406).
  - New `affordability_md` local variable (near `category_balance_md`, line ~1938).
  - New match arm `"CATEGORY_AFFORDABILITY"` (after the `"CATEGORY_BALANCE"` arm, line ~3328).
  - Append `affordability_md` to `final_response_text` (near the `category_balance_md` append,
    line ~3550).
  - New offline chain entry (before `offline_category_balance_action`'s `else if`, line ~1617).

This task has no new pure-function tests of its own (Tasks 1-2 already cover the pure logic
end-to-end); it wires already-tested building blocks together, then Step 6 verifies the FULL
`cargo test -p backend` suite still passes (the ticket's own acceptance bar).

- [ ] **Step 1: Add `CATEGORY_AFFORDABILITY` to the JSON schema action enum**

In `backend/src/rag.rs`, find this line (around line 1334):

```rust
           \"action\": \"NONE\" | \"CREATE_BUDGET\" | \"UPDATE_BUDGET\" | \"CLOSE_BUDGET\" | \"ARCHIVE_BUDGET\" | \"UNARCHIVE_BUDGET\" | \"ROLLUP_BUDGET\" | \"UNROLLUP_BUDGET\" | \"CREATE_CATEGORY\" | \"SEED_CATEGORIES\" | \"DELETE_CATEGORY\" | \"UPDATE_CATEGORY\" | \"SET_CATEGORY_ROLLOVER\" | \"SET_CATEGORY_FUND\" | \"DELETE_BUDGET\" | \"ADD_TRANSACTION\" | \"EDIT_TRANSACTION\" | \"DELETE_TRANSACTION\" | \"SHARE_BUDGET\" | \"CREATE_GOAL\" | \"ADD_GOAL_CONTRIBUTION\" | \"CREATE_REMINDER\" | \"LIST_BUDGETS\" | \"SWITCH_BUDGET\" | \"SET_USER_NAME\" | \"EXPORT_DATA\" | \"OPEN_INSIGHTS\" | \"OPEN_BUDGETS_LIST\" | \"LIST_CATEGORIES\" | \"CATEGORY_BALANCE\" | \"SEARCH_TRANSACTIONS\" | \"LIST_TRANSACTIONS\" | \"DELETE_ACCOUNT\" | \"REPORT_ISSUE\",\n\
```

Replace `\"CATEGORY_BALANCE\" | \"SEARCH_TRANSACTIONS\"` with
`\"CATEGORY_BALANCE\" | \"CATEGORY_AFFORDABILITY\" | \"SEARCH_TRANSACTIONS\"` (i.e. insert the new
action right after `CATEGORY_BALANCE`, leaving everything else on the line unchanged).

- [ ] **Step 2: Add system-prompt rule 20d**

In `backend/src/rag.rs`, find rule `20c` (the `CATEGORY_BALANCE` rule, around line 1406) and the
two `{}\n\` placeholder lines immediately after it (around line 1407-1408 — these are the
`EDIT_TRANSACTION_RULE` and `DELETE_TRANSACTION_RULE` format placeholders, do NOT touch them).
Insert a NEW literal line between rule 20c's line and the first placeholder line:

```rust
         20d. CATEGORY_AFFORDABILITY: If the user asks whether they can afford to spend a specific amount on a SINGLE category (e.g. 'Can I spend $50 on Entertainment?', 'Do I have $30 left for Dining Out?'), set 'action' to 'CATEGORY_AFFORDABILITY', populate 'category_name' with the named category and 'amount' with the requested amount. This runs an EXACT, current-period-scoped comparison against that category's actual limit/spent/remaining and appends a deterministic yes/no answer with the limit/spent/remaining/requested figures to your reply automatically — do NOT compute or state whether they can afford it yourself in 'response_text' (the same class of bug CATEGORY_BALANCE (rule 20c) exists to prevent, now for an affordability comparison instead of a plain balance readout). Give a brief natural transition instead, e.g. \"Let me check that for you.\" If they name a category but no dollar amount, set 'action' to 'NONE' and ask how much they'd like to spend. AMBIGUOUS AMOUNTS (voice dictation): the SAME ambiguity check from rule 1 applies here — if the stated amount looks like a dictation-mangled, decimal-less run-together figure (e.g. 19204 for $192.04) or you are not confident you parsed it correctly, do NOT set 'action' to 'CATEGORY_AFFORDABILITY' yet; set 'action' to 'NONE', state your single most-likely interpretation, and ask the user to confirm before checking — resolved the same way as rule 1 (a plain affirmative next message uses your proposed amount; a restated amount overrides it). If you cannot tell which category they mean, set 'action' to 'NONE' and ask a clarifying question instead of guessing.\n\
```

So the surrounding block reads (rule 20c's line, then the new 20d line, then the pre-existing
placeholder lines, unchanged):

```rust
         20c. CATEGORY_BALANCE: ... (existing text, unchanged) ...\n\
         20d. CATEGORY_AFFORDABILITY: ... (the new line from above) ...\n\
         {}\n\
         {}\n\
         22. ALWAYS produce perfectly clean, valid, parseable JSON only.",
```

- [ ] **Step 3: Add the `affordability_md` local variable**

In `backend/src/rag.rs`, find the `category_balance_md` declaration (around line 1938):

```rust
    let mut category_balance_md: Option<String> = None;
```

Add immediately after it:

```rust
    // Deterministic yes/no answer for the read-only CATEGORY_AFFORDABILITY action
    // (nels#302). Mirrors category_balance_md: appended as a plain addendum to
    // response_text, never routed through mutation_log/mutation_error (that
    // channel is reserved for genuine DB read failures — see the match arm).
    let mut affordability_md: Option<String> = None;
```

- [ ] **Step 4: Add the `"CATEGORY_AFFORDABILITY"` match arm**

In `backend/src/rag.rs`, find the end of the `"CATEGORY_BALANCE"` match arm (it ends with a closing
`}` for that arm, immediately followed by the `// SEARCH_TRANSACTIONS (#195): ...` comment, around
line 3328-3329). Insert the new arm between them:

```rust
        "CATEGORY_AFFORDABILITY" => {
            // Deterministic single-category affordability answer (nels#302): compares a
            // requested spend amount against the category's actual current-period effective
            // remaining balance (fund-aware, mirroring CATEGORY_BALANCE's own limit/spent
            // computation so the two actions can never disagree about what "remaining" means for
            // the same category) — no LLM arithmetic. Four outcomes, extending CATEGORY_BALANCE's
            // three: missing category -> clarifying prompt, missing amount -> "how much?" prompt,
            // unresolvable category -> the same not-found convention, DB failure ->
            // mutation_error, found -> deterministic yes/no verdict via affordability_md (never
            // categories_table_html — this answer is prose, not a table).
            if let Some(bid) = active_budget_id {
                let cat_name = parsed_ai_res.action_params.as_ref()
                    .and_then(|p| p.category_name.clone())
                    .filter(|cn| !cn.trim().is_empty());
                let requested = parsed_ai_res.action_params.as_ref().and_then(|p| p.amount);
                match (cat_name, requested) {
                    (None, _) => {
                        affordability_md = Some(
                            "Which category would you like to check?".to_string(),
                        );
                    }
                    (Some(cn), None) => {
                        affordability_md = Some(format!(
                            "How much would you like to spend on {}?",
                            cn.trim()
                        ));
                    }
                    (Some(cn), Some(amount)) => match resolve_category_balance(&state.db, bid, cn.trim()).await {
                        Ok(Some(row)) => {
                            affordability_md = Some(format_affordability_message(&row.name, &row, amount));
                        }
                        Ok(None) => {
                            affordability_md = Some(category_not_found_message(&cn));
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                budget_id = %bid,
                                "CATEGORY_AFFORDABILITY: failed to resolve category balance"
                            );
                            mutation_error = Some(
                                "I couldn't check that category's balance right now. Please try again."
                                    .to_string(),
                            );
                        }
                    },
                }
            }
        }
```

- [ ] **Step 5: Append `affordability_md` to the final response text**

In `backend/src/rag.rs`, find the `category_balance_md` append (around line 3548-3551):

```rust
    // Append the CATEGORY_BALANCE clarifying/not-found prose (nels#282) as a
    // read-only addendum, mirroring transactions_list_md above.
    if let Some(results) = &category_balance_md {
        final_response_text = format!("{}\n\n{}", final_response_text, results);
    }
```

Add immediately after it:

```rust
    // Append the CATEGORY_AFFORDABILITY deterministic yes/no answer (nels#302) as
    // a read-only addendum, mirroring category_balance_md above.
    if let Some(results) = &affordability_md {
        final_response_text = format!("{}\n\n{}", final_response_text, results);
    }
```

- [ ] **Step 6: Wire the offline matcher into `chat_endpoint`'s offline chain**

**Important ordering note (spec A5):** the new branch must run before BOTH
`offline_list_transactions_category` and `offline_category_balance_action` — not just the latter.
`offline_list_transactions_category` claims any message containing "spend"/"spent"/"spending"/
"transaction" (and trips none of its own ADD_TRANSACTION/EDIT_TRANSACTION/"recategorize" exclusion
guards for a plain "Can I spend $50 on Entertainment?"), so if the new branch were inserted only
before `offline_category_balance_action` (i.e. after `offline_list_transactions_category` has
already run), the ticket's own AC #1 example would be misrouted to `LIST_TRANSACTIONS` and never
reach `CATEGORY_AFFORDABILITY` at all.

In `backend/src/rag.rs`, find the boundary between the `offline_privacy_action` branch and the
`offline_list_transactions_category` branch (around line 1595-1607):

```rust
        } else if let Some(privacy_action) = offline_privacy_action(&msg_lower) {
            // Privacy routing lives in `offline_privacy_action` (pinned by a unit
            // test) and runs EARLY so the generic "save"/"delete" branches below
            // never shadow "export my data" or "delete my account". Both actions
            // are advisory only — they never mutate or export anything here.
            action = privacy_action.to_string();
            response_text = if privacy_action == "DELETE_ACCOUNT" {
                "(Mock AI Offline Mode) Deleting your account is permanent and removes all your data. For your security it must be confirmed in Settings: open the sidebar account menu, choose \"Delete my account\", then type your email and a current authenticator code to confirm. I can't delete an account from chat.".to_string()
            } else {
                "(Mock AI Offline Mode) You can export all your data as a JSON file from the account menu: open the sidebar account menu and choose \"Export my data\". It includes your budgets, categories, transactions, goals, chat history, and account info.".to_string()
            };

        } else if let Some(cat_name) = offline_list_transactions_category(&payload.message) {
```

Insert a NEW branch between them (so it is tried before BOTH `offline_list_transactions_category`
AND `offline_category_balance_action`), leaving everything else in the chain — including the
existing `offline_category_balance_action` branch further down — completely unchanged:

```rust
        } else if let Some(privacy_action) = offline_privacy_action(&msg_lower) {
            // Privacy routing lives in `offline_privacy_action` (pinned by a unit
            // test) and runs EARLY so the generic "save"/"delete" branches below
            // never shadow "export my data" or "delete my account". Both actions
            // are advisory only — they never mutate or export anything here.
            action = privacy_action.to_string();
            response_text = if privacy_action == "DELETE_ACCOUNT" {
                "(Mock AI Offline Mode) Deleting your account is permanent and removes all your data. For your security it must be confirmed in Settings: open the sidebar account menu, choose \"Delete my account\", then type your email and a current authenticator code to confirm. I can't delete an account from chat.".to_string()
            } else {
                "(Mock AI Offline Mode) You can export all your data as a JSON file from the account menu: open the sidebar account menu and choose \"Export my data\". It includes your budgets, categories, transactions, goals, chat history, and account info.".to_string()
            };

        } else if let Some((cat_name, amount_opt)) = offline_category_affordability_action(&payload.message) {
            // Offline fallback for CATEGORY_AFFORDABILITY (nels#302): deterministic
            // affordability-question routing. Routed BEFORE offline_list_transactions_category
            // AND offline_category_balance_action: "Can I spend $50 on X" contains "spend" (an
            // offline_list_transactions_category trigger word) and "do I have $30 left for X"
            // contains "left" (a CATEGORY_BALANCE trigger word), so the affordability question
            // must claim both classes of phrasing before either of those matchers gets a look.
            action = "CATEGORY_AFFORDABILITY".to_string();
            action_params.category_name = Some(cat_name.clone());
            match amount_opt {
                Some(amt) => {
                    action_params.amount = Some(amt);
                    response_text = format!("(Mock AI Offline Mode) Let me check your {} balance.", cat_name);
                }
                None => {
                    response_text = format!("(Mock AI Offline Mode) How much would you like to spend on {}?", cat_name);
                }
            }

        } else if let Some(cat_name) = offline_list_transactions_category(&payload.message) {
```

- [ ] **Step 7: Run the full backend test suite**

Run: `cd backend && cargo test 2>&1 | tail -40`
Expected: all tests pass (`test result: ok. NNN passed`), including every test added in Tasks 1-2.

- [ ] **Step 8: Run clippy**

Run: `cd backend && cargo clippy --all-targets 2>&1 | tail -40`
Expected: no warnings/errors (clean, matching the repo's existing clippy-clean baseline).

- [ ] **Step 9: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#302): wire CATEGORY_AFFORDABILITY into the chat action dispatch"
```

---

### Task 4: Add an offline dispatch integration test proving end-to-end routing

**Files:**
- Modify: `backend/src/rag.rs` (add a test near the existing offline-dispatch-style tests; see
  Step 1 for exactly which existing test to use as the placement anchor)

This closes the loop between Tasks 1-3: Tasks 1-2 unit-test the pure helpers in isolation, and
Task 3 wires them together, but nothing yet proves `offline_category_affordability_action`'s
output actually flows through `format_affordability_message` and `category_not_found_message`
correctly when combined. This task adds pure, DB-free coverage of that combination without
standing up a database (unlike `resolve_category_balance`, which needs one and is therefore
`#[ignore]`d elsewhere in this file — this task deliberately avoids adding a new `#[ignore]` test,
since the ticket's AC only requires `cargo test -p backend` — the DEFAULT, non-ignored run — to
pass).

- [ ] **Step 1: Write the failing test**

Find `format_affordability_message_fund_uses_effective_limit` (added in Task 2, Step 5) in
`backend/src/rag.rs`'s `mod tests` block, and add this test immediately after it:

```rust
    #[test]
    fn offline_category_affordability_action_output_feeds_format_affordability_message_correctly() {
        // End-to-end (offline-router -> formatter) without a database: simulates what
        // chat_endpoint's offline branch + CATEGORY_AFFORDABILITY match arm do together, using a
        // hand-built CategoryTableRow standing in for what resolve_category_balance would return.
        let (name, amount) = offline_category_affordability_action("Can I spend $50 on Entertainment?")
            .expect("must match the core affordability phrasing");
        assert_eq!(name, "Entertainment");
        let requested = amount.expect("a dollar figure was present in the message");

        let row = crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: name.clone(),
            category_type: "expense".to_string(),
            category_limit: Some(200.0),
            spent: 80.0,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let msg = format_affordability_message(&row.name, &row, requested);
        assert!(msg.starts_with("Yes"), "got: {}", msg);
        assert!(msg.contains("120.00"), "must state remaining, got: {}", msg);
    }
```

- [ ] **Step 2: Run the test to verify it fails to compile or fails**

Run: `cd backend && cargo test offline_category_affordability_action_output_feeds 2>&1 | tail -30`
Expected: this should actually PASS immediately, since Tasks 1-3 already implemented both
functions correctly — this step confirms that (if it fails, it means one of the two functions
has a defect Tasks 1-2's isolated unit tests didn't catch; fix the function, not the test).

- [ ] **Step 3: Confirm the test passes**

Run: `cd backend && cargo test offline_category_affordability_action_output_feeds 2>&1 | tail -30`
Expected: `test result: ok. 1 passed`

- [ ] **Step 4: Run the full backend test suite one more time**

Run: `cd backend && cargo test 2>&1 | tail -40`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add backend/src/rag.rs
git commit -m "test(#302): cover offline matcher -> formatter integration without a database"
```
