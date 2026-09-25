//! The read path: everything the server used to *enforce* or *store* on write
//! is *derived* here from merged facts, identically on every device.
//!
//! Every function is a pure function of document state (plus `now` where a
//! period is involved), so two devices holding the same changes always show
//! the user the same numbers, whatever order those changes arrived in.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::{AutoCommit, ChangeHash, ObjId, ReadDoc, ScalarValue, Value};
use chrono::{DateTime, NaiveDate, Utc};

use crate::device::Device;
use crate::model::{self, get_i64, get_str, map, Cents};
use crate::period;

/// One budget as a device sees it: its budget doc plus every transactions doc
/// for it that the device holds.
pub struct BudgetView<'a> {
    pub id: String,
    pub doc: &'a AutoCommit,
    pub txns: Vec<(&'a str, &'a AutoCommit)>,
}

impl<'a> BudgetView<'a> {
    pub fn load(dev: &'a Device, budget_id: &str) -> Self {
        let prefix = model::txns_prefix(budget_id);
        let txns = dev
            .docs_with_prefix(&prefix)
            .map(|(id, doc)| (id.as_str(), doc))
            .collect::<Vec<_>>();
        Self {
            id: budget_id.to_string(),
            doc: dev.doc(&model::budget_doc(budget_id)),
            txns,
        }
    }

    fn meta(&self, key: &str) -> Option<String> {
        get_str(self.doc, &map(self.doc, "meta"), key)
    }

    pub fn time_frame(&self) -> String {
        model::time_frame(self.doc)
    }

    pub fn is_closed(&self) -> bool {
        self.doc
            .get(map(self.doc, "meta"), "closed_at")
            .unwrap()
            .is_some()
    }

    pub fn name(&self) -> Option<String> {
        self.meta("name")
    }

    /// Every transaction (including tombstoned ones) across all held periods.
    pub fn transactions(&self) -> Vec<Txn> {
        self.collect(transactions)
    }

    /// [`Self::transactions`] without the costly delete/edit conflict flag
    /// (see [`ledger`]). What totals are computed from.
    pub fn ledger(&self) -> Vec<Txn> {
        self.collect(ledger)
    }

    /// One of the held transactions docs, by id.
    pub fn doc_of(&self, doc_id: &str) -> &'a AutoCommit {
        self.txns
            .iter()
            .find(|(id, _)| *id == doc_id)
            .map(|(_, d)| *d)
            .unwrap_or_else(|| panic!("view does not hold {doc_id}"))
    }

    fn collect(&self, rows: fn(&AutoCommit) -> Vec<Txn>) -> Vec<Txn> {
        let mut out: Vec<Txn> = self.txns.iter().flat_map(|(_, d)| rows(d)).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

// --- categories -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub limit: Cents,
    /// Other category ids with the same normalized name, folded into this one
    /// (scenario 5). Their transactions count here; their limits are listed so
    /// the UI can ask the user which to keep.
    pub folded: Vec<(String, Cents)>,
}

/// Case- and whitespace-insensitive. Stricter than today's
/// `unique_category_name_per_budget`, which is an exact-match UNIQUE.
pub fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Visible categories after read-time duplicate resolution: categories whose
/// names normalize equal are folded into the one with the lowest id. Lowest
/// id is arbitrary but deterministic, which is what matters: every device
/// folds the same way without coordinating.
pub fn categories(doc: &AutoCommit) -> Vec<Category> {
    let cats = map(doc, "categories");
    let mut groups: BTreeMap<String, Vec<(String, String, Cents)>> = BTreeMap::new();
    for id in doc.keys(&cats) {
        let (_, obj) = doc.get(&cats, id.as_str()).unwrap().unwrap();
        let name = get_str(doc, &obj, "name").unwrap_or_default();
        let limit = get_i64(doc, &obj, "category_limit").unwrap_or(0);
        groups
            .entry(normalize(&name))
            .or_default()
            .push((id, name, limit));
    }
    groups
        .into_values()
        .map(|mut g| {
            g.sort();
            let (id, name, limit) = g.remove(0);
            Category {
                id,
                name,
                limit,
                folded: g.into_iter().map(|(id, _, limit)| (id, limit)).collect(),
            }
        })
        .collect()
}

