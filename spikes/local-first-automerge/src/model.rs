//! The write path: Nels's budget data modeled as Automerge documents, following
//! the plan's rule **store facts, derive numbers**.
//!
//! Layout rules, each forced by an observed Automerge behavior
//! (`tests/automerge_semantics.rs`):
//!
//! - **Every shared doc starts from a deterministic genesis** that creates all
//!   of its containers, so no two devices ever create "the same" container.
//! - **No object is ever created by more than one device.** Categories are
//!   nested objects because only their creator makes them (random id).
//!   Transactions are NOT: two devices importing the same bank row would both
//!   create its object. So a transactions doc is a set of flat *columns*
//!   (`date`, `amount`, … each a map `txn_id -> scalar`), and per-(category,
//!   period) facts use flat `"<cat>/<period>"` keys for the same reason.
//! - **Bank facts are immutable; user edits live in separate columns.** An
//!   import writes `amount`; a user correction writes `amount_edit`. Two
//!   devices importing the same row write identical values, so their conflict
//!   is harmless, and a user edit never races an import.
//! - **Deletes are tombstones** (`deleted` column), because a map-key delete
//!   silently drops a concurrent edit, and a per-column delete would let that
//!   edit resurrect a half-deleted transaction.
//! - **Money is integer cents.**
//!
//! Document ids: `budget/<budget_id>`, and `txns/<budget_id>/<YYYY-MM>` (the
//! default [`TxnLayout::Monthly`]) or `txns/<budget_id>/all`
//! ([`TxnLayout::Single`], kept for the 2b size comparison).

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, ROOT};
use chrono::{Datelike, NaiveDate};

use crate::device::Device;
use crate::period;

pub type Cents = i64;

pub fn budget_doc(budget_id: &str) -> String {
    format!("budget/{budget_id}")
}

pub fn txns_prefix(budget_id: &str) -> String {
    format!("txns/{budget_id}/")
}

/// Where a budget's transactions live. Chosen once by the budget's creator
/// and stored in `meta.txn_layout`; the read path is identical under both
/// (asserted in `tests/week2b.rs`). `Monthly` is the recommended topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnLayout {
    /// One transactions doc per calendar month (`txns/<id>/<YYYY-MM>`).
    Monthly,
    /// Every transaction in one doc (`txns/<id>/all`): the monolithic baseline.
    Single,
}

impl TxnLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            TxnLayout::Monthly => "monthly",
            TxnLayout::Single => "single",
        }
    }

    /// The suffix of the transactions doc a row dated `date` lives in.
    pub fn doc_key(self, date: NaiveDate) -> String {
        match self {
            TxnLayout::Monthly => format!("{:04}-{:02}", date.year(), date.month()),
            TxnLayout::Single => "all".to_string(),
        }
    }
}

/// The budget's layout as recorded in its doc. Absent means `Monthly`, which
/// is also what every budget created before 2b has.
pub fn txn_layout(doc: &AutoCommit) -> TxnLayout {
    match get_str(doc, &map(doc, "meta"), "txn_layout").as_deref() {
        Some("single") => TxnLayout::Single,
        _ => TxnLayout::Monthly,
    }
}

fn layout_on(dev: &Device, budget_id: &str) -> TxnLayout {
    let budget = budget_doc(budget_id);
    if dev.has_doc(&budget) {
        txn_layout(dev.doc(&budget))
    } else {
        TxnLayout::Monthly
    }
}

/// The monthly transactions doc for `date`.
pub fn txns_doc(budget_id: &str, date: NaiveDate) -> String {
    format!(
        "{}{}",
        txns_prefix(budget_id),
        TxnLayout::Monthly.doc_key(date)
    )
}

/// Containers every budget doc has from genesis.
pub const BUDGET_MAPS: [&str; 10] = [
    "meta",
    // "<txns doc suffix>" ("YYYY-MM", or "all" for the single layout) -> true
    // for every transactions doc that exists, so members discover docs
    // created on other devices and join them.
    "periods",
    "members",
    "categories",
    // "<cat_id>/<period_start>" -> limit cents set in that period (#228 §12 fix:
    // this IS the historical per-period limit the server schema lacks).
    "limit_history",
    // "<cat_id>/<period_start>" -> is_fund as toggled in that period.
    "fund_history",
    // "<txns doc id>" -> comma-joined heads of that doc the closer had seen.
    "close_seen",
    // "<period_start>" -> device that first observed the renewal (event log).
    "renewals",
    "rollup",
    // "<month key>/<doc key>" -> true: doc <doc key> holds a row whose
    // provider-revised date falls in <month key> (see `apply_bank_revision`).
    "moved",
];

