// Pure helpers for the Transactions view (#376).

// formatAmount moved to ./money.js in #426 so the transactions list and the
// categories view share one implementation. Re-exported here so this module's
// existing importers keep working unchanged.
export { formatAmount } from "./money.js";

/**
 * Only expense, non-mirror categories may receive a manual reassignment.
 * Income/savings categories are the wrong bucket for a spend, and mirror
 * categories (#52, `linked_budget_id` set) are live reflections of another
 * budget and must never receive a manual assignment.
 * @param {Array<{category_type?: string, linked_budget_id?: string|null}>} categories
 * @returns {Array} the assignable subset, order preserved.
 */
export function assignableCategories(categories) {
  return (categories || []).filter(
    (c) => c.category_type === "expense" && c.linked_budget_id == null,
  );
}

/**
 * Amount tone (#403) derived from the category TYPE, never the amount's sign —
 * amounts are stored as unsigned magnitudes, so direction lives in the category.
 * `income` → positive/success tone (rendered with a leading `+`); `expense` →
 * muted; anything else (savings, or an uncategorized import) → neutral.
 * @param {string|null|undefined} categoryType
 * @returns {"income"|"expense"|"neutral"}
 */
export function amountTone(categoryType) {
  if (categoryType === "income") return "income";
  if (categoryType === "expense") return "expense";
  return "neutral";
}

/**
 * Provenance kind (#403) for the row's origin badge, driven by the real `source`
 * field the API now returns. `ai` → "Logged by Nels", `imported` → a linked
 * account badge, `manual` (future add-form) or anything unknown → none.
 * @param {{source?: string}} tx
 * @returns {"ai"|"imported"|null}
 */
export function provenanceKind(tx) {
  const s = tx?.source;
  if (s === "ai") return "ai";
  if (s === "imported") return "imported";
  return null;
}

/**
 * A transaction is in the Needs-Review state (#403 P2) when its API
 * `review_status` is exactly `needs_review` — the state a freshly bank-synced
 * row enters until the user approves it. Drives the "Needs review" indicator,
 * the inline Approve action, and the pending de-emphasis. Everything else
 * (reviewed rows, Nels-logged rows, or an older payload with no field) is not
 * flagged.
 * @param {{review_status?: string}|null|undefined} tx
 * @returns {boolean}
 */
export function needsReview(tx) {
  return tx?.review_status === "needs_review";
}

/**
 * Client-side predicate for the "Needs Review" filter chip (#403 P2). When
 * `active`, keep only needs-review rows; when inactive, return the input array
 * unchanged (identity) so no copy churn happens on the common all-rows view.
 * @param {Array<{review_status?: string}>} transactions
 * @param {boolean} active whether the chip is toggled on
 * @returns {Array}
 */
export function filterNeedsReview(transactions, active) {
  if (!transactions) return [];
  if (!active) return transactions;
  return transactions.filter(needsReview);
}

/**
 * Immutably flip one row to `reviewed` (#403 P2) — the optimistic update applied
 * after a successful approve POST, so the row's indicator/de-emphasis clears
 * without a full reload. Returns a NEW array; the input and its other rows are
 * left untouched.
 * @param {Array<{id: string}>} transactions
 * @param {string} id the approved transaction's id
 * @returns {Array}
 */
export function markReviewed(transactions, id) {
  if (!transactions) return [];
  return transactions.map((t) =>
    t.id === id ? { ...t, review_status: "reviewed" } : t,
  );
}

/**
 * A transaction is a resolvable "possible duplicate" (#403 P3) when the sync-path
 * matcher linked it to a pre-existing Nels-logged twin (`matched_transaction_id`
 * set) AND it is still `needs_review`. Gating on `needs_review` (not just the
 * link) means the banner clears after EITHER resolution — merge and dismiss both
 * flip the imported row to `reviewed` — and, because that reviewed state persists
 * and a re-sync `DO NOTHING`s the already-imported row, it never resurfaces.
 * Only imported rows ever carry the link, so no extra source check is needed.
 * @param {{matched_transaction_id?: string|null, review_status?: string}|null|undefined} tx
 * @returns {boolean}
 */
