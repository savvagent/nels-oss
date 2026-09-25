# Ambiguous transaction amount — ask for clarification (savvagent/nels#257) — Design

**Date:** 2026-07-03
**Status:** Draft (revision 2, addressing spec-critique round 1)

## Brief (verbatim from the ticket)

> ## Summary
>
> When a user dictates a transaction amount by voice, an ambiguous transcript
> is logged verbatim as a number with no sanity check. Reported case: the
> user spoke **"one ninety two o 4"** (intended **$192.04**) and Nels logged
> **$19204**.
>
> The AI should recognize when a parsed amount is ambiguous — especially a
> large, round, decimal-less number that arrived via dictation — and ask the
> user to confirm the amount instead of silently logging it.
>
> ## Root cause
>
> Voice input is browser-native speech-to-text (`frontend/src/lib/speech.svelte.js`,
> Web Speech API). The recognizer returns a best-guess string (e.g.
> `"one ninety two o 4"` / `"19204"`) that is inserted into the chat input as
> literal text — there is no number normalization on the client.
>
> That raw text is sent to the LLM, which extracts `amount` for
> `ADD_TRANSACTION` per the system prompt in `backend/src/rag.rs`:
>
> - Rule 1 (`ADD_TRANSACTION`, rag.rs:1377) — "Populate 'amount' as positive
>   number" with no guidance about ambiguous or dictation-mangled numbers.
> - Rule 21 (`EDIT_TRANSACTION`, rag.rs:1408) — same gap for amount
>   corrections.
>
> The handler (`backend/src/rag.rs:2717` `ADD_TRANSACTION`) inserts whatever
> `amount` it receives with no confirmation step. Nothing in the pipeline
> flags that a spoken "one ninety two oh four" is far more likely $192.04
> than $19,204.
>
> ## Proposed fix
>
> Add a clarification path in the LLM system prompt so the AI does **not**
> log a transaction when the amount is ambiguous. Instead it should return
> `action: "NONE"` with a `response_text` that restates its best
> interpretation and asks the user to confirm (e.g. *"Did you mean
> **$192.04** or **$19,204**? I'll log it once you confirm."*).
>
> This is chat-driven and reachable through the existing `ADD_TRANSACTION` /
> `EDIT_TRANSACTION` arms — no new action or endpoint needed.
>
> Ambiguity signals the rule should call out:
> - A decimal-less integer that is large/round enough to plausibly be
>   dollars-and-cents run together (the "o/oh = zero" spoken-number failure
>   mode).
> - Any amount the model is not confident it parsed correctly from the
>   phrasing.
>
> When the user *does* state an unambiguous amount (`$15`, `fifteen
> dollars`, `192 dollars and 4 cents`), behavior is unchanged — log it
> directly.
>
> ## Scope / affected code
> - `backend/src/rag.rs` — system prompt rule 1 (ADD_TRANSACTION) and rule 21
>   (EDIT_TRANSACTION); add the clarify-on-ambiguity instruction.
> - No schema, migration, or frontend change required. (Optional future
>   enhancement: client-side spoken-number normalization in
>   `speech.svelte.js`, out of scope here.)
>
> ## Acceptance criteria
> - Dictating "one ninety two o 4" (or similar) yields a clarifying
>   question, not a logged $19204 transaction.
> - Confirming the amount then logs the correct value.
> - Unambiguous amounts still log in one turn with no extra prompt.
>
> ## Testing notes
> - The offline/mock router (`rag.rs:1803`) parses amounts deterministically
>   and is not affected, but existing ADD_TRANSACTION prompt-behavior tests
>   live alongside it — add coverage asserting the clarify path for an
>   ambiguous amount and the unchanged happy path for an explicit one.

(Line numbers above are as filed; current locations were re-verified from
source per the ticket's own caveat — see Architecture below: rule 1 is now at
`rag.rs:1377`, rule 21 at `rag.rs:1408`, the offline router's ADD_TRANSACTION
branch at `rag.rs:1803`\-1836, and the ADD_TRANSACTION handler at
`rag.rs:2717` — all consistent with the filed line numbers, confirmed
unchanged since filing.)

## Assumptions

1. **No new action, no code-path change is needed for "NONE" to be safe.**
   Verified in source: `chat_endpoint`'s action dispatch (`backend/src/rag.rs`,
   the `match parsed_ai_res.action.as_str()` block ending in a catch-all
   `_ => {}` at line 3409) already treats any action other than an
   explicitly-matched one as a no-op — no transaction is inserted, no audit
   row is written — while `response_text` is *always* returned to the user as
   the chat reply. So `action: "NONE"` with a clarifying `response_text` is
   already a fully safe, working path; this ticket is prompt-text-only,
   exactly as scoped.