/// Columns every transactions doc has from genesis. Each is `txn_id -> scalar`.
pub const TXN_COLUMNS: [&str; 9] = [
    "date",        // YYYY-MM-DD; also the "exists" column
    "amount",      // cents, as created or imported (immutable fact)
    "description", // as created or imported (immutable fact)
    "provider_id", // "<external_account_id>:<provider_transaction_id>" for bank rows
    "amount_edit", // user correction, overrides `amount`
    "category",    // user (or rule) assignment
    "deleted",     // tombstone: timestamp of deletion
    "note",
    "revision", // provider modification, ONE value: "<seq>|<date>|<amount>|<description>"
];

pub fn map(doc: &AutoCommit, name: &str) -> ObjId {
    match doc.get(ROOT, name).unwrap() {
        Some((_, id)) => id,
        None => panic!("document has no {name:?} container; was it opened with genesis?"),
    }
}

/// A fresh copy of `doc_id`'s genesis, as every device would create it.
pub fn genesis_for(doc_id: &str) -> AutoCommit {
    if doc_id.starts_with("budget/") {
        crate::device::genesis_doc(budget_genesis)
    } else if doc_id.starts_with("txns/") {
        crate::device::genesis_doc(txns_genesis)
    } else {
        panic!("unknown doc id {doc_id}");
    }
}

fn budget_genesis(d: &mut AutoCommit) {
    for name in BUDGET_MAPS {
        d.put_object(ROOT, name, ObjType::Map).unwrap();
    }
}

fn txns_genesis(d: &mut AutoCommit) {
    for name in TXN_COLUMNS {
        d.put_object(ROOT, name, ObjType::Map).unwrap();
    }
}

pub fn open_budget(dev: &mut Device, budget_id: &str) {
    dev.join_with_genesis(&budget_doc(budget_id), budget_genesis);
}

/// Open (creating if needed) the transactions doc `date` belongs in under the
/// budget's layout, and make sure the budget doc's `periods` index lists it.
/// Two devices that both create a month offline write the same index key with
/// the same value.
pub fn open_txns(dev: &mut Device, budget_id: &str, date: NaiveDate) -> String {
    let key = layout_on(dev, budget_id).doc_key(date);
    let id = format!("{}{key}", txns_prefix(budget_id));
    dev.join_with_genesis(&id, txns_genesis);
    let budget = budget_doc(budget_id);
    if dev.has_doc(&budget)
        && dev
            .doc(&budget)
            .get(map(dev.doc(&budget), "periods"), &key)
            .unwrap()
            .is_none()
    {
        dev.change(&budget, "index period", |d| {
            d.put(map(d, "periods"), key.as_str(), true).unwrap();
        });
    }
    id
}

/// Join every transactions doc listed in this device's copy of the budget's
/// `periods` index. Returns how many were newly joined.
pub fn join_listed_periods(dev: &mut Device, budget_id: &str) -> usize {
    let budget = budget_doc(budget_id);
    if !dev.has_doc(&budget) {
        return 0;
    }
    let keys: Vec<String> = dev
        .doc(&budget)
        .keys(map(dev.doc(&budget), "periods"))
        .collect();
    let mut joined = 0;
    for key in keys {
        let id = format!("{}{key}", txns_prefix(budget_id));
        if !dev.has_doc(&id) {
            dev.join_with_genesis(&id, txns_genesis);
            joined += 1;
        }
    }
    joined
}

/// Join a budget or transactions doc from its id alone, with the right
/// genesis. Used when a doc first arrives through the secure relay, which
/// hands over changes, not a `periods` index to walk.
pub fn open_by_id(dev: &mut Device, doc_id: &str) {
    if let Some(budget_id) = doc_id.strip_prefix("budget/") {
        open_budget(dev, budget_id);
    } else if doc_id.starts_with("txns/") {
        // Genesis only, unlike `open_txns`: indexing the period is the
        // author's job, and doing it here would make a receiving device author
        // a budget-doc change.
        dev.join_with_genesis(doc_id, txns_genesis);
    } else {
        panic!("unknown doc id {doc_id}");
    }
}

