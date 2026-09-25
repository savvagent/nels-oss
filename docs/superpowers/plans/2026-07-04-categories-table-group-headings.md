# Categories Table Group Headings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the categories table's column headings reflect each rendered group's own
semantics (Income/Savings/Expense) instead of one Expense-flavored heading applying to the
whole table, per GitHub issue savvagent/nels#299. See
`docs/superpowers/specs/2026-07-04-categories-table-group-headings-design.md` for the full
design rationale, including why this plan supersedes an earlier, abandoned draft already
on the ticket.

**Architecture:** `build_categories_table_html` (`backend/src/budget.rs`) currently renders
one shared `<thead>` (`Category | Limit | Spent | Remaining`) plus `<tr class="cat-group">`
divider rows (#239) inside a single `<tbody>`, only when the budget mixes more than one
`category_type`. This plan keeps that same `>1 distinct type` gate, but for the grouped
case removes the shared `<thead>` and turns each group's divider row into a real 4-column
`<th>` header row using that group's own labels (Income: Target/Received; Savings:
Target/Invested; Expense: Limit/Spent — all four groups end in Remaining). A single-type
budget's flat rendering (including its `<thead>`) is untouched. `push_category_row` gains
two parameters so each row's `data-label`s (driving the mobile stacked layout) match
whichever heading applies to its group. Presentation-only — no data model or endpoint
changes.

**Tech Stack:** Rust (axum backend), Svelte 5 (frontend, server-rendered HTML injected via
`{@html}`), `cargo test`.

---

### Task 1: Rewrite the categories-table heading/grouping logic and its tests

**Files:**
- Modify: `backend/src/budget.rs` (doc comment + `CATEGORY_TABLE_GROUPS` const, ~line 4175-4206)
- Modify: `backend/src/budget.rs` (`build_categories_table_html`, ~line 4208-4286)
- Modify: `backend/src/budget.rs` (`push_category_row`, ~line 4288-4335)
- Modify: `backend/src/budget.rs` (`categories_table_tests` module — specific tests listed below)

