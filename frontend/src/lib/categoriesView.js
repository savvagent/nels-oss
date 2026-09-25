// Pure helpers for the Categories view (#426). No Svelte, no DOM — these are
// the only part of the page under unit test, matching transactionsView.js.

/** Share of the effective limit at which a category reads as "near limit". */
export const NEAR_LIMIT_THRESHOLD = 0.8;

/**
 * Display order of the category-type groups.
 *
 * EXPORTED for the drift guard in categoriesView.test.js. This is a
 * hand-maintained literal and `categories.category_type` has NO CHECK
 * constraint in the database, so a row whose type is outside this list is
 * dropped from the page entirely — not merely from a group, but from the
 * visible set, the search results and every total. The backend guards the same
 * hazard for its HTML table with `grouped_types_match_allowed_category_types`;
 * this is that guard's client-side twin, so a future fourth category type
 * fails a test here instead of silently making rows disappear.
 */
export const GROUP_ORDER = ["income", "savings", "expense"];

/**
 * THE SHARED ZERO-DENOMINATOR RULE, stated once for both `meterPercent` and
 * `healthStatus` so the two can never drift apart and paint contradicting
 * visuals, text and ARIA for the same row:
 *
 * - `null` or exactly `0` effective limit = NO MEANINGFUL DENOMINATOR. A limit
 *   of 0 is explicitly permitted (`validateLimit` accepts `n >= 0`) and is also
 *   reachable for a fund whose `category_limit + fund_balance` lands on zero,
 *   since #228 has no floor. Such a row is not "over" — there is simply nothing
 *   to measure against. `meterPercent` -> 0, `healthStatus` -> "none".
 * - A NEGATIVE effective limit is a genuinely overdrawn fund: any spend against
 *   it is fully over. `meterPercent` -> 100, `healthStatus` -> "over".
 *
 * Invariant the tests pin: whenever `healthStatus` says "over",
 * `meterPercent` is 100.
 */

/**
 * Spent share of the effective limit, clamped to 0-100 for rendering.
 * See the shared zero-denominator rule above.
 * @returns {number} 0-100
 */
export function meterPercent(spent, effectiveLimit) {
  if (effectiveLimit == null || effectiveLimit === 0) return 0;
  if (effectiveLimit < 0) return 100;
  return Math.min(100, Math.max(0, ((spent ?? 0) / effectiveLimit) * 100));
}

/**
 * Health of a category's spend against its effective limit. Callers pair this
 * with an icon AND a text label — never color alone.
 * See the shared zero-denominator rule above.
 * @returns {"none"|"healthy"|"near"|"over"}
 */
export function healthStatus(spent, effectiveLimit) {
  if (effectiveLimit == null || effectiveLimit === 0) return "none";
  if (effectiveLimit < 0) return "over";
  const share = (spent ?? 0) / effectiveLimit;
  if (share >= 1) return "over";
  if (share >= NEAR_LIMIT_THRESHOLD) return "near";
  return "healthy";
}

/**
 * Group rows into Income -> Savings -> Expense sections with per-group totals.
 * A limit-less row contributes its spend but not its (absent) limit. Empty
 * groups are omitted.
 *
 * DOCUMENTED BEHAVIOR: a row whose `category_type` is not in `GROUP_ORDER` is
 * excluded from every group and therefore never rendered. See GROUP_ORDER.
 * @returns {Array<{type: string, rows: Array, totalLimit: number, totalSpent: number}>}
 */
export function groupCategories(rows) {
  return GROUP_ORDER.map((type) => {
    const members = (rows || []).filter((r) => r.category_type === type);
    return {
      type,
      rows: members,
      totalLimit: members.reduce((sum, r) => sum + (r.effective_limit ?? 0), 0),
      totalSpent: members.reduce((sum, r) => sum + (r.spent ?? 0), 0),
    };
  }).filter((g) => g.rows.length > 0);
}

/** Case-insensitive name search. A blank query matches everything. */
export function searchCategories(rows, query) {
  const q = (query || "").trim().toLowerCase();
  if (!q) return rows || [];
  return (rows || []).filter((r) => (r.name || "").toLowerCase().includes(q));
}