#[derive(Debug, Clone)]
pub struct BudgetMeta {
    pub name: &'static str,
    pub time_frame: &'static str,
    pub budget_type: &'static str,
    pub strategy: &'static str,
    pub auto_renew: bool,
    pub layout: TxnLayout,
}

impl BudgetMeta {
    pub fn monthly(name: &'static str) -> Self {
        Self {
            name,
            time_frame: "monthly",
            budget_type: "time_based",
            strategy: "limit_spent_remaining",
            auto_renew: false,
            layout: TxnLayout::Monthly,
        }
    }

    pub fn project(name: &'static str) -> Self {
        Self {
            budget_type: "project",
            ..Self::monthly(name)
        }
    }
}

pub fn create_budget(dev: &mut Device, budget_id: &str, meta: &BudgetMeta) {
    open_budget(dev, budget_id);
    dev.change(&budget_doc(budget_id), "create budget", |d| {
        let m = map(d, "meta");
        d.put(&m, "name", meta.name).unwrap();
        d.put(&m, "time_frame", meta.time_frame).unwrap();
        d.put(&m, "budget_type", meta.budget_type).unwrap();
        d.put(&m, "strategy", meta.strategy).unwrap();
        d.put(&m, "auto_renew", meta.auto_renew).unwrap();
        d.put(&m, "txn_layout", meta.layout.as_str()).unwrap();
    });
}

pub fn time_frame(doc: &AutoCommit) -> String {
    get_str(doc, &map(doc, "meta"), "time_frame").unwrap_or_else(|| "monthly".into())
}

fn current_period_key(dev: &Device, budget: &AutoCommit) -> String {
    period::key(period::period_start(&time_frame(budget), dev.clock.now()))
}

pub fn add_category(dev: &mut Device, budget_id: &str, cat_id: &str, name: &str, limit: Cents) {
    let doc_id = budget_doc(budget_id);
    let p = current_period_key(dev, dev.doc(&doc_id));
    dev.change(&doc_id, "add category", |d| {
        let cats = map(d, "categories");
        let c = d.put_object(&cats, cat_id, ObjType::Map).unwrap();
        d.put(&c, "name", name).unwrap();
        d.put(&c, "category_type", "expense").unwrap();
        d.put(&c, "category_limit", limit).unwrap();
        let lh = map(d, "limit_history");
        d.put(&lh, format!("{cat_id}/{p}"), limit).unwrap();
    });
}

/// A limit change applies to the CURRENT period only: it writes this period's
/// `limit_history` entry (and the current-limit convenience field). Earlier
/// periods keep the limit they had.
pub fn set_limit(dev: &mut Device, budget_id: &str, cat_id: &str, limit: Cents) {
    let doc_id = budget_doc(budget_id);
    let p = current_period_key(dev, dev.doc(&doc_id));
    dev.change(&doc_id, "set limit", |d| {
        let (_, c) = d
            .get(map(d, "categories"), cat_id)
            .unwrap()
            .expect("category exists");
        d.put(&c, "category_limit", limit).unwrap();
        d.put(map(d, "limit_history"), format!("{cat_id}/{p}"), limit)
            .unwrap();
    });
}

/// Enable/disable fund status (#228) as of the current period. No balance, no
/// marker: the balance is derived from `fund_history` + `limit_history` + spend.
pub fn set_fund(dev: &mut Device, budget_id: &str, cat_id: &str, is_fund: bool) {
    let doc_id = budget_doc(budget_id);
    let p = current_period_key(dev, dev.doc(&doc_id));
    dev.change(&doc_id, "set fund", |d| {
        d.put(map(d, "fund_history"), format!("{cat_id}/{p}"), is_fund)
            .unwrap();
    });
}

pub fn rename_category(dev: &mut Device, budget_id: &str, cat_id: &str, name: &str) {
    dev.change(&budget_doc(budget_id), "rename category", |d| {
        let (_, c) = d
            .get(map(d, "categories"), cat_id)
            .unwrap()
            .expect("category exists");
        d.put(&c, "name", name).unwrap();
    });
}

pub fn delete_category(dev: &mut Device, budget_id: &str, cat_id: &str) {
    dev.change(&budget_doc(budget_id), "delete category", |d| {
        d.delete(map(d, "categories"), cat_id).unwrap();
    });
}

pub fn set_rollup_parent(dev: &mut Device, child_budget: &str, parent_budget: &str) {
    dev.change(&budget_doc(child_budget), "rollup link", |d| {
        d.put(map(d, "rollup"), "parent_id", parent_budget).unwrap();
    });
}