/// The visible category a stored `category_id` resolves to: itself, the
/// category it was folded into, or `None` (uncategorized) when it no longer
/// exists (scenario 6: deleted while another device assigned to it).
pub fn resolve_category(cats: &[Category], cat_id: &str) -> Option<String> {
    cats.iter()
        .find(|c| c.id == cat_id || c.folded.iter().any(|(f, _)| f == cat_id))
        .map(|c| c.id.clone())
}

// --- transactions -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Txn {
    pub id: String,
    pub date: NaiveDate,
    /// `amount_edit` if the user corrected it, else the imported/created amount.
    pub amount: Cents,
    pub category: Option<String>,
    pub deleted: bool,
    pub provider_id: Option<String>,
    /// Every concurrent value of `amount_edit` when two devices corrected the
    /// amount at once; `amount` is Automerge's winner (scenario 2).
    pub amount_edit_conflict: Vec<Cents>,
    /// Set when the transaction is tombstoned but some other device changed it
    /// concurrently with the delete (scenario 4).
    pub edited_concurrently_with_delete: bool,
}

/// Every row with its conflict flags, for showing transactions to the user.
pub fn transactions(doc: &AutoCommit) -> Vec<Txn> {
    let mut rows = ledger(doc);
    for t in rows.iter_mut().filter(|t| t.deleted) {
        t.edited_concurrently_with_delete = edited_concurrently_with_delete(doc, &t.id);
    }
    rows
}