- [ ] **Step 1: Update the affected tests in `categories_table_tests` first (TDD — these must fail against today's code)**

Leave every test NOT listed below untouched: `money_formats_whole_dollars_with_separators`,
`escapes_category_names`, `escapes_category_names_on_fund_row`, `null_limit_renders_dash`,
`build_categories_table_html_renders_resolved_mirror_limit_and_spent`,
`totals_row_sums_expense_only`, `null_limit_excluded_from_expense_totals`,
`overspend_renders_negative_remaining_with_sign_before_dollar`,
`fund_overspend_gets_text_error_class_non_fund_does_not`,
`large_value_renders_multi_comma_in_cell`,
`categories_table_renders_fund_balance_and_effective_limit`,
`categories_table_non_fund_row_unaffected`, `empty_rows_still_produce_well_formed_table`,
`unrecognized_category_type_is_silently_excluded_from_grouped_body`,
`cells_carry_data_label_paired_with_value` — none of these assert on heading markup,
`cat-group` shape, or `CATEGORY_TABLE_GROUPS`'s tuple arity, so none need to change (the
last one uses a single expense-type row, which stays on the flat/ungrouped path and still
gets `Limit`/`Spent` labels by default — verify this one still passes unmodified in Step 6,
since it's the closest existing test to the new behavior without actually exercising it).

Replace `mixed_type_groups_render_in_income_savings_expense_order` with:

```rust
    #[test]
    fn mixed_type_groups_render_in_income_savings_expense_order() {
        // Rows arrive GLOBALLY alphabetical-by-name (mirrors the real input:
        // category_table_rows sorts by name only, so types interleave — this
        // fixture interleaves savings/income/savings/expense/income/expense
        // on purpose). Names are chosen so alphabetical-by-name does NOT
        // already put rows in Income/Savings/Expense order overall, so the
        // header-order assertions below can only be explained by real
        // grouping, not incidental name order. Because the implementation
        // partitions this already-sorted slice by filtering (order-preserving,
        // no re-sort), each group's rows must appear in the SAME relative
        // order they have in this fixture — so the fixture itself must be
        // alphabetical for the intra-group order assertions to hold.
        let rows = vec![
            row("Anchor Fund", "savings", Some(500.0), 0.0),
            row("Bonus", "income", Some(1000.0), 0.0),
            row("Car Repair Fund", "savings", Some(200.0), 50.0),
            row("Groceries", "expense", Some(300.0), 100.0),
            row("Wages", "income", Some(4000.0), 4000.0),
            row("Zesty Snacks", "expense", Some(50.0), 10.0),
        ];
        let html = build_categories_table_html(&rows);

        // #299: a grouped (multi-type) table has NO top-level shared <thead>
        // at all — each group's own 4-column <th> header row (inside
        // <tbody>) replaces it entirely.
        assert!(!html.contains("<thead>"), "grouped table must have no table-wide thead: {html}");

        // Exactly one 4-column header per present type, in Income -> Savings
        // -> Expense order, using each group's own column semantics (#299).
        let income_hdr = html
            .find("<tr class=\"cat-group\"><th>Income</th><th>Target</th><th>Received</th><th>Remaining</th></tr>")
            .expect("Income header present");
        let savings_hdr = html
            .find("<tr class=\"cat-group\"><th>Savings</th><th>Target</th><th>Invested</th><th>Remaining</th></tr>")
            .expect("Savings header present");
        let expense_hdr = html
            .find("<tr class=\"cat-group\"><th>Expenses</th><th>Limit</th><th>Spent</th><th>Remaining</th></tr>")
            .expect("Expense header present");
        assert!(income_hdr < savings_hdr, "Income header must precede Savings header");
        assert!(savings_hdr < expense_hdr, "Savings header must precede Expense header");

        // Within each group, rows stay alphabetical by name (Bonus < Wages;
        // Anchor Fund < Car Repair Fund; Groceries < Zesty Snacks) and every
        // row in a group appears after that group's header and before the
        // next group's header.
        let bonus = html.find("data-label=\"Category\">Bonus</td>").expect("Bonus row present");
        let wages = html.find("data-label=\"Category\">Wages</td>").expect("Wages row present");
        assert!(income_hdr < bonus, "Bonus must come after the Income header");
        assert!(bonus < wages, "Bonus (alphabetically first) must precede Wages within Income group");
        assert!(wages < savings_hdr, "Wages (last Income row) must precede the Savings header");

        let anchor = html.find("data-label=\"Category\">Anchor Fund</td>").expect("Anchor Fund row present");
        let car_repair = html.find("data-label=\"Category\">Car Repair Fund</td>").expect("Car Repair Fund row present");
        assert!(savings_hdr < anchor, "Anchor Fund must come after the Savings header");
        assert!(anchor < car_repair, "Anchor Fund (alphabetically first) must precede Car Repair Fund within Savings group");
        assert!(car_repair < expense_hdr, "Car Repair Fund (last Savings row) must precede the Expense header");

        let groceries = html.find("data-label=\"Category\">Groceries</td>").expect("Groceries row present");
        let zesty = html.find("data-label=\"Category\">Zesty Snacks</td>").expect("Zesty Snacks row present");
        assert!(expense_hdr < groceries, "Groceries must come after the Expense header");
        assert!(groceries < zesty, "Groceries (alphabetically first) must precede Zesty Snacks within Expense group");

        // #299: each row's 2nd/3rd data-label matches its OWN group's
        // amount/activity columns, not a shared "Limit"/"Spent" — this is
        // the contract the mobile stacked layout depends on.
        assert!(html.contains("data-label=\"Target\">$1,000</td>"), "Bonus's limit cell is labeled Target: {html}");
        assert!(html.contains("data-label=\"Received\">$0</td>"), "Bonus's spent cell is labeled Received: {html}");
        assert!(html.contains("data-label=\"Target\">$500</td>"), "Anchor Fund's limit cell is labeled Target: {html}");
        assert!(html.contains("data-label=\"Invested\">$0</td>"), "Anchor Fund's spent cell is labeled Invested: {html}");
        assert!(html.contains("data-label=\"Limit\">$300</td>"), "Groceries's limit cell is still labeled Limit: {html}");
        assert!(html.contains("data-label=\"Spent\">$100</td>"), "Groceries's spent cell is still labeled Spent: {html}");

        // tfoot still sums expense-only (Groceries 300/100 + Zesty Snacks 50/10 = 350/110).
        let foot = tfoot(&html);
        assert!(foot.contains("$350"), "expense limit total in tfoot");
        assert!(foot.contains("$110"), "expense spent total in tfoot");
    }
```

Replace `absent_group_renders_no_empty_header` with:

```rust
    #[test]
    fn absent_group_renders_no_empty_header() {
        // Income + Expense present, Savings absent entirely -> two headers
        // (Income, Expense), never a "Savings" header/text anywhere.
        let rows = vec![
            row("Salary", "income", Some(4000.0), 4000.0),
            row("Rent", "expense", Some(1000.0), 900.0),
        ];
        let html = build_categories_table_html(&rows);
        assert!(html.contains(
            "<tr class=\"cat-group\"><th>Income</th><th>Target</th><th>Received</th><th>Remaining</th></tr>"
        ));
        assert!(html.contains(
            "<tr class=\"cat-group\"><th>Expenses</th><th>Limit</th><th>Spent</th><th>Remaining</th></tr>"
        ));
        assert!(
            !html.contains("Savings"),
            "no Savings header or any other Savings text when no savings categories are present"
        );
    }
```

Replace `grouped_types_match_allowed_category_types` with (unpacking the wider tuple):

```rust
    #[test]
    fn grouped_types_match_allowed_category_types() {
        // Drift guard (#239 follow-up): CATEGORY_TABLE_GROUPS is a separate
        // literal from ALLOWED_CATEGORY_TYPES, hand-maintained in a
        // different part of this file. If a future ticket adds a 4th
        // legitimate category type to ALLOWED_CATEGORY_TYPES without also
        // updating CATEGORY_TABLE_GROUPS, every category of that new type
        // would silently vanish from any mixed-type budget's grouped view
        // (dropped by the same code path unrecognized-type rows take today).
        // This test fails loudly the moment the two lists diverge, instead
        // of relying on that invariant only being documented in a comment.
        let mut group_keys: Vec<&str> =
            CATEGORY_TABLE_GROUPS.iter().map(|(key, _, _, _)| *key).collect();
        group_keys.sort_unstable();
        let mut allowed: Vec<&str> = ALLOWED_CATEGORY_TYPES.to_vec();
        allowed.sort_unstable();
        assert_eq!(
            group_keys, allowed,
            "CATEGORY_TABLE_GROUPS's keys must be exactly ALLOWED_CATEGORY_TYPES"
        );
    }
```

Modify `single_type_budget_stays_flat_with_no_group_header` — add two assertions (do not
rename; Assumption 1 keeps this behavior exactly as-is, this only tightens the pin):

```rust
    #[test]
    fn single_type_budget_stays_flat_with_no_group_header() {
        // Multiple rows, all the SAME category_type -> today's flat behavior:
        // no header row of any kind, rows just render in input order. #299
        // deliberately scopes its fix to grouped (multi-type) budgets only
        // (see the design doc's Assumption 1) — a single-type budget keeps
        // the generic Category/Limit/Spent/Remaining thead and data-labels
        // verbatim, even though the type here (income) doesn't semantically
        // match "Limit"/"Spent" either. That is an intentional, documented
        // scope boundary, not an oversight.
        let rows = vec![
            row("Bonus", "income", Some(1000.0), 0.0),
            row("Salary", "income", Some(4000.0), 4000.0),
        ];
        let html = build_categories_table_html(&rows);
        assert!(!html.contains("cat-group"), "single-type budget must not render any group header");
        assert!(!html.contains(">Income<"), "no standalone Income header text");
        // The generic shared thead is untouched (#299 scope boundary).
        assert!(html.contains(
            "<thead><tr><th>Category</th><th>Limit</th><th>Spent</th><th>Remaining</th></tr></thead>"
        ));
        assert!(html.contains("data-label=\"Limit\">$1,000</td>"), "flat rows keep the generic Limit label");
        // Rows are still present and in the given (already-alphabetical) order.
        let bonus = html.find("data-label=\"Category\">Bonus</td>").expect("Bonus row present");
        let salary = html.find("data-label=\"Category\">Salary</td>").expect("Salary row present");
        assert!(bonus < salary);
    }
```

- [ ] **Step 2: Run the test module to confirm it fails**

Run: `cd backend && cargo test categories_table_tests`
Expected: FAIL — compile errors first (`CATEGORY_TABLE_GROUPS` is still the old 2-tuple, so
the new `4-tuple` destructuring in the just-edited tests won't compile), then, once you
stub the constant to compile, assertion failures (the implementation still emits the old
shared thead / colspan divider markup).

- [ ] **Step 3: Replace the doc comment + `CATEGORY_TABLE_GROUPS` constant**

Replace the doc comment block and `const CATEGORY_TABLE_GROUPS` (currently ~budget.rs:4175-4206) with:

```rust
/// Build an escaped HTML table of categories with limit, spend, and remaining,
/// plus a totals row.
///
/// All dynamic text (category names) is HTML-escaped. A category with no
/// limit renders "—" for both its limit and remaining cells (spending still
/// shows). The totals row sums limit/spent/remaining over EXPENSE categories
/// only (`remaining = limit - spent`), mirroring the insights spend-vs-budget
/// rollup which filters to expense categories; income/savings categories are
/// still listed but excluded from the totals because "remaining" is not
/// meaningful for them.
///
/// Categories are grouped into Income -> Savings -> Expense sections (#239),
/// each preceded by a per-group 4-column `<tr class="cat-group">` header row
/// (#299: reflects each group's own column semantics, e.g. Income's
/// "Target"/"Received" rather than Expense's "Limit"/"Spent") — but ONLY when
/// `rows` contains more than one distinct `category_type`. A single-type
/// budget keeps the flat, generic-header list from before this change — an
/// intentional, documented scope boundary (see
/// docs/superpowers/specs/2026-07-04-categories-table-group-headings-design.md),
/// not an oversight. A group with no rows never emits its header. `rows` is
/// expected pre-sorted alphabetically by name (as returned by
/// `category_table_rows`'s `ORDER BY c.name ASC`); partitioning that
/// already-sorted slice preserves alphabetical order within each rendered
/// group without re-sorting. A row whose `category_type` is outside the
/// three recognized values is silently excluded from the grouped body — this
/// should be unreachable given `ALLOWED_CATEGORY_TYPES` write-time
/// validation, and mirrors this function's own totals loop, which already
/// only ever matched the exact string `"expense"`.
/// The three category-table groups in display order (Income -> Savings ->
/// Expense), paired with each group's own column headings: `(type_key,
/// group_heading, amount_label, activity_label)` — the 4th column is always
/// "Remaining" (#299). Keys MUST stay exactly `ALLOWED_CATEGORY_TYPES` (in
/// any order) — pinned by the `grouped_types_match_allowed_category_types`
/// test — so a future addition to `ALLOWED_CATEGORY_TYPES` can't silently
/// reintroduce the "unrecognized type is silently dropped" case in
/// `build_categories_table_html` without a test failure calling it out.
const CATEGORY_TABLE_GROUPS: [(&str, &str, &str, &str); 3] = [
    ("income", "Income", "Target", "Received"),
    ("savings", "Savings", "Target", "Invested"),
    ("expense", "Expenses", "Limit", "Spent"),
];
```

- [ ] **Step 4: Replace `build_categories_table_html`**

Replace the function body (currently ~budget.rs:4208-4286) with:

```rust
pub fn build_categories_table_html(rows: &[CategoryTableRow]) -> String {
    let distinct_types: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.category_type.as_str()).collect();
    let grouped = distinct_types.len() > 1;

    let mut out = String::from("<table class=\"cat-table\">");
    if !grouped {
        out.push_str(
            "<thead><tr>\
             <th>Category</th><th>Limit</th><th>Spent</th><th>Remaining</th>\
             </tr></thead>",
        );
    }
    out.push_str("<tbody>");

    // Totals accumulate over ALL rows regardless of grouping (expense only).
    // A fund row's contribution is its EFFECTIVE limit (limit + fund_balance)
    // — the true available headroom this period — not the bare category_limit
    // (#228). For a non-fund row these are numerically identical.
    let (mut tot_limit, mut tot_spent) = (0.0_f64, 0.0_f64);
    for r in rows {
        if r.category_type == "expense" {
            let base = r.category_limit.unwrap_or(0.0);
            tot_limit += fund_effective_limit(r.is_fund, base, r.fund_balance);
            tot_spent += r.spent;
        }
    }

    if grouped {
        for (type_key, group_label, amount_label, activity_label) in CATEGORY_TABLE_GROUPS {
            let group_rows: Vec<&CategoryTableRow> =
                rows.iter().filter(|r| r.category_type == type_key).collect();
            if group_rows.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "<tr class=\"cat-group\"><th>{}</th><th>{}</th><th>{}</th><th>Remaining</th></tr>",
                group_label, amount_label, activity_label
            ));
            for r in group_rows {
                push_category_row(&mut out, r, amount_label, activity_label);
            }
        }
        // A row whose category_type matched none of CATEGORY_TABLE_GROUPS
        // never rendered above. This should be unreachable
        // (ALLOWED_CATEGORY_TYPES is enforced at every write path — see
        // validate_category_type / resolve_category_type), but if the
        // invariant is ever violated (e.g. a direct-SQL migration/backfill,
        // or CATEGORY_TABLE_GROUPS drifting out of sync with
        // ALLOWED_CATEGORY_TYPES) a user's category would otherwise vanish
        // from their own table with zero signal. Warn loudly instead,
        // matching this file's existing convention for "should never
        // happen" cases (e.g. the budget-row-missing warning in
        // computed_budget_total).
        for r in rows {
            if !CATEGORY_TABLE_GROUPS
                .iter()
                .any(|(type_key, _, _, _)| r.category_type == *type_key)
            {
                tracing::warn!(
                    category_name = %r.name,
                    category_type = %r.category_type,
                    "build_categories_table_html: row has an unrecognized category_type and was dropped from the grouped table"
                );
            }
        }
    } else {
        for r in rows {
            push_category_row(&mut out, r, "Limit", "Spent");
        }
    }

    out.push_str(&format!(
        "</tbody><tfoot><tr>\
         <td data-label=\"Category\">Totals (expense)</td>\
         <td data-label=\"Limit\">{}</td>\
         <td data-label=\"Spent\">{}</td>\
         <td data-label=\"Remaining\">{}</td>\
         </tr></tfoot></table>",
        money(tot_limit),
        money(tot_spent),
        money(tot_limit - tot_spent)
    ));
    out
}
```

- [ ] **Step 5: Replace `push_category_row`**

Replace the function (currently ~budget.rs:4288-4335) with:

```rust
/// Render one category's `<tr>` for `build_categories_table_html`. Shared by
/// both the grouped and flat rendering paths so row markup is identical
/// either way. `amount_label`/`activity_label` set the 2nd/3rd cell's
/// `data-label` (#299 — e.g. "Target"/"Received" for an Income row,
/// "Limit"/"Spent" for an Expense row or the flat/ungrouped path) so the
/// responsive mobile layout's per-cell label always matches the heading the
/// cell falls under. The 1st cell's `data-label` stays "Category" (inert on
/// mobile regardless — the first cell's `::before` is unconditionally
/// suppressed by existing CSS) and the 4th stays "Remaining" (#181).
fn push_category_row(
    out: &mut String,
    r: &CategoryTableRow,
    amount_label: &str,
    activity_label: &str,
) {
    let effective_limit = r.category_limit.map(|l| fund_effective_limit(r.is_fund, l, r.fund_balance));
    let (limit_cell, remaining_cell) = match effective_limit {
        Some(eff) => (money(eff), money(eff - r.spent)),
        None => ("—".to_string(), "—".to_string()),
    };
    // Reuse EXISTING styled daisyUI/Tailwind classes (see the Styling note
    // above) — badge-warning for the fund indicator, text-error for a
    // negative (deficit) balance/remaining. Never invent a bespoke class name
    // here; it would render completely unstyled.
    let name_cell = if r.is_fund {
        let balance_class = if r.fund_balance < 0.0 { " text-error" } else { "" };
        format!(
            "{} <span class=\"badge badge-warning badge-sm\">Fund</span><br>\
             <small class=\"{}\">Balance: {}</small>",
            html_escape(&r.name),
            balance_class.trim_start(),
            money(r.fund_balance),
        )
    } else {
        html_escape(&r.name)
    };
    // Deficit styling is scoped to fund categories only: an ordinary (non-fund)
    // category could already go overspent before this feature, and that case
    // never carried a `class` attribute — widening it to every overspent row
    // would be an unreviewed scope expansion beyond "fund categories".
    let remaining_class = match effective_limit {
        Some(eff) if r.is_fund && eff - r.spent < 0.0 => " class=\"text-error\"",
        _ => "",
    };
    out.push_str(&format!(
        "<tr><td data-label=\"Category\">{}</td>\
         <td data-label=\"{}\">{}</td>\
         <td data-label=\"{}\">{}</td>\
         <td data-label=\"Remaining\"{}>{}</td></tr>",
        name_cell,
        amount_label, limit_cell,
        activity_label, money(r.spent),
        remaining_class,
        remaining_cell
    ));
}
```

- [ ] **Step 6: Run the test module again to confirm it passes**

Run: `cd backend && cargo test categories_table_tests`
Expected: PASS — all tests in `categories_table_tests` green (the `#[ignore]`d DB test
`category_table_rows_sums_current_period` is skipped by default; leave it untouched).