pub fn add_member(dev: &mut Device, budget_id: &str, member: &str, role: &str) {
    dev.change(&budget_doc(budget_id), "add member", |d| {
        d.put(map(d, "members"), member, role).unwrap();
    });
}

/// Refuses locally when THIS device already knows the budget is closed: the
/// local equivalent of the server's `ensure_not_closed` 409. It cannot stop a
/// device that has not yet seen the close; the read path handles those.
pub fn ensure_not_closed(dev: &Device, budget_id: &str) -> Result<(), &'static str> {
    let doc = dev.doc(&budget_doc(budget_id));
    match doc.get(map(doc, "meta"), "closed_at").unwrap() {
        Some(_) => Err("budget is closed"),
        None => Ok(()),
    }
}

pub struct NewTxn<'a> {
    pub id: &'a str,
    pub date: NaiveDate,
    pub amount: Cents,
    pub description: &'a str,
    pub category: Option<&'a str>,
}

pub fn add_txn(dev: &mut Device, budget_id: &str, t: &NewTxn) -> Result<(), &'static str> {
    ensure_not_closed(dev, budget_id)?;
    let doc_id = open_txns(dev, budget_id, t.date);
    dev.change(&doc_id, "add transaction", |d| {
        d.put(map(d, "date"), t.id, t.date.to_string()).unwrap();
        d.put(map(d, "amount"), t.id, t.amount).unwrap();
        d.put(map(d, "description"), t.id, t.description).unwrap();
        if let Some(c) = t.category {
            d.put(map(d, "category"), t.id, c).unwrap();
        }
    });
    Ok(())
}

/// Deterministic transaction id for a bank row, so every device that imports
/// the same row writes to the same key (the CRDT stand-in for the
/// `(external_account_id, provider_transaction_id)` UNIQUE constraint).
pub fn bank_txn_id(external_account_id: &str, provider_transaction_id: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("{external_account_id}:{provider_transaction_id}").as_bytes(),
    )
    .to_string()
}

pub struct BankRow<'a> {
    pub external_account_id: &'a str,
    pub provider_transaction_id: &'a str,
    pub date: NaiveDate,
    pub amount: Cents,
    pub description: &'a str,
}

/// Import writes only immutable bank facts. It never writes `category` or
/// `amount_edit`, so an import on one device can never overwrite a user's edit
/// made on another.
///
/// A row this device holds a tombstone for is skipped: after a compaction
/// that tombstone is all that's left of it, and re-importing would write the
/// erased facts back (2c). A device that hasn't seen the delete yet still
/// imports; the next compaction strips those facts again.
pub fn import_bank_txn(dev: &mut Device, budget_id: &str, row: &BankRow) -> String {
    let id = bank_txn_id(row.external_account_id, row.provider_transaction_id);
    let doc_id = open_txns(dev, budget_id, row.date);
    if is_tombstoned(dev.doc(&doc_id), &id) {
        return id;
    }
    dev.change(&doc_id, "import bank transaction", |d| {
        d.put(map(d, "date"), &id, row.date.to_string()).unwrap();
        d.put(map(d, "amount"), &id, row.amount).unwrap();
        d.put(map(d, "description"), &id, row.description).unwrap();
        let provider = format!(
            "{}:{}",
            row.external_account_id, row.provider_transaction_id
        );
        d.put(map(d, "provider_id"), &id, provider).unwrap();
    });
    id
}

/// Import a batch of bank rows as ONE change per transactions doc, the way a
/// real bank sync lands a page of rows. Same facts-only and skip-tombstoned
/// rules as [`import_bank_txn`]. Returns the row ids in input order.
pub fn import_bank_batch(dev: &mut Device, budget_id: &str, rows: &[BankRow]) -> Vec<String> {
    let mut by_doc: std::collections::BTreeMap<String, Vec<(String, &BankRow)>> =
        Default::default();
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        let id = bank_txn_id(row.external_account_id, row.provider_transaction_id);
        let doc_id = open_txns(dev, budget_id, row.date);
        if !is_tombstoned(dev.doc(&doc_id), &id) {
            by_doc.entry(doc_id).or_default().push((id.clone(), row));
        }
        ids.push(id);
    }
    for (doc_id, rows) in by_doc {
        dev.change(&doc_id, "import bank transactions", |d| {
            let (date, amount, desc, provider) = (
                map(d, "date"),
                map(d, "amount"),
                map(d, "description"),
                map(d, "provider_id"),
            );
            for (id, row) in rows {
                d.put(&date, &id, row.date.to_string()).unwrap();
                d.put(&amount, &id, row.amount).unwrap();
                d.put(&desc, &id, row.description).unwrap();
                let p = format!(
                    "{}:{}",
                    row.external_account_id, row.provider_transaction_id
                );
                d.put(&provider, &id, p).unwrap();
            }
        });
    }
    ids
}