export function possibleDuplicate(tx) {
  return (
    tx?.matched_transaction_id != null && tx?.review_status === "needs_review"
  );
}

/**
 * Immutable optimistic update after a successful `resolve-match` POST (#403 P3),
 * mirroring the server's non-destructive resolution so the UI updates without a
 * full reload. Both actions mark the imported row `reviewed` (clearing the
 * duplicate banner via `possibleDuplicate`). Additionally:
 *  - `merge`  → the linked Nels twin is excluded from the budget, so reflect
 *               `excluded_from_budget = true` on that row (its amount then reads
 *               as "not counted"); the link is kept, matching the server.
 *  - `dismiss` → clear the imported row's `matched_transaction_id` (keep both).
 * No row is ever removed. Returns a NEW array; inputs are untouched.
 * @param {Array<{id: string, matched_transaction_id?: string|null}>} transactions
 * @param {string} importedId the imported row the user resolved
 * @param {"merge"|"dismiss"} action
 * @returns {Array}
 */
export function resolveMatch(transactions, importedId, action) {
  if (!transactions) return [];
  const imported = transactions.find((t) => t.id === importedId);
  const matchedId = imported?.matched_transaction_id ?? null;
  return transactions.map((t) => {
    if (t.id === importedId) {
      return action === "dismiss"
        ? { ...t, review_status: "reviewed", matched_transaction_id: null }
        : { ...t, review_status: "reviewed" };
    }
    if (action === "merge" && matchedId != null && t.id === matchedId) {
      return { ...t, excluded_from_budget: true };
    }
    return t;
  });
}

// --- P4: relative-date grouping + running totals (#407) ---

/** Local midnight of the given date. @param {Date} d @returns {Date} */
function startOfDay(d) {
  const s = new Date(d);
  s.setHours(0, 0, 0, 0);
  return s;
}

/**
 * Bucket transactions under relative-date headers with a per-group running
 * total (#403 P4). Buckets by the row's local `transaction_date` into
 * **Today / Yesterday / This Week / `Month Year`**, emitted in that order
 * (months newest-first). "This Week" is the current Sunday-start calendar week,
 * excluding today and yesterday; anything older than the week start buckets by
 * calendar month.
 *
 * The header **running total** sums only **budget-affecting** rows — i.e. rows
 * with `excluded_from_budget` falsy — since an excluded row does not move the
 * budget; excluded rows still appear in the group's `rows` (rendered dimmed),
 * they just do not add to the total. The total is a gross magnitude sum, not a
 * signed net: amounts are unsigned magnitudes (direction lives in the category
 * type) and uncategorized imports have no known direction, so a net would be
 * ill-defined.
 *
 * Pure and deterministic: pass `now` to bucket relative to a fixed instant.
 * A row whose `transaction_date` cannot be parsed is skipped (never throws).
 *
 * @param {Array<{transaction_date?: string|Date, amount?: number, currency?: string|null, excluded_from_budget?: boolean}>} rows
 * @param {Date} [now]
 * @returns {Array<{ key: string, label: string, total: number, currency: string|null, rows: Array }>}
 *   `key` is `today` | `yesterday` | `thisWeek` | `YYYY-MM`; for month keys
 *   `label` is a localized `Month Year`, for relative keys `label === key` (the
 *   caller maps it to an i18n string).
 */