- [ ] **Step 7: Run the full backend test suite and clippy**

Run: `cd backend && cargo build && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no new warnings. (The `#[ignore]`d Postgres-backed tests are skipped unless
`podman-compose up -d` is running and `-- --ignored` is passed — not required for this
change, which touches no SQL.)

- [ ] **Step 8: Commit**

```bash
git add backend/src/budget.rs
git commit -m "fix(#299): categories table headings reflect Income/Savings/Expense group semantics"
```

---

### Task 2: Update `.cat-table` CSS so the mobile stacked layout matches the new per-group `<th>` markup

**Files:**
- Modify: `frontend/src/App.svelte` (`.cat-table tr.cat-group` selectors — desktop ~line 2527-2534, mobile ~line 2614-2635)

**Why this task exists:** the CSS currently styles the group divider row via `.cat-table
tr.cat-group td` selectors (4 occurrences — see below). Task 1 changes that row's cells
from `<td>` to `<th>`, so every one of those selectors would silently stop matching
(dead CSS, no test catches it) unless updated. This task also adds a mobile rule so the
now-4-column header row still collapses to just the bare group name on narrow screens,
preserving today's mobile appearance (the AC's "mobile stacked layout stays in sync" bullet
is satisfied primarily by Task 1's row-level `data-label` changes; this task's job is just
to not let the group *header* row itself become visually broken).