/**
 * Which carry mechanism, if any, does this row actually get?
 *
 * A LINE-FOR-LINE MIRROR of `budget::category_carry_for` in
 * `backend/src/budget.rs`, which is the single source of the carry rule for the
 * REST surfaces. Chat does NOT call it — `rag.rs` re-derives the same rule from
 * `fund_effective_limit` / `category_carried`, a divergence AGENTS.md §19
 * documents. Editing that one function does not cover chat.
 *
 *   if is_expense && !is_linked && is_fund -> (0.0, fund_balance)   "fund"
 *   else if is_expense && !is_linked       -> (prev_spent, carry)   "rollover"
 *   else                                   -> (0.0, 0.0)            "none"
 *
 * THE SHAPE OF THE ARMS IS LOAD-BEARING, and is why this is one three-way
 * classifier rather than a pile of independent boolean exclusions. Precisely:
 * `!is_linked` is a conjunct of the FUND arm itself, not merely of the rollover
 * arm below it. So a #52 mirror that is also a #228 fund fails BOTH non-terminal
 * arms and lands on `"none"`, carrying NOTHING — not its `fund_balance`, not the
 * #49 carry. (`!is_linked` and `is_fund` commute within the arm; what matters is
 * which arm each appears in, and that the fund arm precedes the rollover arm —
 * the rollover arm's guard is a strict superset of the fund arm's, so swapping
 * them would make the fund arm unreachable.)
 * Dropping `!is_linked` from the fund arm is the exact mutation that would
 * resurrect the bug, and a test pins it. Stating this as two separate
 * exclusions instead of one ordered match is how the earlier prose here came to
 * claim something false. Pinned backend-side by
 * `category_carry_for_mirror_is_zero` and `category_carry_for_non_expense_is_zero`.
 *
 * "none" means the row's `carried_amount` is exactly 0.0 and nothing about a
 * carry may be rendered for it. Note "not the #49 carry" is NOT "carries
 * nothing": a "fund" row does carry, just its `fund_balance` under the Fund
 * badge instead of the #49 one-period carry.
 *
 * A row is treated as LINKED when EITHER `is_mirror` is true or
 * `linked_budget_id` is set. The backend derives the former from the latter
 * (`is_mirror: is_linked`) so they always agree in practice; reading both means
 * an envelope that carries only one of them still suppresses correctly, and the
 * failure direction of a disagreement is "suppress", never "advertise a carry
 * that does not happen".
 *
 * @param {object|null|undefined} row a `/categories-view` row. `null` and
 *   `undefined` are LEGAL input and classify as "none" — the page renders
 *   before the first response lands.
 * @returns {"fund"|"rollover"|"none"}
 */
export function categoryCarryKind(row) {
  const isExpense = row?.category_type === "expense";
  const isLinked = row?.is_mirror === true || row?.linked_budget_id != null;
  if (isExpense && !isLinked && row?.is_fund === true) return "fund";
  if (isExpense && !isLinked) return "rollover";
  return "none";
}