export function groupTransactions(rows, now = new Date()) {
  if (!rows || rows.length === 0) return [];
  const today = startOfDay(now);
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  const weekStart = new Date(today);
  weekStart.setDate(today.getDate() - today.getDay()); // most recent Sunday

  // Preserve insertion order of rows within a bucket; track month Date for label.
  const buckets = new Map(); // key -> { key, label, total, currency, rows, monthDate }
  for (const row of rows) {
    const parsed = new Date(row.transaction_date);
    if (Number.isNaN(parsed.getTime())) continue; // skip unparseable dates
    const day = startOfDay(parsed);

    let key;
    let label;
    let monthDate = null;
    if (day >= today) {
      key = "today"; // includes any (unexpected) future-dated row
      label = "today";
    } else if (day >= yesterday) {
      key = "yesterday";
      label = "yesterday";
    } else if (day >= weekStart) {
      key = "thisWeek";
      label = "thisWeek";
    } else {
      const y = day.getFullYear();
      const m = day.getMonth();
      key = `${y}-${String(m + 1).padStart(2, "0")}`;
      monthDate = new Date(y, m, 1);
      label = monthDate.toLocaleDateString(undefined, {
        month: "long",
        year: "numeric",
      });
    }

    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = { key, label, total: 0, currency: null, rows: [], monthDate };
      buckets.set(key, bucket);
    }
    bucket.rows.push(row);
    if (!row.excluded_from_budget) {
      bucket.total += row.amount ?? 0;
      // Prefer the first non-excluded row's currency as the group's currency.
      if (bucket.currency == null) bucket.currency = row.currency ?? null;
    }
  }

  // Any group with no budget-affecting row falls back to its first row's
  // currency (else null → formatAmount's USD default handles it downstream).
  for (const bucket of buckets.values()) {
    if (bucket.currency == null && bucket.rows.length > 0) {
      bucket.currency = bucket.rows[0].currency ?? null;
    }
  }

  const order = { today: 0, yesterday: 1, thisWeek: 2 };
  const groups = [...buckets.values()];
  groups.sort((a, b) => {
    const ra = a.key in order ? order[a.key] : 3;
    const rb = b.key in order ? order[b.key] : 3;
    if (ra !== rb) return ra - rb;
    // Both are month buckets: newest month first.
    return b.monthDate - a.monthDate;
  });
  // Drop the internal monthDate from the public shape.
  return groups.map(({ monthDate, ...g }) => g);
}

// --- P4: instant search + Pending/Uncategorized chips (#407) ---

/**
 * Instant-search predicate (#403 P4). A blank/whitespace query is identity —
 * returns the SAME array reference so the common unfiltered view does no copy.
 * Otherwise keeps rows whose `description`, `category_name`, or amount (matched
 * both as the raw number string and as a fixed-2-decimal string so "12.50"
 * finds `12.5`) contains the trimmed, lower-cased query. Case-insensitive.
 * Notes/tags are not in the data model, so they are out of scope for search.
 * @param {Array<{description?: string, category_name?: string|null, amount?: number}>} rows
 * @param {string} query
 * @returns {Array}
 */
export function searchTransactions(rows, query) {
  if (!rows) return [];
  const q = (query || "").trim().toLowerCase();
  if (!q) return rows;
  return rows.filter((r) => {
    const amount = r.amount;
    const haystacks = [
      r.description,
      r.category_name,
      amount == null ? "" : String(amount),
      amount == null ? "" : Number(amount).toFixed(2),
    ];
    return haystacks.some((h) => (h || "").toString().toLowerCase().includes(q));
  });
}

/**
 * "Pending" predicate for the Pending filter chip (#403 P4 / P5). A row is
 * pending when it still needs attention: either a freshly bank-synced row
 * awaiting approval (`needs_review`) OR a row with no category assigned yet
 * (`category_id` null). This is the app's notion of "not yet settled" until a
 * provider-level pending/hold flag is modeled (own follow-up).
 * @param {{review_status?: string, category_id?: string|null}|null|undefined} tx
 * @returns {boolean}
 */
export function isPending(tx) {
  if (!tx) return false;
  return needsReview(tx) || isUncategorized(tx);
}

/**
 * Client-side predicate for the Pending filter chip (#403 P4). Identity (same
 * ref) when inactive; mirrors `filterNeedsReview`.
 * @param {Array} transactions @param {boolean} active @returns {Array}
 */