fn is_tombstoned(doc: &AutoCommit, id: &str) -> bool {
    doc.get(map(doc, "deleted"), id).unwrap().is_some()
}

/// Assign categories to several rows of one transactions doc in one change
/// (an auto-categorization rule run after an import).
pub fn set_categories(dev: &mut Device, doc_id: &str, assignments: &[(String, String)]) {
    dev.change(doc_id, "apply rules", |d| {
        let cat = map(d, "category");
        for (id, c) in assignments {
            d.put(&cat, id.as_str(), c.as_str()).unwrap();
        }
    });
}

pub fn set_note(dev: &mut Device, budget_id: &str, date: NaiveDate, id: &str, note: &str) {
    edit_txn(dev, budget_id, date, "set note", "note", id, note.into());
}

/// A provider modification of an already-imported bank row (Plaid
/// `modified`, which today overwrites amount/date/description in place).
///
/// `first_seen` is the date the row was first imported with, and `seq` is the
/// position of this record in the server's per-account feed. Both come from
/// the server, which still runs bank linking: it hands every device the same
/// ordered feed, so every device agrees on both.
pub struct BankRevision<'a> {
    pub external_account_id: &'a str,
    pub provider_transaction_id: &'a str,
    pub first_seen: NaiveDate,
    pub seq: u64,
    pub date: NaiveDate,
    pub amount: Cents,
    pub description: &'a str,
}

/// Parsed `revision` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub seq: u64,
    pub date: NaiveDate,
    pub amount: Cents,
    pub description: String,
}

impl Revision {
    pub fn encode(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.seq, self.date, self.amount, self.description
        )
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut parts = s.splitn(4, '|');
        Some(Self {
            seq: parts.next()?.parse().ok()?,
            date: NaiveDate::parse_from_str(parts.next()?, "%Y-%m-%d").ok()?,
            amount: parts.next()?.parse().ok()?,
            description: parts.next()?.to_string(),
        })
    }
}

/// The newest revision among every concurrent value of a row's `revision`
/// (highest `seq`, not Automerge's LWW winner).
pub fn newest_revision(doc: &AutoCommit, id: &str) -> Option<Revision> {
    doc.get_all(map(doc, "revision"), id)
        .unwrap()
        .into_iter()
        .filter_map(|(v, _)| match v {
            automerge::Value::Scalar(s) => s.as_str().and_then(Revision::decode),
            _ => None,
        })
        .max_by_key(|r| r.seq)
}

/// Apply a provider modification. The row stays in the doc of its FIRST-SEEN
/// date for good; only its `revision` changes, so no row ever exists in two
/// docs. The modified facts are one value, so a concurrent pair can't tear
/// (date from one, amount from the other), and the read picks the highest
/// `seq` among concurrent values.
///
/// Returns `false`, writing nothing, when this device already holds a
/// revision at least as new: a device replaying an older feed page must not
/// causally overwrite a newer one (a causal overwrite leaves only the stale
/// value, which no read-time rule could undo).
///
/// If the revised date falls in another month, the budget doc's `moved`
/// index records which doc holds it, so a reader of that month knows to load
/// it too (`read::docs_for_period`).
pub fn apply_bank_revision(dev: &mut Device, budget_id: &str, rev: &BankRevision) -> bool {
    let id = bank_txn_id(rev.external_account_id, rev.provider_transaction_id);
    let doc_id = open_txns(dev, budget_id, rev.first_seen);
    if newest_revision(dev.doc(&doc_id), &id).is_some_and(|r| r.seq >= rev.seq) {
        return false;
    }
    let value = Revision {
        seq: rev.seq,
        date: rev.date,
        amount: rev.amount,
        description: rev.description.to_string(),
    }
    .encode();
    dev.change(&doc_id, "bank revision", |d| {
        d.put(map(d, "revision"), &id, value).unwrap();
    });
    let layout = layout_on(dev, budget_id);
    let (target, source) = (layout.doc_key(rev.date), layout.doc_key(rev.first_seen));
    if target != source {
        let key = format!("{target}/{source}");
        let budget = budget_doc(budget_id);
        dev.change(&budget, "index moved row", |d| {
            d.put(map(d, "moved"), key.as_str(), true).unwrap();
        });
    }
    true
}