/**
 * Does this row earn a Rollover badge?
 *
 * Two gates, both of which must hold:
 *  1. `rolloverActive` — the BUDGET-level gate: the #47 master switch on AND
 *     not a #48 project budget, see PR #427. The caller derives it once; this
 *     helper takes it as an argument rather than re-deriving it from budget
 *     fields, so there is only one place the derivation lives.
 *  2. The row's own `rollover_enabled`, AND `categoryCarryKind(row) ===
 *     "rollover"` — i.e. the row is on the one arm of `category_carry_for` that
 *     actually applies the #49 one-period carry. That single term covers all
 *     three of the shapes that take NO #49 carry at once: a #228 NON-MIRROR
 *     fund (#433, whose carry is its `fund_balance` under the Fund badge
 *     instead — a fund that is ALSO a mirror carries nothing at all), a #52
 *     mirror, and any non-expense row (#474). `categories.rollover_enabled` is
 *     `DEFAULT TRUE`, so without the mirror/non-expense half every income and
 *     savings category on a rollover-on budget badged a carry that never
 *     happened.
 *
 * This now matches the chat surface ON WHICH ROWS MAY BE SHOWN A CARRY AT ALL:
 * `rag.rs` gates its whole rollover annotation on
 * `category_type == "expense" && linked_budget_id.is_none()` and short-circuits
 * a fund to its balance line. It is NOT full parity — chat renders an
 * annotation in places the page renders nothing, and for a fund+mirror the page
 * keeps a standalone Fund badge chat has no equivalent for. See the AGENTS.md
 * §19 paragraph, which states the divergence precisely.
 *
 * `CHIP_PREDICATES.rollover` carries the SAME `categoryCarryKind` term —
 * deliberately NOT the `rolloverActive` gate, which the strip handles by
 * dropping the chip entirely, so on an inert budget `chipCounts().rollover` MAY
 * be non-zero while zero badges render — and when it is zero the strip now
 * drops the chip on that ground too, see `visibleChips`. The two share the
 * `categoryCarryKind` term BY CONSTRUCTION, but the `rollover_enabled` term is
 * still duplicated by convention: change both. A test pins them over a shared
 * row matrix.
 *
 * @param {object|null|undefined} row a `/categories-view` row. A `null` or
 *   `undefined` row is LEGAL input and returns `false` — deliberate contract,
 *   pinned by a test, because the page renders before the first response lands.
 * @param {boolean} rolloverActive the budget-level gate, derived once by the
 *   caller. Anything other than exactly `true` suppresses the badge.
 * @returns {boolean} true when the Rollover badge should render for this row
 */
export function showsRolloverBadge(row, rolloverActive) {
  return (
    rolloverActive === true &&
    row?.rollover_enabled === true &&
    categoryCarryKind(row) === "rollover"
  );
}

/**
 * Does this row's `fund_balance` describe a real carry worth printing?
 *
 * Only on the "fund" arm. A row that is both `is_fund` and a #52 mirror keeps
 * its `fund_balance` column — the accrual path preserves it — but
 * `category_carry_for` never returns it for such a row, so `effective_limit` is
 * the bare `category_limit` there, and NULL when the row has no limit at all.
 * Rendering the `categories.fundBalance` line beside its Fund badge would
 * therefore advertise a number this row's own carry never uses. The Fund badge
 * itself still renders: `is_fund` is genuinely true, and only the BALANCE is
 * the false claim. (`Balance: $X` is chat's wording for the same idea in
 * `rag.rs`; the page's string is the `categories.fundBalance` key.)
 *
 * @param {object|null|undefined} row a `/categories-view` row; `null` and
 *   `undefined` are legal and return `false`.
 * @returns {boolean}
 */
export function showsFundBalance(row) {
  return categoryCarryKind(row) === "fund";
}

// The Over / Near chips are scoped to EXPENSE categories. Income and savings
// are target-based, not limit-based: an income category that meets or beats
// its "target received" is good news, and `healthStatus` — which reads any
// effective_limit as a spend ceiling — would otherwise mark it "over" and
// pollute the Over chip and its count. Same rule `expenseSummary` already uses.
const CHIP_PREDICATES = {
  all: () => true,
  over: (r) => r.category_type === "expense" && healthStatus(r.spent, r.effective_limit) === "over",
  near: (r) => r.category_type === "expense" && healthStatus(r.spent, r.effective_limit) === "near",
  nolimit: (r) => r.category_limit == null,
  // DELIBERATELY the raw `is_fund` flag, NOT `categoryCarryKind`. A fund that is
  // also a #52 mirror carries nothing, but it is still a fund and still badges
  // as one — #474 took away only its balance line. Routing this through the
  // classifier would silently drop it from the chip. A test pins that.
  funds: (r) => r.is_fund === true,
  // #433 + #474: the chip selects exactly the rows that earn the badge at
  // `rolloverActive === true` — the one arm of `category_carry_for` that applies
  // the #49 carry. See showsRolloverBadge.
  rollover: (r) => r.rollover_enabled === true && categoryCarryKind(r) === "rollover",
};

/** Filter rows by one chip key. An unknown key is treated as "all". */
export function filterByChip(rows, chip) {
  const pred = CHIP_PREDICATES[chip] || CHIP_PREDICATES.all;
  return (rows || []).filter(pred);
}