export function filterPending(transactions, active) {
  if (!transactions) return [];
  if (!active) return transactions;
  return transactions.filter(isPending);
}

/**
 * "Uncategorized" predicate (#403 P4): a row with no category assigned
 * (`category_id` null/absent) — e.g. a bank import before the user categorizes
 * it, or a row whose auto-created category was cleaned up.
 * @param {{category_id?: string|null}|null|undefined} tx
 * @returns {boolean}
 */
export function isUncategorized(tx) {
  return tx?.category_id == null;
}

/**
 * Client-side predicate for the Uncategorized filter chip (#403 P4). Identity
 * (same ref) when inactive; mirrors `filterNeedsReview`.
 * @param {Array} transactions @param {boolean} active @returns {Array}
 */
export function filterUncategorized(transactions, active) {
  if (!transactions) return [];
  if (!active) return transactions;
  return transactions.filter(isUncategorized);
}

/**
 * Compose instant search with the three filter chips (#403 P4) in a single,
 * unit-testable pipeline: search first, then Needs Review, Pending, and
 * Uncategorized. Each stage is identity when inactive, so an all-inactive,
 * blank-query call returns the input array reference unchanged (no churn on the
 * common view). The caller then groups the result for render (filter → group →
 * render).
 * @param {Array} rows
 * @param {{query?: string, needsReviewOnly?: boolean, pendingOnly?: boolean, uncategorizedOnly?: boolean}} opts
 * @returns {Array}
 */
export function applyTransactionFilters(rows, opts = {}) {
  if (!rows) return [];
  let out = searchTransactions(rows, opts.query || "");
  out = filterNeedsReview(out, !!opts.needsReviewOnly);
  out = filterPending(out, !!opts.pendingOnly);
  out = filterUncategorized(out, !!opts.uncategorizedOnly);
  return out;
}

/**
 * Locate the row a duplicate link points at (#403 P5). Given a row `tx` with a
 * `matched_transaction_id`, return the row in `transactions` with that id, or
 * `null` (no link, no match, or nullish input). Used to render an imported row
 * beside its Nels-logged twin in the stitched-duplicate card.
 * @param {Array<{id: string}>} transactions
 * @param {{matched_transaction_id?: string|null}|null|undefined} tx
 * @returns {object|null}
 */
export function findTwin(transactions, tx) {
  const target = tx?.matched_transaction_id;
  if (!transactions || target == null) return null;
  return transactions.find((t) => t.id === target) ?? null;
}

/**
 * Live counts for the filter chips + alert strip (#403 P5), computed over the
 * FULL loaded set (not the already-filtered view) so each chip shows how many
 * rows it would match. `duplicates` counts resolvable possible-duplicate rows.
 * @param {Array} transactions
 * @returns {{all:number, needsReview:number, pending:number, uncategorized:number, duplicates:number}}
 */
export function filterCounts(transactions) {
  const rows = transactions || [];
  return {
    all: rows.length,
    needsReview: rows.filter(needsReview).length,
    pending: rows.filter(isPending).length,
    uncategorized: rows.filter(isUncategorized).length,
    duplicates: rows.filter(possibleDuplicate).length,
  };
}

/**
 * Split a backend `account_label` into its display name and `••last4` mask
 * (#425) so the badge can ellipsize the name while pinning the mask, which is
 * the token that actually distinguishes two cards at the same institution.
 * Mirrors `build_account_label` in backend/src/budget.rs, which emits
 * `"{name} ••{last4}"`. The name group is lazy and the mask is anchored at
 * end-of-string, so a name that itself contains `••` splits at the last one.
 * Returns nulls for absent input; a non-matching label passes through as a
 * bare name.
 * @param {string|null|undefined} label
 * @returns {{name: string|null, mask: string|null}}
 */
export function splitAccountLabel(label) {
  if (!label) return { name: null, mask: null };
  const m = /^(.*?)\s*••\s*(\w{2,4})$/.exec(label);
  return m ? { name: m[1] || null, mask: m[2] } : { name: label, mask: null };
}