2. **The offline/mock router is correctly out of scope.** `chat_endpoint`
   only consults Gemini's real response (and therefore the system prompt) when
   `GEMINI_API_KEY` is set and non-empty; otherwise it runs a separate,
   deterministic keyword/regex offline router (the `msg_lower.contains("log")`
   branch at `rag.rs:1803`) that never reads the system prompt at all. The
   ticket's own testing note confirms this ("not affected" — quoted verbatim
   above). No offline-router code changes are in scope.
3. **No live-LLM test harness exists in this repo, and none will be added.**
   There is no Gemini API-base test seam (unlike `GITHUB_API_BASE` /
   `STRIPE_API_BASE` for other integrations) and no `wiremock`/`mockito`
   dependency in `backend/Cargo.toml`. The only tests that exercise
   `chat_endpoint` end-to-end are `#[ignore]` Postgres-backed tests that run
   with `GEMINI_API_KEY` unset (i.e. through the offline router, per
   Assumption 2) — they cannot observe real-Gemini prompt-following behavior,
   and this ticket does not add that infrastructure (out of scope: the ticket
   scopes the change to the prompt text in `rag.rs` only). Consequently, the
   ticket's testing note ("add coverage asserting the clarify path... and the
   unchanged happy path", quoted verbatim above) is interpreted, as this
   spec's own testing-strategy decision, to mean: **add deterministic unit
   tests over the prompt *text itself*** — asserting rule 1 and rule 21
   contain the new ambiguity-clarification guidance, the specific dictation
   failure mode ("oh"/"o" → zero), the `action: "NONE"` instruction, the
   confirmation-resolution wording (Assumption 7 below), and that the
   unambiguous-amount happy path (`$15`, `fifteen dollars`, `192 dollars and
   4 cents`) is still explicitly called out as NOT requiring confirmation.
   This is a content/regression guard on the prompt, not a behavioral test of
   the LLM (which is untestable in this repo without a live key) — a
   documented, unavoidable limitation of the scope as given, not a gap this
   change should silently paper over.
