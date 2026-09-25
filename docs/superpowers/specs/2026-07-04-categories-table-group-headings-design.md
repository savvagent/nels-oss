# Spec: Categories table headings reflect Income/Savings/Expense group semantics (nels#299)

## 1. Brief (verbatim from the ticket)

Repro: open a budget whose categories span more than one group type (Income, Savings,
and/or Expense) — e.g. a "Salary" income category and a "Rent" expense category. Go to the
Categories page.

Expected: since #239, the categories table already groups rows into Income/Savings/Expense
sections (via `<tr class="cat-group">` divider rows in `build_categories_table_html`,
`backend/src/budget.rs`). The column headings should follow the same per-group semantics,
since "Spent"/"Remaining" don't describe income or savings activity:
- No single generic top-level heading spanning the whole table.
- Income group heading: Income, Target, Received, Remaining
- Savings group heading: Savings, Target, Invested, Remaining
- Expense group heading: Expenses, Limit, Spent, Remaining

Actual: the table renders one shared `<thead>` for the entire table regardless of which
group(s) are present: `Category | Limit | Spent | Remaining`
(`build_categories_table_html`, `backend/src/budget.rs`). This heading is accurate for
expense categories but meaningless for Income and Savings groups.

Acceptance criteria (verbatim):
- The categories table has no single table-wide top heading when rendering grouped
  (multi-type) budgets.