/** Live count per chip, for the chip labels. */
export function chipCounts(rows) {
  const list = rows || [];
  return Object.fromEntries(
    Object.entries(CHIP_PREDICATES).map(([key, pred]) => [key, list.filter(pred).length]),
  );
}

/**
 * Expense-only rollup for the page summary. Income and savings are excluded
 * because "remaining" is not meaningful for them — the same rule
 * build_categories_table_html applies to its totals row.
 */
export function expenseSummary(rows) {
  const expenses = (rows || []).filter((r) => r.category_type === "expense");
  const totalLimit = expenses.reduce((sum, r) => sum + (r.effective_limit ?? 0), 0);
  const totalSpent = expenses.reduce((sum, r) => sum + (r.spent ?? 0), 0);
  return { totalLimit, totalSpent, remaining: totalLimit - totalSpent };
}

// --- Error classification -----------------------------------------------
//
// These live here rather than in CategoriesView.svelte because they are pure
// string/status logic and this repo has NO Svelte component-test harness
// (vitest runs `environment: "node"`). They therefore return KEY strings, not
// translated copy: the component does `$_("categories." + key)`, and nothing
// here has to import svelte-i18n.

/**
 * Does this error mean "a category with that name already exists"?
 *
 * Matched on the MESSAGE rather than the status alone. As of #426 BOTH write
 * paths return the same 409 body — `create_category` and `update_category`
 * share `duplicate_category_name_error` — but the raw sqlx text
 * ("duplicate key", "unique constraint") is still matched so an older backend,
 * or any future path that forgets the mapping, still highlights the name field
 * instead of showing a generic failure the user can only retry forever.
 */
export function isDuplicateName(e) {
  const msg = String(e?.message ?? "").toLowerCase();
  return (
    msg.includes("already exists") ||
    msg.includes("duplicate key") ||
    msg.includes("unique constraint")
  );
}

/** Fallback keys `formMessageFor` uses when nothing more specific matches. */
export const SAVE_FAILED_KEY = "saveFailed";
export const DELETE_FAILED_KEY = "deleteFailed";

/**
 * Classify a failed category WRITE into a `categories.*` message key.
 *
 * THE ORDER IS LOAD-BEARING and is pinned by the unit tests:
 *   1. 403                      -> readOnly          (permission beats every message match)
 *   2. "linked rollup"          -> mirrorNotEditable (409, the most specific body)
 *   3. "only expense…funds"     -> fundExpenseOnly   (400, the server's own actionable wording)
 *   4. 409 + "closed"           -> closedBudget      (ensure_not_closed's exact status)
 *   5. 404                      -> notFound          (the row is gone; retrying always 404s)
 *   6. fallback
 *
 * 3 sits above 4 because only the message distinguishes them and the
 * fund/expense body is the narrower match. Rule 4 requires STATUS 409, never
 * the bare body: `ensure_not_closed` is the only producer of the "closed"
 * rejection and it ALWAYS 409s, while a transport error can legitimately carry
 * the word with no status at all ("the server closed the connection") — a
 * message-only match would mislabel an outage as a closed budget. 5 sits last
 * because it is a bare status with no distinguishing text, so any body-based
 * rule must win first.
 *
 * `fallbackKey` exists because the DELETE path reuses this: reporting
 * "Couldn't save that change" for a failed deletion describes an operation the
 * user never asked for.
 *
 * @returns {"readOnly"|"mirrorNotEditable"|"fundExpenseOnly"|"closedBudget"|"notFound"|"saveFailed"|"deleteFailed"}
 */
export function formMessageFor(e, fallbackKey = SAVE_FAILED_KEY) {
  const msg = String(e?.message ?? "").toLowerCase();
  if (e?.status === 403) return "readOnly";
  if (msg.includes("linked rollup")) return "mirrorNotEditable";
  if (msg.includes("only expense categories can be funds")) return "fundExpenseOnly";
  if (e?.status === 409 && msg.includes("closed")) return "closedBudget";
  if (e?.status === 404) return "notFound";
  return fallbackKey;
}