4. **Real-LLM verification is a manual/production step, not CI.** No
   `GEMINI_API_KEY` is available in this environment, so I cannot exercise
   the real Gemini call end-to-end during development or CI. The unit tests
   in Assumption 3 are the automated safety net; true behavioral verification
   (does the deployed model actually ask for clarification on "one ninety two
   o 4", and correctly resolve a plain "yes"?) is a manual smoke check to be
   run by whoever holds a Gemini key — called out explicitly in the close-out,
   not silently skipped.
5. **The prompt text needs to become independently unit-testable.** Today the
   entire system prompt — including rules 1–22 — is one giant inline
   `format!` string literal inside `chat_endpoint`, with no accessible name.
   To satisfy Assumption 3's testing approach without a large, risky
   restructure, this change extracts *only* rule 1's and rule 21's paragraph
   text into two new private `const ADD_TRANSACTION_RULE: &str` /
   `const EDIT_TRANSACTION_RULE: &str` module-level constants (verbatim
   original wording + the new clarification clause appended), referenced from
   the existing `format!` call via two new `{}` placeholders inserted exactly
   where the literal text used to sit, with the two constants appended as the
   final two positional arguments (after `usage_context`, which is already
   the last argument consumed before the literal CRITICAL RULES text begins —
   confirmed by reading the current argument list and placeholder order).
   This is a minimal, structurally-safe change: it does not touch any other
   rule's text, does not change the 8 pre-existing placeholders/arguments, and
   the resulting runtime prompt string is byte-for-byte the same as if the
   text had stayed inline (format! concatenation is order-preserving).
6. **Ambiguity heuristic is expressed in natural language, not code.** Per
   the ticket's proposed fix, the detector lives entirely in the LLM's
   judgment, guided by prompt wording — there is no new Rust parsing/regex
   logic. The wording gives the model a concrete rule of thumb (a
   decimal-less integer large/round enough to plausibly be a
   dollars-and-cents run-together dictation artifact — the "19204 ⇒ $192.04"
   pattern) plus a confidence fallback ("any amount you are not confident you
   parsed correctly"), and explicit negative examples ($15, "fifteen
   dollars", "192 dollars and 4 cents") so ordinary amounts are unaffected.
   This mirrors how every other judgment call in this prompt (e.g. rule 14's
   "budget maturity" coaching, rule 19's issue-worthiness) is expressed —
   natural-language guidance, not code — so it's consistent with the file's
   existing style.
7. **Rule 21 delegates to rule 1 rather than repeating the heuristic.** To
   avoid prompt bloat and drift between two independent descriptions of the
   same heuristic, rule 21's addition is a short cross-reference ("the same
   AMBIGUOUS AMOUNTS check from rule 1 applies here") rather than restating
   the full heuristic. This matches existing prompt style, which already
   cross-references other rules by number (e.g. rule 2k → "route it per rule
   3").
8. **Confirmation-resolution is explicitly specified (addresses spec-critique
   round 1, issue 2).** AC #2 ("Confirming the amount then logs the correct
   value") must survive the common case where the user's confirmation is a
   bare affirmative ("yes", "that's right", "correct") rather than a
   restated number — the ticket's own example question offers TWO candidates
   ("$192.04 or $19,204?"), so a bare "yes" is inherently ambiguous unless the
   rule text resolves it. The rule text therefore requires the model to:
   (a) always state its SINGLE most-likely interpretation first, with any
   alternative reading mentioned only as a secondary aside (matching the
   ticket's own example, where $192.04 — the cents-shaped reading — is named
   first); and (b) explicitly instructs that a plain affirmative in the
   user's next message confirms that PRIMARY stated interpretation, which the
   model should then log via ADD_TRANSACTION/EDIT_TRANSACTION — while a
   message naming a different concrete amount instead uses that one. This
   makes the confirmation turn deterministic in the common "yes" case without
   any new state field (the "RECENT CHAT HISTORY" context already gives the
   model its own prior turn for grounding, exactly as goal-progress/coaching
   context works elsewhere in this prompt) — consistent with the ticket's
   explicit "no schema/migration" scope note.

## Goal & Success Criteria

Update the Gemini system prompt's ADD_TRANSACTION (rule 1) and
EDIT_TRANSACTION (rule 21) instructions so the model recognizes a dictation-
mangled, decimal-less, large/round dollar amount (or any amount it isn't
confident it parsed correctly) as ambiguous, and responds with a clarifying
question (`action: "NONE"`) instead of logging/editing a transaction with
that amount — while leaving unambiguous amounts logging in a single turn,
exactly as today, and while making a plain "yes"-style confirmation resolve
deterministically to the stated best interpretation.

- [ ] Rule 1's text instructs the model to detect ambiguous amounts and
      route to `action: "NONE"` with a confirming `response_text` instead of
      `ADD_TRANSACTION`, citing the "oh/o → zero" dictation failure mode.
- [ ] Rule 1's text requires the clarifying question to state ONE primary
      best-guess amount (not just two equally-weighted options), and states
      that a plain affirmative reply confirms that primary amount — which
      the model then logs via ADD_TRANSACTION on the user's next turn (AC #2).
- [ ] Rule 21's text applies the same ambiguity check AND the same
      confirmation-resolution behavior to `EDIT_TRANSACTION`'s new `amount`.
- [ ] Both rules explicitly state that a clearly-stated amount ($15, "fifteen
      dollars", amounts with cents) is NOT ambiguous and logs directly, no
      extra prompt (AC #3).
- [ ] The two rule strings are extracted into independently unit-testable
      constants; unit tests assert the clarify-path guidance, the
      confirmation-resolution guidance, and the unchanged-happy-path
      guidance are all present, for both rules.
- [ ] `cargo test` (unit, no DB) and `cargo build`/`cargo check` pass; no
      existing test's expectations change (the offline router and all
      REST/DB-backed tests are untouched — confirmed by reading the relevant
      code paths).
- [ ] `cargo fmt`/no new clippy warnings on the touched code.

## Scope

**In scope:**
- `backend/src/rag.rs`: rule 1 and rule 21 text (content change), extracted
  into two new module-level `const` string literals, wired into the existing
  `format!` call via two new placeholders/arguments.
- New unit tests (in the existing `#[cfg(test)] mod tests` block) asserting
  the new guidance text is present in both rules.

**Out of scope (explicitly, per the ticket):**
- Any DB schema/migration change.
- Any frontend change (`speech.svelte.js` client-side number normalization is
  named in the ticket as a future, out-of-scope enhancement).
- Any new chat `action` value or new endpoint.
- Any change to the offline/mock router.
- Adding a live-Gemini test harness (API-base test seam, wiremock, etc.) —
  not requested, and a nontrivial addition on its own; noted as a possible
  future improvement, not attempted here.

## Architecture

No architectural change. `chat_endpoint` (`backend/src/rag.rs`) already:
1. Builds `system_instructions` via one `format!` call (rules 1–22 inline,
   rule 1 at line 1377, rule 21 at line 1408 — current line numbers, re-
   verified from source, matching the numbers as filed).
2. Calls Gemini with that system prompt when `GEMINI_API_KEY` is set,
   otherwise runs the offline router at line 1803 (unaffected).
3. Dispatches on `parsed_ai_res.action` via a `match` (starting ~line 2717
   for ADD_TRANSACTION, ~line 3402 for EDIT_TRANSACTION); any action not
   explicitly handled (including `"NONE"`) falls into the `_ => {}` no-op arm
   at line 3409.
4. Always returns `parsed_ai_res.response_text` as the chat reply.

This change only edits the content of two rule strings consumed by step 1;
steps 2–4 are unmodified and require no code changes to support the new
"ask, don't log" behavior — the mechanism already exists for every other rule
that routes to `action: "NONE"` (e.g. rule 5's Q&A path, rule 12's
budget-not-found path).

**New constants** (module level, near the top of `rag.rs` or immediately
above `chat_endpoint`, matching where other prompt-related helpers/constants
already live):

```rust
const ADD_TRANSACTION_RULE: &str = "1. ... <original text> ... \
     AMBIGUOUS AMOUNTS (voice dictation): ... always state ONE primary
     best-guess amount ... a plain 'yes'/'that's right' confirms that
     primary amount ...";

const EDIT_TRANSACTION_RULE: &str = "21. ... <original text> ... \
     The same AMBIGUOUS AMOUNTS check (and confirmation handling) from
     rule 1 applies here...";
```

Referenced in the existing `format!(...)` call as two new `{}` placeholders
at rule 1's and rule 21's original textual positions, with the two constants
appended as the 9th/10th positional arguments (immediately after
`usage_context`, the last argument currently consumed before the literal
CRITICAL RULES text begins).

## Error Handling & Edge Cases

- **Prompt-only change, no new failure mode.** No new Result/Option, no new
  DB call, no new network call. The change cannot introduce a new panic,
  error branch, or fallback path — it only changes what text the model
  reads.
- **Model non-compliance is a pre-existing risk, not a new one.** As with
  every other prompt rule, the model could still ignore the instruction
  (this is fundamentally the risk profile of prompt-engineering, no
  different from any other rule in this file). No code-level enforcement is
  added or possible within this ticket's scope (a code-level fallback would
  require parsing "amount" heuristically server-side, explicitly not what
  the ticket asks for — the proposed fix is prompt-only).
- **Rule 21 edge case — clarifying an edit vs. an add.** The model must not
  confuse "ambiguous NEW amount while editing" with "ambiguous locate-phrase"
  (`transaction_match`) — the new clause is scoped explicitly to "the new
  'amount' value," leaving `transaction_match` disambiguation (already an
  existing, separate concern) untouched.
- **Bare-affirmative confirmation (addresses spec-critique round 1, issue
  2).** Handled by Assumption 8 — the rule text requires a single stated
  primary interpretation so "yes" is never ambiguous about which amount it
  confirms.

## Testing Approach

Per Assumption 3, no live-LLM behavioral test is possible in this repo.
Coverage added:

1. **Unit tests (no DB, no network)** in `backend/src/rag.rs`'s existing
   `#[cfg(test)] mod tests`, alongside the other pure-string/const tests
   (e.g. `vector_to_string_formats_pgvector_literal`):
   - `add_transaction_rule_flags_ambiguous_dictated_amounts` — asserts
     `ADD_TRANSACTION_RULE` mentions ambiguity detection, the "oh/o → zero"
     dictation pattern, and instructs `action: "NONE"` instead of
     `ADD_TRANSACTION` when ambiguous.
   - `add_transaction_rule_resolves_plain_confirmation_to_primary_amount` —
     asserts `ADD_TRANSACTION_RULE` requires a single primary interpretation
     and states that a plain affirmative confirms it (AC #2 coverage).
   - `add_transaction_rule_keeps_unambiguous_amounts_logging_directly` —
     asserts `ADD_TRANSACTION_RULE` still explicitly says a clear amount (a
     concrete dollar figure / one with cents) logs directly, no extra prompt
     (AC #3 coverage).
   - `edit_transaction_rule_flags_ambiguous_amounts_and_cross_references_rule_1`
     — same shape for `EDIT_TRANSACTION_RULE`, plus asserting the rule-1
     cross-reference is present.
2. **Existing test suite unaffected**: `cargo test` (unit) and the
   `--ignored` Postgres-backed tests (not run in this environment, but
   reasoned about via source reading) exercise the offline router or
   permission/formatting helpers, none of which read `ADD_TRANSACTION_RULE`
   / `EDIT_TRANSACTION_RULE` — confirmed no existing test asserts on the
   literal old rule-1/rule-21 text.
3. **Manual/production verification** (documented, not automated): once
   deployed, a person with `GEMINI_API_KEY` access should dictate "one ninety
   two o 4" (or type it) and confirm Nels asks for clarification rather than
   logging $19,204, reply "yes" and confirm it logs $192.04 (not $19,204),
   and confirm that an unambiguous "log $15 on food" still logs in one turn.
   This is called out explicitly in the close-out summary as a follow-up
   manual check, consistent with the "target verification" step of the wider
   ship process.

## Risks & Open Questions

- **Heuristic calibration risk**: a "large, round, decimal-less" wording
  could be interpreted too aggressively by the model (e.g. flagging a
  legitimate "$1500 rent payment" or "log 2000 for the kitchen project" as
  ambiguous) or too loosely (missing genuine dictation artifacts). Mitigated
  by explicit negative examples in the rule text ($15, "fifteen dollars",
  "192 dollars and 4 cents") and by keeping the primary signal tied to the
  *dictation* framing (decimal-less AND large/round AND plausibly a
  dollars-cents concatenation) rather than "any large number." This can't be
  proven correct without a live model in the loop; it's the best-effort
  textual heuristic the ticket asks for, and is the same class of judgment
  call the rest of the prompt already makes (e.g. rule 19's "when to file an
  issue"). Flagged here rather than hidden.
- **No enforcement backstop**: since this is prompt-only, a determined or
  confused model could still log an ambiguous amount, or misresolve a
  confirmation. If this proves insufficient in production, a follow-up
  ticket could add a server-side sanity check (e.g. flag decimal-less
  amounts above some threshold before insert) — explicitly out of scope here
  per the ticket's own scope note, so not attempted.
- **Test coverage is necessarily indirect** (Assumption 3): the added tests
  verify the prompt *says the right thing*, not that Gemini *does* the right
  thing. This is a known, stated limitation, not an oversight.