- Each Income section is headed "Income, Target, Received, Remaining".
- Each Savings section is headed "Savings, Target, Invested, Remaining".
- Each Expense section is headed "Expenses, Limit, Spent, Remaining".
- Mobile stacked layout (`data-label` / `::before { content: attr(data-label) }` in
  `App.svelte`'s `.cat-table` styles) stays in sync with whichever heading applies to each
  row's group.
- Existing categories-table tests in `backend/src/budget.rs` (`categories_table_tests`) are
  updated to match the new headings.

## 2. Resolving prior ticket comments

The GitHub issue already carried a `[Spec]` (11:09 UTC) and `[Plan]` (11:22 UTC) comment
from an earlier, apparently-abandoned automated run (no PR or branch existed from it before
this work started). That prior spec/plan is **superseded by this one**, for two concrete
reasons — recorded here so the design decision reads as reasoned, not silently
inconsistent with what's on the ticket:

1. It extends the fix to **single-type** budgets too (every single-type budget adopts its
   own type-specific heading, and the `cat-group` mechanism is deleted outright), justified
   by a claim that this was "confirmed by the ticket author." The issue's actual comment/
   timeline history (`gh api repos/savvagent/nels/issues/299/timeline`) shows only
   `assigned` → `labeled` ×2 → the two automated comments themselves — no edit event, no
   third comment, no confirmation exchange of any kind. The claim is unverifiable and this
   spec does not rely on it. It also asserts the original ticket body "briefly omitted
   Savings' Remaining column" — false relative to the live body (`gh issue view 299 --json
   body`), which already lists all three groups with 4 columns including Savings'
   Remaining, and the issue was never edited per the timeline.
2. The live AC's own wording is explicit and narrower than the prior spec assumed: "The
   categories table has no single table-wide top heading when rendering **grouped
   (multi-type)** budgets" — the parenthetical scopes the fix to multi-type budgets. A
   single-type budget (e.g. all-income) never enters the grouped path at all today
   (`distinct_types.len() > 1` gates it), so it isn't reproducible per the issue's own repro
   steps either. This spec (Assumption 1 below) takes that wording at face value.

Mechanically, this spec also differs from the prior plan's approach: the prior plan gives
every present group its own `<thead>`/`<tbody>` pair (multiple `<thead>` elements in one
`<table>` — functional in major browsers per its own analysis, but explicitly
non-conforming to the HTML5 content model, and its own plan flags needing a manual browser
check to confirm no visual misplacement). This spec instead keeps exactly one `<thead>`
(present only for the single-type/flat case) and renders each group's heading as a `<tr
class="cat-group">` of `<th>` cells inside one shared `<tbody>` — standards-conforming,
lower-risk, and no different in browser support surface than the divider row it replaces.

## 3. Assumptions

1. **Scope is limited to the grouped (multi-type) rendering path.** A single-type budget
   (e.g. all-income, all-savings, or all-expense categories, where `distinct_types.len() <=
   1`) keeps today's flat rendering with the generic `Category | Limit | Spent | Remaining`
   `<thead>` and `data-label="Limit"`/`"Spent"` on rows, unchanged. Rationale: the issue's
   repro, "Actual" description, and every AC bullet are explicitly scoped to "grouped
   (multi-type) budgets" — the bug as filed is about the mismatch between per-group
   sections and one shared heading, which by construction only arises when >1 type is
   present (grouping itself is gated on `distinct_types.len() > 1`, per #239). Fixing the
   single-type non-expense case is a natural follow-up but out of scope here. A test
   explicitly pins this boundary so it reads as an intentional scope decision.
2. **The existing per-group divider row (`<tr class="cat-group"><td colspan="4">{label}</td></tr>`)
   is replaced, not supplemented**, by a proper 4-column header row using `<th>` cells:
   `<tr class="cat-group"><th>{Group}</th><th>{amount label}</th><th>{activity
   label}</th><th>Remaining</th></tr>`. Rationale: HTML5 permits only one `<thead>` per
   `<table>`, so per-group headers must live in the body; `<th>` (rather than `<td>`) is the
   semantically correct way to mark these cells as column headers for the rows that follow.
3. **The Expense group's heading label is "Expenses" (plural)**, per the AC text, even
   though the existing divider-row label and `CATEGORY_TABLE_GROUPS` constant currently say
   "Expense" (singular). This is a deliberate rename scoped to the heading row text only —
   no other user-facing "Expense" string, badge, or API field references this specific
   string (verified by repo-wide grep).
4. **Underlying data model is unchanged.** "Target" = `category_limit` (income/savings),
   "Received"/"Invested" = `spent` (the existing spend/activity aggregate). No new fields,
   no new queries — presentation/labeling only.
5. **The `<tfoot>` totals row is unchanged** ("Totals (expense)", labeled
   Category/Limit/Spent/Remaining) since it is already scoped to expense-only totals and is
   not called out in any AC bullet.
6. **Row `data-label` attributes for the 2nd/3rd cells become group-aware**: an Income row's
   cells carry `data-label="Target"` / `data-label="Received"`; a Savings row carries
   `data-label="Target"` / `data-label="Invested"`; an Expense row keeps
   `data-label="Limit"` / `data-label="Spent"` (byte-identical to today). This satisfies the
   "mobile stacked layout stays in sync" AC bullet directly. The name cell's own
   `data-label="Category"` stays unchanged in every row, grouped or not — an intentional
   carry-over, inert on mobile since the first cell's `::before` is already suppressed.
7. **On mobile (`<=480px`), the per-group header row collapses to just its first cell (the
   group name), hiding the 3 column-label cells**, matching today's mobile behavior for the
   divider row. Rationale: every data row already carries its own `data-label` prefix per
   cell on mobile, so repeating the column labels in the group header would be redundant.

## 4. Goal & Success Criteria

Fix the categories table so each rendered group's column headings match that group's
semantics, instead of one Expense-flavored heading applying uniformly.

- A multi-type budget's rendered table has no table-wide `<thead>`.
- Each present group (Income/Savings/Expense) is preceded by a 4-column `<th>` header row
  with the exact text specified in the AC.
- Every row's `data-label` attributes match its own group's column semantics.
- A single-type budget's rendering is byte-for-byte unchanged.
- All existing `categories_table_tests` pass (updated where necessarily changed) and new
  tests pin the exact new markup.

## 5. Scope

**In scope:**
- `backend/src/budget.rs`: `build_categories_table_html`, `push_category_row`,
  `CATEGORY_TABLE_GROUPS`, and `categories_table_tests`.
- `frontend/src/App.svelte`: all `.cat-table` CSS rules keyed on `tr.cat-group td` (desktop
  + the `@media (max-width: 480px)` block — 4 occurrences, see Architecture), updated to
  `th`-based selectors, plus the mobile "collapse to group name only" rule.

**Out of scope:**
- Single-type (ungrouped) budget rendering (Assumption 1).
- The `<tfoot>` totals row's labels/semantics.
- Any change to `CategoryTableRow`, `category_table_rows`, or any SQL query.
- `rag.rs`'s two call sites of `build_categories_table_html` — they only consume the
  returned HTML string; no call-site changes needed.

## 6. Architecture

`build_categories_table_html` currently:
1. Writes `<table class="cat-table"><thead>...</thead><tbody>` unconditionally.
2. Accumulates expense-only totals.
3. Branches on `distinct_types.len() > 1`: grouped path emits a `<tr class="cat-group"><td
   colspan="4">{Label}</td></tr>` divider per non-empty group, then rows via
   `push_category_row`; flat path just emits rows via `push_category_row`.
4. Appends `</tbody><tfoot>...</tfoot></table>`.

New shape:
1. Compute `distinct_types` (so `grouped = distinct_types.len() > 1`) before opening the
   table, so the opening tags can be chosen conditionally.
2. Write `<table class="cat-table">` then, only when `!grouped`, the existing shared
   `<thead>`; then `<tbody>`.
3. `CATEGORY_TABLE_GROUPS` becomes a 4-tuple `(type_key, group_heading, amount_label,
   activity_label)`:
   - `("income", "Income", "Target", "Received")`
   - `("savings", "Savings", "Target", "Invested")`
   - `("expense", "Expenses", "Limit", "Spent")`
   The `grouped_types_match_allowed_category_types` drift-guard test still compares only the
   `type_key` column against `ALLOWED_CATEGORY_TYPES`.
4. Grouped path: for each non-empty group, emit
   `<tr class="cat-group"><th>{group_heading}</th><th>{amount_label}</th><th>{activity_label}</th><th>Remaining</th></tr>`
   then `push_category_row(&mut out, r, amount_label, activity_label)` for each row.
   Flat path: `push_category_row(&mut out, r, "Limit", "Spent")` for each row (identical
   output to today).
5. `push_category_row` gains two new `&str` parameters (`amount_label`, `activity_label`)
   used in place of the hardcoded `"Limit"`/`"Spent"` literals in its `data-label`
   attributes; all other logic (fund badge, effective-limit math, deficit class) is
   unchanged.

CSS: rename every `.cat-table tr.cat-group td` selector to `th` — four occurrences in
`App.svelte`: the desktop weight/background/border rule, the desktop `:first-child`
border-top reset (a separate, narrower selector easy to miss), and the two mobile
overrides. Grep for `tr.cat-group` before editing to confirm all occurrences are caught.
Also add a mobile rule hiding the non-first `<th>` cells of a `.cat-group` row
(`nth-child(n+2) { display: none; }`) so the mobile view keeps showing just the bare group
name, matching pre-change appearance.

## 7. Error Handling & Edge Cases

- **Unrecognized `category_type`** (already-covered defensive path): unchanged — a row
  whose type isn't one of the three keys is still dropped from the grouped body with a
  `tracing::warn!`.
- **A present-but-empty group** (e.g. income+expense present, savings absent): unchanged —
  no header row of any kind for the absent group.
- **All rows the same type**: flat path, unchanged output including headings.
- **Zero rows**: `distinct_types` is empty, `grouped` is `false`, output is the existing
  well-formed empty table.

## 8. Testing Approach

Rust unit tests in `backend/src/budget.rs`'s `categories_table_tests` module (`cargo test -p
backend categories_table_tests`, no DB needed):

- Update `mixed_type_groups_render_in_income_savings_expense_order`,
  `absent_group_renders_no_empty_header`, and `grouped_types_match_allowed_category_types`
  to match the new 4-`<th>` header markup / 4-tuple shape.
- Add: no top-level `<thead>` when grouped.
- Add: exact header text per group, in Income→Savings→Expense order.
- Add: row-level `data-label` assertions for each group's amount/activity columns.
- Confirm `single_type_budget_stays_flat_with_no_group_header` still passes unchanged (it
  already pins the single-type scope boundary).
- Existing tests untouched by this change (money formatting, escaping, fund rendering,
  totals-row math, mirror-row resolution) keep passing verbatim.

No frontend/Playwright test changes planned — no existing automated coverage of the
`.cat-table` CSS; `pnpm run build` / a manual dev-server check is the verification path.

## 9. Risks & Open Questions

- **Scope boundary (Assumption 1)** — a reviewer could reasonably argue the single-type
  non-expense case should also be fixed now. Flagged explicitly with a pinning test rather
  than silently left inconsistent.
- **"Expenses" vs "Expense" naming (Assumption 3)** — verified via repo-wide grep that no
  other code or test depends on the exact string "Expense" from `CATEGORY_TABLE_GROUPS`
  outside `build_categories_table_html`'s own tests (which this change updates anyway).