/// Every row, with `edited_concurrently_with_delete` left `false`. That flag
/// forks the doc once per change for every tombstone (quadratic in history),
/// which is fine for a row the user opens but not for totals. Totals never
/// need it, since a deleted row is excluded either way.
pub fn ledger(doc: &AutoCommit) -> Vec<Txn> {
    // One pass per column (`map_range`) rather than a `get` per row per
    // column: on the 2b household this cut a full-history pass from 230 ms
    // to 19 ms.
    let (edit_obj, [date, amount, edit, cat, deleted, provider]) = (
        map(doc, "amount_edit"),
        [
            "date",
            "amount",
            "amount_edit",
            "category",
            "deleted",
            "provider_id",
        ]
        .map(|name| column(doc, name)),
    );
    let str_of = |c: &HashMap<String, (ScalarValue, bool)>, id: &str| {
        c.get(id).and_then(|(v, _)| v.as_str().map(str::to_string))
    };
    let i64_of = |c: &HashMap<String, (ScalarValue, bool)>, id: &str| {
        c.get(id).and_then(|(v, _)| v.as_i64())
    };
    let revision = column(doc, "revision");
    let mut rows: Vec<Txn> = date
        .keys()
        .map(|id| {
            let conflicted = edit.get(id).is_some_and(|(_, conflict)| *conflict);
            // A provider revision replaces the imported date and amount; with
            // concurrent revisions the highest feed sequence wins.
            let rev = match revision.get(id) {
                Some((_, true)) => model::newest_revision(doc, id),
                Some((v, false)) => v.as_str().and_then(model::Revision::decode),
                None => None,
            };
            let imported_date =
                NaiveDate::parse_from_str(&str_of(&date, id).unwrap(), "%Y-%m-%d").unwrap();
            Txn {
                date: rev.as_ref().map_or(imported_date, |r| r.date),
                // A user's correction beats the provider's figure.
                amount: i64_of(&edit, id)
                    .or(rev.as_ref().map(|r| r.amount))
                    .or_else(|| i64_of(&amount, id))
                    .unwrap_or(0),
                category: str_of(&cat, id),
                deleted: deleted.contains_key(id),
                provider_id: str_of(&provider, id),
                amount_edit_conflict: if conflicted {
                    all_i64(doc, &edit_obj, id)
                } else {
                    Vec::new()
                },
                edited_concurrently_with_delete: false,
                id: id.clone(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// Every entry of a flat column: `key -> (winning scalar, has a conflict)`.
fn column(doc: &AutoCommit, name: &str) -> HashMap<String, (ScalarValue, bool)> {
    doc.map_range(map(doc, name), ..)
        .filter_map(|item| match Value::from(item.value) {
            Value::Scalar(s) => Some((item.key.into_owned(), (s.into_owned(), item.conflict))),
            _ => None,
        })
        .collect()
}

fn all_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Vec<i64> {
    let mut v: Vec<i64> = doc
        .get_all(obj, key)
        .unwrap()
        .into_iter()
        .filter_map(|(v, _)| match v {
            Value::Scalar(s) => s.as_i64(),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

/// Whether any edit to `id` is causally concurrent with (not before) its
/// deletion, i.e. the device that edited it had not seen the delete. Uses the
/// change graph, not clocks: find the change that wrote the tombstone, then
/// look for edits to this transaction in changes that are not its ancestors.
fn edited_concurrently_with_delete(doc: &AutoCommit, id: &str) -> bool {
    let mut doc = doc.clone();
    let deleted = map(&doc, "deleted");
    let Some((_, del_op)) = doc.get(&deleted, id).unwrap() else {
        return false;
    };
    // The tombstone's op id names the change that wrote it: find that change's
    // heads-at-write by forking the doc to each change in turn. A spike-grade
    // brute force: fine for tens of changes, not for production.
    let changes = doc.get_changes(&[]);
    let Some(del_change) = changes.iter().find(|c| writes_op(c, &del_op)) else {
        return false;
    };
    let ancestors = ancestors_of(&changes, &del_change.hash());
    changes.iter().any(|c| {
        c.hash() != del_change.hash()
            && !ancestors.contains(&c.hash())
            && !is_descendant_of(&changes, &c.hash(), &del_change.hash())
            && touches_txn(&mut doc, c.hash(), id)
    })
}

fn writes_op(change: &automerge::Change, op: &automerge::ObjId) -> bool {
    match op {
        automerge::ObjId::Id(counter, actor, _) => {
            change.actor_id() == actor
                && *counter >= change.start_op().get()
                && *counter < change.start_op().get() + change.len() as u64
        }
        automerge::ObjId::Root => false,
    }
}

fn ancestors_of(changes: &[automerge::Change], hash: &ChangeHash) -> BTreeSet<ChangeHash> {
    let by_hash: BTreeMap<ChangeHash, &automerge::Change> =
        changes.iter().map(|c| (c.hash(), c)).collect();
    let mut out = BTreeSet::new();
    let mut stack: Vec<ChangeHash> = by_hash[hash].deps().to_vec();
    while let Some(h) = stack.pop() {
        if out.insert(h) {
            stack.extend(by_hash[&h].deps().iter().copied());
        }
    }
    out
}

fn is_descendant_of(changes: &[automerge::Change], hash: &ChangeHash, of: &ChangeHash) -> bool {
    ancestors_of(changes, hash).contains(of)
}

/// Whether the change `hash` wrote any column entry for transaction `id`:
/// compare the transaction's columns just before and just after the change.
fn touches_txn(doc: &mut AutoCommit, hash: ChangeHash, id: &str) -> bool {
    let change = doc.get_change_by_hash(&hash).unwrap();
    let before = doc.fork_at(change.deps()).unwrap();
    let after = doc.fork_at(&[hash]).unwrap();
    model::TXN_COLUMNS.iter().any(|col| {
        let b = before
            .get(map(&before, col), id)
            .unwrap()
            .map(|(v, _)| v.to_owned());
        let a = after
            .get(map(&after, col), id)
            .unwrap()
            .map(|(v, _)| v.to_owned());
        a != b
    })
}

/// Live (non-tombstoned) transactions whose resolved category is `cat` (or
/// uncategorized for `None`) with a date in `[start, end)`.
pub fn spent(
    view: &BudgetView,
    cats: &[Category],
    cat: Option<&str>,
    start: NaiveDate,
    end: NaiveDate,
) -> Cents {
    view.ledger()
        .iter()
        .filter(|t| !t.deleted && t.date >= start && t.date < end)
        .filter(|t| {
            t.category
                .as_deref()
                .and_then(|c| resolve_category(cats, c))
                .as_deref()
                == cat
        })
        .map(|t| t.amount)
        .sum()
}

pub fn total_spent(view: &BudgetView, start: NaiveDate, end: NaiveDate) -> Cents {
    view.ledger()
        .iter()
        .filter(|t| !t.deleted && t.date >= start && t.date < end)
        .map(|t| t.amount)
        .sum()
}

// --- funds (#228), derived -----------------------------------------------------

/// Latest `"<cat>/<period>"` entry at or before period `p`.
fn history_at(doc: &AutoCommit, map_name: &str, cat: &str, p: NaiveDate) -> Option<ScalarValue> {
    let m = map(doc, map_name);
    let lo = format!("{cat}/");
    let hi = format!("{cat}/{}", period::key(p));
    doc.map_range(&m, lo..=hi)
        .last()
        .and_then(|item| match Value::from(item.value) {
            Value::Scalar(s) => Some(s.into_owned()),
            _ => None,
        })
}

fn first_history_period(doc: &AutoCommit, map_name: &str, cat: &str) -> Option<NaiveDate> {
    let m = map(doc, map_name);
    let prefix = format!("{cat}/");
    let first = doc.map_range(&m, prefix.clone()..).next()?;
    first.key.strip_prefix(&prefix).map(period::parse_key)
}

pub fn limit_in(doc: &AutoCommit, cat: &str, p: NaiveDate) -> Cents {
    history_at(doc, "limit_history", cat, p)
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// A fund's balance carried into the current period:
/// `Σ over completed periods where the fund was on (limit_in(p) − spent_in(p))`.
///
/// This replaces `fund_balance` + `fund_advanced_through` + the
/// `advance_fund_categories` job. There is nothing to advance, so two devices
/// crossing a boundary offline cannot double-apply a period (scenario 10), and
/// a late transaction for a past period simply changes that period's spend
/// (scenario 11). Disabled periods don't count and re-enabling resumes, which
/// matches #228's freeze/resume semantics. `None` if the category was never a
/// fund.
///
/// Computed from a [`SpendSummary`] of every held transactions doc, built
/// here from scratch; [`SummaryCache`] is the same computation with the
/// summaries kept between reads.
pub fn fund_balance(
    view: &BudgetView,
    cats: &[Category],
    cat: &str,
    now: DateTime<Utc>,
) -> Option<Cents> {
    let tf = view.time_frame();
    let summaries: Vec<SpendSummary> = view
        .txns
        .iter()
        .map(|(_, d)| SpendSummary {
            heads: Vec::new(),
            time_frame: tf.clone(),
            spend: spend_by_period(d, &tf),
        })
        .collect();
    fund_balance_from(view.doc, summaries.iter(), cats, cat, now)
}

/// [`fund_balance`] over precomputed per-doc summaries. Needs the budget doc
/// (limits, fund toggles, categories) but no transactions doc.
pub fn fund_balance_from<'s>(
    budget: &AutoCommit,
    summaries: impl Iterator<Item = &'s SpendSummary>,
    cats: &[Category],
    cat: &str,
    now: DateTime<Utc>,
) -> Option<Cents> {
    let tf = model::time_frame(budget);
    let start = first_history_period(budget, "fund_history", cat)?;
    // Fold every summary into spend per period for this (resolved) category.
    let mut by_period: BTreeMap<NaiveDate, Cents> = BTreeMap::new();
    for s in summaries {
        assert_eq!(s.time_frame, tf, "summary built for another time frame");
        for ((p, raw), cents) in &s.spend {
            let resolved = raw.as_deref().and_then(|c| resolve_category(cats, c));
            if resolved.as_deref() == Some(cat) {
                *by_period.entry(*p).or_default() += cents;
            }
        }
    }
    let balance = period::completed_periods(&tf, start, now)
        .into_iter()
        .filter(|p| {
            history_at(budget, "fund_history", cat, *p).and_then(|v| v.as_bool()) == Some(true)
        })
        .map(|p| limit_in(budget, cat, p) - by_period.get(&p).copied().unwrap_or(0))
        .sum();
    Some(balance)
}

/// The transactions docs a reader of `doc_key`'s period must load: that doc,
/// plus any doc the `moved` index says holds a row provider-revised into it.
pub fn docs_for_period(budget: &AutoCommit, budget_id: &str, doc_key: &str) -> Vec<String> {
    let prefix = format!("{doc_key}/");
    let mut out = vec![format!("{}{doc_key}", model::txns_prefix(budget_id))];
    out.extend(
        budget
            .map_range(map(budget, "moved"), prefix.clone()..)
            .take_while(|item| item.key.starts_with(&prefix))
            .map(|item| {
                format!(
                    "{}{}",
                    model::txns_prefix(budget_id),
                    &item.key[prefix.len()..]
                )
            }),
    );
    out
}

// --- per-doc spend summaries (2b) ---------------------------------------------------

/// Live spend in one transactions doc, per `(period start, stored category
/// id)`. Category ids are kept RAW (unresolved) so a later rename, fold or
/// delete in the budget doc doesn't invalidate the summary; resolution happens
/// in [`fund_balance_from`]. Keyed by the transaction's own date, not by which
/// doc it lives in, so a doc may contribute to more than one period.
///
/// A pure function of the doc's heads and the time frame, which is what makes
/// it safe to cache on a device and never sync: equal heads, equal summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpendSummary {
    pub heads: Vec<ChangeHash>,
    pub time_frame: String,
    pub spend: BTreeMap<(NaiveDate, Option<String>), Cents>,
}

pub fn spend_by_period(
    doc: &AutoCommit,
    time_frame: &str,
) -> BTreeMap<(NaiveDate, Option<String>), Cents> {
    let mut spend = BTreeMap::new();
    for t in ledger(doc).into_iter().filter(|t| !t.deleted) {
        *spend
            .entry((period::period_of(time_frame, t.date), t.category))
            .or_default() += t.amount;
    }
    spend
}

/// A device-local, never-synced cache of [`SpendSummary`] per transactions
/// doc. An entry is reused only while the doc's heads and the budget's time
/// frame are unchanged, so it can go stale only by being out of date, never by
/// being wrong. A real client would store heads next to each doc on disk and
/// compare them without loading the doc; [`SummaryCache::refresh`] counts how
/// many docs it had to (re)summarize so tests can assert that.
#[derive(Debug, Default, Clone)]
pub struct SummaryCache {
    pub by_doc: BTreeMap<String, SpendSummary>,
}

impl SummaryCache {
    /// Bring the cache up to date with every transactions doc the view holds.
    /// `heads` is each doc's current heads, from
    /// [`Device::heads_with_prefix`] (Automerge needs `&mut` to read heads,
    /// and a view only borrows). Returns how many docs were summarized.
    pub fn refresh(
        &mut self,
        view: &BudgetView,
        heads: &BTreeMap<String, Vec<ChangeHash>>,
    ) -> usize {
        let tf = view.time_frame();
        let mut built = 0;
        for (doc_id, doc) in &view.txns {
            let heads = &heads[*doc_id];
            let fresh = self
                .by_doc
                .get(*doc_id)
                .is_some_and(|s| &s.heads == heads && s.time_frame == tf);
            if !fresh {
                let summary = SpendSummary {
                    heads: heads.clone(),
                    time_frame: tf.clone(),
                    spend: spend_by_period(doc, &tf),
                };
                self.by_doc.insert(doc_id.to_string(), summary);
                built += 1;
            }
        }
        built
    }

    pub fn fund_balance(
        &self,
        budget: &AutoCommit,
        cats: &[Category],
        cat: &str,
        now: DateTime<Utc>,
    ) -> Option<Cents> {
        fund_balance_from(budget, self.by_doc.values(), cats, cat, now)
    }
}

// --- auto-renew (#51), derived -------------------------------------------------

/// The current period's `[start, end)`: a pure function of `time_frame` and
/// the device's clock. Replaces the stored `next_renewal_at` marker.
pub fn current_period(view: &BudgetView, now: DateTime<Utc>) -> (NaiveDate, NaiveDate) {
    let tf = view.time_frame();
    let start = period::period_start(&tf, now);
    (start, period::next_period(&tf, start))
}

pub fn renewals(view: &BudgetView) -> Vec<String> {
    view.doc.keys(map(view.doc, "renewals")).collect()
}

// --- rollup (#52), resolved at read time --------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rollup {
    /// Accepted links, child -> parent.
    pub parent_of: BTreeMap<String, String>,
    /// Links present in the data but ignored, with the rule they broke. The UI
    /// shows these ("this link is inactive because …") so the user can fix it.
    pub ignored: Vec<(String, String, &'static str)>,
}

/// Rebuild a valid single-level rollup from whatever links merged. Links are
/// considered in child-id order and each is accepted only if it keeps the
/// structure valid under `validate_rollup_link`'s rules given the links
/// already accepted. Deterministic, so every device drops the same link. A
/// 2-cycle X->Y / Y->X keeps exactly one direction; longer chains are cut.
pub fn resolve_rollup(views: &[BudgetView]) -> Rollup {
    let meta = |v: &BudgetView, k: &str| v.meta(k).unwrap_or_default();
    let by_id: BTreeMap<&str, &BudgetView> = views.iter().map(|v| (v.id.as_str(), v)).collect();
    let mut out = Rollup::default();
    for (child, view) in &by_id {
        let Some(parent) = get_str(view.doc, &map(view.doc, "rollup"), "parent_id") else {
            continue;
        };
        let child = child.to_string();
        let reason = match by_id.get(parent.as_str()) {
            _ if parent == child => Some("a budget cannot roll up into itself"),
            None => Some("parent budget not present"),
            Some(p) if meta(p, "budget_type") != meta(view, "budget_type") => {
                Some("budget types differ")
            }
            Some(p) if meta(p, "strategy") != meta(view, "strategy") => Some("strategies differ"),
            _ if out.parent_of.contains_key(&parent) => {
                Some("parent is itself rolled up (single-level)")
            }
            _ if out.parent_of.values().any(|p| *p == child) => {
                Some("child already has children (single-level)")
            }
            _ => None,
        };
        match reason {
            Some(r) => out.ignored.push((child, parent, r)),
            None => {
                out.parent_of.insert(child, parent);
            }
        }
    }
    out
}

/// A budget's spend in a window plus, if it is a rollup parent, its children's.
/// Children are summed once each; with a resolved (acyclic, single-level)
/// rollup there is nothing to loop over twice.
pub fn rollup_spent(
    views: &[BudgetView],
    rollup: &Rollup,
    budget_id: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Cents {
    let own = |id: &str| {
        views
            .iter()
            .find(|v| v.id == id)
            .map(|v| total_spent(v, start, end))
            .unwrap_or(0)
    };
    own(budget_id)
        + rollup
            .parent_of
            .iter()
            .filter(|(_, p)| *p == budget_id)
            .map(|(c, _)| own(c))
            .sum::<Cents>()
}

// --- close (scenario 8) -----------------------------------------------------------

/// Transactions added or changed in a closed budget by a device that had not
/// seen the close. Causal, not clock-based: compare each transactions doc now
/// against the heads the closer recorded for it. A doc the closer never held
/// counts entirely as after-close. `Err` while the closer's heads have not
/// synced here yet (the answer isn't knowable until they do).
pub fn changed_after_close(view: &BudgetView) -> Result<Vec<String>, String> {
    if !view.is_closed() {
        return Ok(Vec::new());
    }
    let seen = map(view.doc, "close_seen");
    let mut out = Vec::new();
    for (doc_id, doc) in &view.txns {
        let now_txns = transactions(doc);
        let Some(heads) = get_str(view.doc, &seen, doc_id) else {
            out.extend(now_txns.into_iter().map(|t| t.id));
            continue;
        };
        // After compaction the closer's heads are gone; the compactor wrote
        // down the rows they classified as late (`erasure::encode_close_seen`).
        let (heads, late) = crate::erasure::decode_close_seen(&heads);
        out.extend(late);
        let mut probe = (*doc).clone();
        if !probe.get_missing_deps(&heads).is_empty() {
            return Err(format!("closer's heads for {doc_id} not synced yet"));
        }
        let at_close = transactions(&probe.fork_at(&heads).unwrap());
        out.extend(
            now_txns
                .into_iter()
                .filter(|t| !at_close.contains(t))
                .map(|t| t.id),
        );
    }
    out.sort();
    out.dedup();
    Ok(out)
}