- [ ] **Step 1: Confirm the exact set of selectors to change**

Run: `grep -n "tr.cat-group" frontend/src/App.svelte`
Expected output (6 lines total — do not proceed if the count differs):
```
frontend/src/App.svelte:2527:  :global(.cat-table tr.cat-group td) {
frontend/src/App.svelte:2532:  :global(.cat-table tbody tr.cat-group:first-child td) {
frontend/src/App.svelte:2619:    :global(.cat-table tr.cat-group td) {
frontend/src/App.svelte:2624:         un-gated `.cat-table tr.cat-group td` rule above (700) losing to
frontend/src/App.svelte:2630:    :global(.cat-table tr.cat-group td::before) {
frontend/src/App.svelte:2633:    :global(.cat-table tbody tr.cat-group:not(:first-child)) {
```
Of these 6, only 4 are `td`-keyed selectors that need renaming to `th` (2527, 2532, 2619,
2630) — handled in Steps 2-3 below. The other 2 need NO change: line 2624 is a prose
comment mentioning the old selector text (not code — update its wording when you touch the
surrounding comment block in Step 3, but it's not a functional selector), and line 2633
(`tr.cat-group:not(:first-child) { margin-top: 0.5rem; }`) has no `td`/`th` in its selector
at all — it targets the `<tr>` itself, so it's already class-agnostic to the cell-tag
change and needs no edit. (Line numbers are approximate — match by content, not line
number, since Task 1 doesn't touch this file so they should be unchanged from today, but
confirm before editing.)

- [ ] **Step 2: Update the desktop rules (`td` -> `th`)**

Replace:

```css
  :global(.cat-table tr.cat-group td) {
    font-weight: 700;
    background: var(--color-base-200, oklch(0% 0 0 / 0.04));
    border-bottom: 1px solid var(--color-base-300, oklch(0% 0 0 / 0.15));
  }
  :global(.cat-table tbody tr.cat-group:first-child td) {
    border-top: none;
  }
```

with:

```css
  :global(.cat-table tr.cat-group th) {
    font-weight: 700;
    background: var(--color-base-200, oklch(0% 0 0 / 0.04));
    border-bottom: 1px solid var(--color-base-300, oklch(0% 0 0 / 0.15));
  }
  :global(.cat-table tbody tr.cat-group:first-child th) {
    border-top: none;
  }
```

- [ ] **Step 3: Update the mobile rules (`td` -> `th`), and hide the 3 non-first header cells**

Replace:

```css
    :global(.cat-table tr.cat-group td) {
      display: block;
      padding: 0.375rem 0.25rem;
      text-align: left;
      /* Without this, the header's weight falls back to the earlier,
         un-gated `.cat-table tr.cat-group td` rule above (700) losing to
         the mobile-only `tbody/tfoot td:first-child` rule (600) — same
         0,2,2 specificity, and :first-child comes later in the file, so it
         wins and the header silently renders at 600 instead of 700. */
      font-weight: 700;
    }
    :global(.cat-table tr.cat-group td::before) {
      content: none;
    }
```

with:

```css
    /* #299: the group header row is now 4 real <th> cells (Group / amount
       column / activity column / Remaining), not one colspan label — but
       the mobile stacked card should still show just the bare group name,
       matching pre-#299 appearance (every data row already carries its own
       data-label prefix, so repeating the column names here would be
       redundant). Only the first <th> renders; the other 3 are hidden. */
    :global(.cat-table tr.cat-group th:first-child) {
      display: block;
      padding: 0.375rem 0.25rem;
      text-align: left;
      /* Without this, the header's weight falls back to the earlier,
         un-gated `.cat-table tr.cat-group th` rule above (700) losing to
         the mobile-only `tbody/tfoot td:first-child` rule (600) — same
         0,2,2 specificity, and :first-child comes later in the file, so it
         wins and the header silently renders at 600 instead of 700. */
      font-weight: 700;
    }
    :global(.cat-table tr.cat-group th:nth-child(n + 2)) {
      display: none;
    }
```

- [ ] **Step 4: Run the frontend build**

Run: `cd frontend && pnpm run build`
Expected: PASS, no new errors.

- [ ] **Step 5: Manually verify in the browser**

Run: `cd frontend && pnpm run dev`, open the Categories page for a budget with Income +
Savings + Expense categories.
Expected, desktop width (>480px): three bold `<th>` header rows reading "Income / Target /
Received / Remaining", "Savings / Target / Invested / Remaining", "Expenses / Limit /
Spent / Remaining", each directly above its own group's rows.
Expected, mobile width (resize to <=480px, or Chrome DevTools device toolbar): each group
still shows a single bold group-name line ("Income" / "Savings" / "Expenses") above its
stacked cards — visually unchanged from before this ticket — and individual cards show the
correct label per row (e.g. an Income row shows "Target  $1,000" / "Received  $0", not
"Limit"/"Spent").

- [ ] **Step 6: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "style(#299): cat-table CSS follows the per-group th markup"
```

---

### Task 3: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full backend suite once more**

Run: `cd backend && cargo build && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 2: Grep for any stale reference to the old 3-column `cat-group` divider markup**

Run: `grep -n "colspan=\\\\\"4\\\\\"" backend/src/budget.rs`
Expected: no matches inside `build_categories_table_html`/`push_category_row`/their tests
(the old `<td colspan="4">` divider is gone, replaced by 4 individual `<th>`s).

- [ ] **Step 3: Confirm the design doc's success criteria**

Check off each bullet in
`docs/superpowers/specs/2026-07-04-categories-table-group-headings-design.md`'s "Goal &
Success Criteria" section against the actual test output from Steps 1-2 of this task and
Task 2's manual browser check.
