# Ambiguous Transaction Amount Clarification (savvagent/nels#257) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update the Gemini system prompt's ADD_TRANSACTION (rule 1) and EDIT_TRANSACTION (rule 21) instructions in `backend/src/rag.rs` so the AI recognizes a dictation-mangled, decimal-less, large/round dollar amount as ambiguous and asks the user to confirm (`action: "NONE"`) instead of silently logging/editing a transaction with that amount — while unambiguous amounts still log in one turn, and a plain "yes"-style confirmation resolves deterministically to the AI's stated best interpretation.

**Architecture:** No code-path change — `action: "NONE"` already falls through the existing `_ => {}` no-op arm in `chat_endpoint`'s action dispatch, so this is a prompt-text-only change. Rule 1's and rule 21's paragraph text is extracted from the single giant inline `format!` literal into two new module-level `const ADD_TRANSACTION_RULE: &str` / `const EDIT_TRANSACTION_RULE: &str` constants (original wording + new ambiguity-clarification clause appended), wired back into the same `format!` call via two new placeholders/arguments. This makes the new guidance independently unit-testable without a live Gemini call.

**Tech Stack:** Rust (axum), no new dependencies.

**Full spec:** recorded as a `[Spec]` comment on `savvagent/nels#257` (https://github.com/savvagent/nels/issues/257#issuecomment-4879204934).

---

## Repo-specific notes for this task

- Test command: `cd backend && cargo test` (this task's tests are plain `#[test]` — no DB, no `--ignored` needed). Lint: `cd backend && cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` if you want a stricter local check (CI conventions were not independently confirmed beyond `cargo test`/`cargo check`, so `cargo check` is the minimum bar).
- No migration, no config, no infra, no feature flag — this ticket has no out-of-band artifacts (confirmed: the change is two `const` string edits + a `format!` call touch-up + unit tests, nothing else).
- Commit message convention (from `git log`): `fix(#257): <subject>` (this repo prefixes fix/feat commits with the bare issue number in parens, no `savvagent/nels` prefix, e.g. `fix(#246): give fetchApi an AbortController-based timeout (#248)` — the trailing `(#NNN)` is the PR number, added automatically by the squash-merge, not something to type by hand).
- Work in the worktree at `/home/robhicks/dev/nels/.worktrees/issue-257-ambiguous-amount-clarify` on branch `issue-257-ambiguous-amount-clarify`. Do NOT touch `frontend/src/App.svelte`, `router.js`, or `backend/src/budget.rs` — a concurrent job (nels#241) is working those files in a different worktree on the same repo.

---

## Task 1: Extract rule 1 & rule 21 into testable constants and add the ambiguity-clarification instruction

**Files:**
- Modify: `backend/src/rag.rs` (module-level consts near line 680-681, the `format!` call around lines 1322-1418, rule 1's literal text at line 1377, rule 21's literal text at line 1408)
- Test: `backend/src/rag.rs` (existing `#[cfg(test)] mod tests` block, starts at line 5149)

### Step 1: Write the failing tests

Open `backend/src/rag.rs`, find the `#[cfg(test)] mod tests` block (`mod tests {` at line 5149, immediately followed by `use super::*;`). Add these four tests directly after the `use super::*;` line (i.e. as the new first tests in the module, right before the existing `usage_metadata_deserializes_and_coerces_missing_to_zero` test):

```rust
    // #257: rule 1 (ADD_TRANSACTION) must flag a dictation-mangled, ambiguous
    // amount and route to action NONE instead of logging it. Pure string
    // assertions over the prompt constant — no DB, no network, no live LLM
    // (there is no Gemini test harness in this repo); this is a content/
    // regression guard on the prompt text itself, not a behavioral test of
    // what Gemini actually does with it.
    #[test]
    fn add_transaction_rule_flags_ambiguous_dictated_amounts() {
        assert!(
            ADD_TRANSACTION_RULE.contains("AMBIGUOUS AMOUNTS"),
            "rule 1 must define an AMBIGUOUS AMOUNTS check"
        );
        assert!(
            ADD_TRANSACTION_RULE.contains("oh'/'o' (meaning zero)"),
            "rule 1 must call out the spoken oh/o -> zero dictation failure mode"
        );
        assert!(
            ADD_TRANSACTION_RULE.contains("set 'action' to 'NONE' instead"),
            "rule 1 must route ambiguous amounts to action NONE instead of ADD_TRANSACTION"
        );
    }

    // #257 AC 2 ("Confirming the amount then logs the correct value"): a bare
    // "yes" to a two-candidate clarifying question is ambiguous about WHICH
    // amount it confirms unless the rule requires a single primary
    // interpretation and defines how a plain affirmative resolves to it.
    #[test]
    fn add_transaction_rule_resolves_plain_confirmation_to_primary_amount() {
        assert!(
            ADD_TRANSACTION_RULE.contains("SINGLE most-likely interpretation"),
            "rule 1 must require stating one primary interpretation, not two equally-weighted options"
        );
        assert!(
            ADD_TRANSACTION_RULE.contains("plain affirmative"),
            "rule 1 must define how a bare 'yes' resolves to the primary amount"
        );
    }

    // #257 AC 3 ("Unambiguous amounts still log in one turn with no extra
    // prompt"): the happy path must remain explicitly stated, not just
    // implied by the absence of the ambiguity clause.
    #[test]
    fn add_transaction_rule_keeps_unambiguous_amounts_logging_directly() {
        assert!(
            ADD_TRANSACTION_RULE.contains("NOT ambiguous — log it directly with no extra confirmation step"),
            "rule 1 must explicitly state that a clearly-stated amount still logs directly"
        );
        assert!(
            ADD_TRANSACTION_RULE.contains("192 dollars and 4 cents"),
            "rule 1 must give an explicit unambiguous-amount example (a spoken cents figure)"
        );
    }

    // #257: rule 21 (EDIT_TRANSACTION) must apply the same ambiguity check to
    // a corrected amount, cross-referencing rule 1 rather than re-deriving
    // the heuristic (avoids prompt bloat / drift between two descriptions of
    // the same rule).
    #[test]
    fn edit_transaction_rule_flags_ambiguous_amounts_and_cross_references_rule_1() {
        assert!(
            EDIT_TRANSACTION_RULE.contains("AMBIGUOUS AMOUNTS"),
            "rule 21 must reference the AMBIGUOUS AMOUNTS check"
        );
        assert!(
            EDIT_TRANSACTION_RULE.contains("rule 1"),
            "rule 21 must cross-reference rule 1's heuristic rather than repeat it"
        );
        assert!(
            EDIT_TRANSACTION_RULE.contains("set 'action' to 'NONE'"),
            "rule 21 must route an ambiguous corrected amount to action NONE instead of EDIT_TRANSACTION"
        );
    }
```

### Step 2: Run tests to verify they fail

Run: `cd backend && cargo test add_transaction_rule_flags_ambiguous_dictated_amounts add_transaction_rule_resolves_plain_confirmation_to_primary_amount add_transaction_rule_keeps_unambiguous_amounts_logging_directly edit_transaction_rule_flags_ambiguous_amounts_and_cross_references_rule_1`

Expected: a **compile error** — `cannot find value \`ADD_TRANSACTION_RULE\` in this scope` (and same for `EDIT_TRANSACTION_RULE`) — because the constants don't exist yet. A compile failure is the correct "red" state here (the constants are the implementation under test).

### Step 3: Add the two constants

In `backend/src/rag.rs`, find the end of `fetch_budgets_context_rows` (the function ends with `.collect())\n}` at line 680) immediately followed by a blank line and then `pub async fn chat_endpoint(` at line 682. Insert the two new constants between them (i.e. after line 680's closing `}`, before line 682's `pub async fn chat_endpoint(`):

```rust

// Rule 1 (ADD_TRANSACTION) of the chat system prompt's CRITICAL RULES,
// including the #257 ambiguous-amount clarification clause. Kept as its own
// constant (instead of inline in the big `format!` below) so it is
// independently unit-testable without a live Gemini call — see the
// `add_transaction_rule_*` tests.
const ADD_TRANSACTION_RULE: &str = "1. If the user asks to log an expense or transaction (e.g., 'Log $15 spent on Food for dinner'), set 'action' to 'ADD_TRANSACTION'. Populate 'amount' as positive number, 'category_name' (e.g., 'Food'), and 'description'. If category doesn't exist, you should still supply it, and the backend will create it. When a transaction auto-creates a BRAND-NEW category (one that did not already exist), the backend AUTOMATICALLY appends an offer to set that new category's limit to the amount just logged — do NOT write your own limit offer in 'response_text' (that would duplicate it); just confirm the transaction was logged. If the user then accepts (e.g. 'yes', 'sure', 'set it to that'), set 'action' to 'CREATE_CATEGORY' with 'category_name' = that new category and 'category_limit' = that amount. Do NOT set a limit automatically without the user accepting, and do NOT make this offer when the transaction matched an EXISTING category. AMBIGUOUS AMOUNTS (voice dictation): speech-to-text can mangle spoken numbers — most often the word 'oh'/'o' (meaning zero) gets folded into the digits, so a spoken dollars-and-cents amount like 'one ninety two oh four' ($192.04) can arrive as a single decimal-less integer like 19204. Before logging, check whether the amount is AMBIGUOUS: a decimal-less integer that is large and round/structured enough that it plausibly represents dollars-and-cents run together (e.g. 19204 could mean $192.04), or ANY amount you are not confident you parsed correctly from the phrasing. If it is ambiguous, do NOT set 'action' to 'ADD_TRANSACTION' and do NOT log anything yet — set 'action' to 'NONE' instead, state your SINGLE most-likely interpretation FIRST in 'response_text' (mentioning an alternative only as a secondary aside), and ask the user to confirm the exact amount (e.g. \"I think you meant $192.04 — did you mean that, or $19,204? I'll log it once you confirm.\"). If the user's next message is a plain affirmative ('yes', 'yep', 'correct', 'that's right') with no new amount stated, log the PRIMARY amount you proposed via ADD_TRANSACTION; if they instead state a different amount, use that one. A clearly-stated amount ($15, 'fifteen dollars', '192 dollars and 4 cents', an amount already given with a decimal point, or an ordinary small/whole-dollar figure) is NOT ambiguous — log it directly with no extra confirmation step.";

// Rule 21 (EDIT_TRANSACTION) of the chat system prompt's CRITICAL RULES,
// including the #257 ambiguous-amount clarification clause (cross-
// referencing rule 1's heuristic rather than repeating it). See
// `ADD_TRANSACTION_RULE`'s doc comment for why this is its own constant.
const EDIT_TRANSACTION_RULE: &str = "21. EDIT_TRANSACTION: If the user asks to change/correct an EXISTING transaction (e.g. 'change my $5 coffee to $7', 'that grocery charge was actually $40', 'recategorize my Uber as Travel'), set 'action' to 'EDIT_TRANSACTION'. Put a short locator describing WHICH transaction in 'transaction_match' (e.g. '$5 coffee', 'grocery charge'), and the NEW values in 'amount' (new amount), 'category_name' (new category), and/or 'new_description' (new description) — only the fields being changed. Example: 'change the $5 coffee to $7' -> transaction_match='$5 coffee', amount=7. Do NOT use this to create a transaction (that is ADD_TRANSACTION) or to change the date (not supported in chat). AMBIGUOUS AMOUNTS: the same ambiguity check from rule 1 (AMBIGUOUS AMOUNTS) applies to the NEW 'amount' here — if the corrected amount is ambiguous (a decimal-less, dictation-plausible run-together figure, or one you're not confident you parsed correctly), do NOT set 'action' to 'EDIT_TRANSACTION'; set 'action' to 'NONE', state your SINGLE most-likely interpretation, and ask the user to confirm before applying the edit — resolved the same way (a plain affirmative confirms the primary amount you proposed; a different stated amount overrides it).";
```

### Step 4: Wire the constants into the `format!` call, replacing the inline rule text

In the same file, find the `format!` call building `system_instructions` (starts `let system_instructions = format!(` at line 1322).

**4a.** Find rule 1's literal text — the line starting `1. If the user asks to log an expense or transaction` (currently line 1377). Replace the ENTIRE line's string content (from `1. If the user asks...` through `...matched an EXISTING category.`) with a bare `{}` placeholder, keeping the line's existing `\n\` line-ending intact. Before:

```
         1. If the user asks to log an expense or transaction (e.g., 'Log $15 spent on Food for dinner'), set 'action' to 'ADD_TRANSACTION'. Populate 'amount' as positive number, 'category_name' (e.g., 'Food'), and 'description'. If category doesn't exist, you should still supply it, and the backend will create it. When a transaction auto-creates a BRAND-NEW category (one that did not already exist), the backend AUTOMATICALLY appends an offer to set that new category's limit to the amount just logged — do NOT write your own limit offer in 'response_text' (that would duplicate it); just confirm the transaction was logged. If the user then accepts (e.g. 'yes', 'sure', 'set it to that'), set 'action' to 'CREATE_CATEGORY' with 'category_name' = that new category and 'category_limit' = that amount. Do NOT set a limit automatically without the user accepting, and do NOT make this offer when the transaction matched an EXISTING category.\n\
```

After:

```
         {}\n\
```

**4b.** Find rule 21's literal text — the line starting `21. EDIT_TRANSACTION: If the user asks to change/correct an EXISTING transaction` (currently line 1408). Replace the ENTIRE line's string content with a bare `{}` placeholder, keeping the line's existing `\n\` line-ending intact. Before:

```
         21. EDIT_TRANSACTION: If the user asks to change/correct an EXISTING transaction (e.g. 'change my $5 coffee to $7', 'that grocery charge was actually $40', 'recategorize my Uber as Travel'), set 'action' to 'EDIT_TRANSACTION'. Put a short locator describing WHICH transaction in 'transaction_match' (e.g. '$5 coffee', 'grocery charge'), and the NEW values in 'amount' (new amount), 'category_name' (new category), and/or 'new_description' (new description) — only the fields being changed. Example: 'change the $5 coffee to $7' -> transaction_match='$5 coffee', amount=7. Do NOT use this to create a transaction (that is ADD_TRANSACTION) or to change the date (not supported in chat).\n\
```

After:

```
         {}\n\
```

**4c.** Find the argument list at the end of the `format!` call (currently lines 1410-1418):

```rust
        user_email,
        name_context,
        language_context,
        budgets_context,
        budget_context,
        semantic_context,
        history_context,
        usage_context
    );
```

Replace with (add a trailing comma after `usage_context` and append the two new constants as the 9th/10th positional arguments — they must come AFTER `usage_context` because rule 1's and rule 21's new `{}` placeholders appear later in the string than `usage_context`'s placeholder, and `format!` consumes positional args in the order their `{}` appear):

```rust
        user_email,
        name_context,
        language_context,
        budgets_context,
        budget_context,
        semantic_context,
        history_context,
        usage_context,
        ADD_TRANSACTION_RULE,
        EDIT_TRANSACTION_RULE
    );
```

### Step 5: Run tests to verify they pass

Run: `cd backend && cargo test add_transaction_rule_flags_ambiguous_dictated_amounts add_transaction_rule_resolves_plain_confirmation_to_primary_amount add_transaction_rule_keeps_unambiguous_amounts_logging_directly edit_transaction_rule_flags_ambiguous_amounts_and_cross_references_rule_1`

Expected: `test result: ok. 4 passed; 0 failed`.

### Step 6: Run the full unit test suite and a build check

Run: `cd backend && cargo test --lib` (runs all non-`--ignored` tests; the `--ignored` Postgres-backed tests are not run here — no DB is available/needed for this change, and none of them touch `ADD_TRANSACTION_RULE`/`EDIT_TRANSACTION_RULE`).

Expected: all tests pass, no new failures. Then run `cd backend && cargo check` — expected: no errors, no new warnings introduced by this change.

### Step 7: Format

Run: `cd backend && cargo fmt`

This may reformat the two new `const` declarations' surrounding whitespace/attribute lines (rustfmt does not rewrap the string literal's contents, since it cannot split inside a string). Re-run `cargo test --lib` and `cargo check` after formatting to confirm nothing broke.

### Step 8: Commit

```bash
cd /home/robhicks/dev/nels/.worktrees/issue-257-ambiguous-amount-clarify
git add backend/src/rag.rs
git commit -m "$(cat <<'EOF'
fix(#257): ask for clarification on ambiguous dictated transaction amounts

Extract ADD_TRANSACTION (rule 1) and EDIT_TRANSACTION (rule 21) of the
chat system prompt into named constants and add an AMBIGUOUS AMOUNTS
clause: a decimal-less, large/round dollar figure (the "oh"/"o" -> zero
dictation failure mode, e.g. "one ninety two oh four" -> 19204 instead
of $192.04) or any amount the model isn't confident it parsed now
routes to action NONE with a clarifying question instead of logging
the transaction. A plain affirmative reply resolves to the model's
stated primary interpretation; unambiguous amounts still log in one
turn, unchanged.
EOF
)"
```

**Do not run `git push` or open a PR from this task** — that happens after this task is reviewed.