/**
 * Marker set on the error thrown when the /categories-view envelope does not
 * have the shape the whole page assumes. Kept as a flag rather than a message
 * match so `loadErrorKey` never has to sniff English prose it authored itself.
 */
export const MALFORMED_ENVELOPE = "malformedEnvelope";

/**
 * True when `res` is a usable /categories-view envelope.
 *
 * `parseApiResponse` returns `null` for a 204 or an empty body, and an
 * envelope missing `categories` coerces to `[]` at every read site — so
 * without this check "the budget genuinely has no categories", "the response
 * was empty" and "the response was structurally wrong" all render the same
 * empty state, which then invites the user to re-create categories that
 * already exist. It is also the single place a missing `spent` is caught:
 * defaulting `spent` deeper in would paint a green check and "On track" for a
 * category whose spend is unknown.
 */
export function isValidCategoriesEnvelope(res) {
  return !!res && Array.isArray(res.categories);
}

/**
 * Classify a failed category LOAD into a `categories.*` message key.
 *
 * PRECEDENCE IS DELIBERATELY INVERTED relative to the original code, which did
 * `e.message || $_("categories.loadError")`. `fetchApi` always constructs
 * `new Error(errText || "API error")`, so `e.message` is NEVER empty and the
 * localized fallback was unreachable: every user in every locale saw the raw
 * ENGLISH response body ("Internal server error", "Access denied") or the
 * browser's untranslated "Failed to fetch". `e.message` is now for
 * `console.error` only and never reaches the screen.
 *
 * @returns {"loadErrorMalformed"|"loadErrorDenied"|"loadErrorNotFound"|"loadErrorServer"|"loadErrorOffline"|"loadError"}
 */
export function loadErrorKey(e) {
  if (e?.[MALFORMED_ENVELOPE]) return "loadErrorMalformed";
  const status = e?.status;
  // No HTTP status at all = the request never produced a response: a network
  // failure, a DNS miss or fetchTimeout's abort. "Check your connection" is
  // the only actionable thing to say.
  if (typeof status !== "number") return "loadErrorOffline";
  if (status === 401 || status === 403) return "loadErrorDenied";
  if (status === 404) return "loadErrorNotFound";
  if (status >= 500) return "loadErrorServer";
  return "loadError";
}

// --- The PUT patch builder ------------------------------------------------

/**
 * Build the PUT body for an edit, sending ONLY what actually changed — every
 * absent field is left alone by the handler's COALESCE.
 *
 * Four non-obvious rules, each pinned by a test:
 *  1. `is_fund` is included only when the fund control is SHOWN. The checkbox
 *     is expense-only and absent in create mode, so an unshown control must
 *     never contribute a field the user could not see.
 *  2. The name is TRIMMED before comparison, so re-saving " Rent " against a
 *     stored "Rent" is a no-op rather than a pointless write.
 *  3. An EMPTIED limit box sends nothing. The server does
 *     `category_limit = COALESCE($3, category_limit)`, so a null reads as
 *     "unchanged" — `validateLimit` refuses the empty box up front when the
 *     row already has a limit, and this builder must not smuggle one through.
 *  4. An EMPTY patch means "no request at all" — the caller closes the sheet
 *     instead of round-tripping a body the server would ignore.
 *
 * @param {{name: string, limit: string, rollover: boolean, fund: boolean}} form
 * @param {object|null} editing last server image of the row being edited
 * @param {{showFund?: boolean}} [opts]
 * @returns {object} plain patch; `{}` means "don't send"
 */
export function buildCategoryPatch(form, editing, { showFund = false } = {}) {
  const patch = {};
  if (!editing) return patch;

  const name = String(form?.name ?? "").trim();
  if (name !== editing.name) patch.name = name;

  const raw = String(form?.limit ?? "").trim();
  const limit = raw === "" ? null : Number(raw);
  if (limit != null && limit !== (editing.category_limit ?? null)) {
    patch.category_limit = limit;
  }

  if (!!form?.rollover !== !!editing.rollover_enabled) {
    patch.rollover_enabled = !!form?.rollover;
  }

  if (showFund && !!form?.fund !== !!editing.is_fund) {
    patch.is_fund = !!form?.fund;
  }

  return patch;
}