fn edit_txn(
    dev: &mut Device,
    budget_id: &str,
    date: NaiveDate,
    msg: &str,
    col: &str,
    id: &str,
    v: ScalarValue,
) {
    let doc_id = open_txns(dev, budget_id, date);
    dev.change(&doc_id, msg, |d| {
        d.put(map(d, col), id, v).unwrap();
    });
}

pub fn edit_amount(dev: &mut Device, budget_id: &str, date: NaiveDate, id: &str, amount: Cents) {
    edit_txn(
        dev,
        budget_id,
        date,
        "edit amount",
        "amount_edit",
        id,
        amount.into(),
    );
}

pub fn set_txn_category(
    dev: &mut Device,
    budget_id: &str,
    date: NaiveDate,
    id: &str,
    cat_id: &str,
) {
    edit_txn(
        dev,
        budget_id,
        date,
        "set category",
        "category",
        id,
        cat_id.into(),
    );
}

pub fn delete_txn(dev: &mut Device, budget_id: &str, date: NaiveDate, id: &str) {
    let now = dev.clock.now().timestamp();
    edit_txn(
        dev,
        budget_id,
        date,
        "delete transaction",
        "deleted",
        id,
        ScalarValue::Timestamp(now),
    );
}

/// Close a (project) budget. Alongside `closed_at` it records, per
/// transactions doc, the heads the closer had seen. That turns "was this
/// transaction added after the close?" into a causal question the read path
/// can answer exactly, with no reliance on device clocks.
pub fn close_budget(dev: &mut Device, budget_id: &str) {
    let now = dev.clock.now().timestamp();
    let prefix = txns_prefix(budget_id);
    let txn_doc_ids: Vec<String> = dev
        .docs_with_prefix(&prefix)
        .map(|(id, _)| id.clone())
        .collect();
    let seen: Vec<(String, String)> = txn_doc_ids
        .into_iter()
        .map(|id| {
            let heads = dev
                .heads(&id)
                .iter()
                .map(ChangeHash::to_string)
                .collect::<Vec<_>>()
                .join(",");
            (id, heads)
        })
        .collect();
    dev.change(&budget_doc(budget_id), "close budget", |d| {
        d.put(map(d, "meta"), "closed_at", ScalarValue::Timestamp(now))
            .unwrap();
        let cs = map(d, "close_seen");
        for (id, heads) in &seen {
            d.put(&cs, id.as_str(), heads.as_str()).unwrap();
        }
    });
}

/// Record that the budget renewed into the current period, keyed by the
/// period itself. Two devices that both observe the boundary write the SAME
/// key, so the log converges to one entry per period however many devices
/// saw it. A no-op when auto-renew is off, the budget is closed/archived, or
/// it is a project budget (the same exclusions as `renew_due_budgets`).
pub fn record_renewal(dev: &mut Device, budget_id: &str) -> bool {
    let doc_id = budget_doc(budget_id);
    let doc = dev.doc(&doc_id);
    let meta = map(doc, "meta");
    let eligible = get_bool(doc, &meta, "auto_renew") == Some(true)
        && get_str(doc, &meta, "budget_type").as_deref() == Some("time_based")
        && doc.get(&meta, "closed_at").unwrap().is_none()
        && doc.get(&meta, "archived_at").unwrap().is_none();
    if !eligible {
        return false;
    }
    let p = current_period_key(dev, doc);
    let device = dev.id.clone();
    dev.change(&doc_id, "renewal observed", |d| {
        d.put(map(d, "renewals"), p, device).unwrap();
    });
    true
}

// --- scalar reads -----------------------------------------------------------

pub fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).unwrap()? {
        (automerge::Value::Scalar(s), _) => s.as_str().map(str::to_string),
        _ => None,
    }
}

pub fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key).unwrap()? {
        (automerge::Value::Scalar(s), _) => s.as_i64(),
        _ => None,
    }
}

pub fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key).unwrap()? {
        (automerge::Value::Scalar(s), _) => s.as_bool(),
        _ => None,
    }
}
