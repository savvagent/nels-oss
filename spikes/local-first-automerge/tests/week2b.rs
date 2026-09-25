//! Week 2b: scale and topology.
//!
//! The always-run tests check the claims the measurement depends on, on a
//! small household: the generator does what it says, the two transaction
//! layouts derive identical numbers, the current period needs only the budget
//! doc and the current month's doc, and fund balances can come from a
//! device-local summary cache instead of loading every month.
//!
//! The full 5-year measurement is ignored by default:
//! `cargo test --release --test week2b -- --ignored --nocapture`.

// Money literals are cents grouped as dollars_cents: `500_00` is $500.00.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use chrono::{DateTime, NaiveDate, Utc};
use common::assert_converged;
use local_first_automerge_spike::model::{self, Cents, TxnLayout};
use local_first_automerge_spike::read::{self, BudgetView, SummaryCache};
use local_first_automerge_spike::workload::{self, category_id, Spec, Workload, BUDGET, FUNDS};
use local_first_automerge_spike::{Device, SimClock};

const LAPTOP: &str = "alice-laptop";

fn small(layout: TxnLayout) -> Spec {
    Spec {
        months: 5,
        txns_per_month: 60,
        layout,
        sync_every_days: 3,
        seed: 7,
    }
}

fn now(w: &Workload) -> DateTime<Utc> {
    w.h.device(LAPTOP).clock.now()
}

/// Spend per visible category in `[start, end)`, from whatever transactions
/// docs the view holds.
fn spend_by_category(
    view: &BudgetView,
    start: NaiveDate,
    end: NaiveDate,
) -> BTreeMap<String, Cents> {
    let cats = read::categories(view.doc);
    let mut out = BTreeMap::new();
    for t in view
        .ledger()
        .iter()
        .filter(|t| !t.deleted && t.date >= start && t.date < end)
    {
        let key = t
            .category
            .as_deref()
            .and_then(|c| read::resolve_category(&cats, c))
            .unwrap_or_else(|| "(uncategorized)".into());
        *out.entry(key).or_default() += t.amount;
    }
    out
}

fn fund_balances(view: &BudgetView, at: DateTime<Utc>) -> Vec<Option<Cents>> {
    let cats = read::categories(view.doc);
    FUNDS
        .iter()
        .map(|i| read::fund_balance(view, &cats, &category_id(*i), at))
        .collect()
}

/// A device that has loaded only these docs, from their saved bytes.
fn cold_device(from: &mut Device, docs: &[String]) -> Device {
    let mut dev = Device::new("cold", from.clock);
    for id in docs {
        let bytes = from.save(id);
        dev.load_saved(id, &bytes);
    }
    dev
}

fn current_month_doc(w: &Workload) -> String {
    model::txns_doc(BUDGET, now(w).date_naive())
}

#[test]
fn the_generator_produces_the_planned_household() {
    // Long enough for late (45+ day) edits to come due.
    let spec = Spec {
        months: 9,
        ..small(TxnLayout::Monthly)
    };
    let mut w = Workload::run(spec);
    let c = w.counts.clone();
    let expected = 9 * 60;
    assert!(
        (expected * 3 / 4..=expected * 5 / 4).contains(&c.txns()),
        "{} transactions, expected about {expected}: {c:?}",
        c.txns()
    );
    assert!(c.manual > 0 && c.bank_rows > c.manual, "{c:?}");
    for (what, n) in [
        ("rule_categorized", c.rule_categorized),
        ("user_categorized", c.user_categorized),
        ("recategorized", c.recategorized),
        ("late_edits", c.late_edits),
        ("amount_edits", c.amount_edits),
        ("deletes", c.deletes),
        ("notes", c.notes),
        ("limit_changes", c.limit_changes),
    ] {
        assert!(n > 0, "no {what}: {c:?}");
    }
    // Nine month docs (plus the budget doc), every row stored exactly once.
    let dev = w.h.device(LAPTOP);
    assert_eq!(dev.docs_with_prefix(&model::txns_prefix(BUDGET)).count(), 9);
    assert_eq!(
        BudgetView::load(dev, BUDGET).ledger().len() as u64,
        c.txns()
    );
    assert_converged(&mut w.h);
}

#[test]
fn both_layouts_derive_identical_numbers() {
    let mut monthly = Workload::run(small(TxnLayout::Monthly));
    let mut single = Workload::run(small(TxnLayout::Single));
    assert_eq!(monthly.counts, single.counts, "same operations");
    assert_converged(&mut monthly.h);
    assert_converged(&mut single.h);

    let m = BudgetView::load(monthly.h.device(LAPTOP), BUDGET);
    let s = BudgetView::load(single.h.device(LAPTOP), BUDGET);
    assert_eq!(m.txns.len(), 5);
    assert_eq!(s.txns.len(), 1);
    assert_eq!(s.txns[0].0, format!("{}all", model::txns_prefix(BUDGET)));

    assert_eq!(m.ledger(), s.ledger());
    assert_eq!(read::categories(m.doc), read::categories(s.doc));
    let at = now(&monthly);
    assert_eq!(fund_balances(&m, at), fund_balances(&s, at));
    let mut p = NaiveDate::from_ymd_opt(2021, 1, 1).unwrap();
    while p < at.date_naive() {
        let next = workload::add_months(p, 1);
        assert_eq!(
            spend_by_category(&m, p, next),
            spend_by_category(&s, p, next)
        );
        p = next;
    }
}

#[test]
fn the_current_period_needs_only_the_budget_doc_and_the_current_month() {
    let mut w = Workload::run(small(TxnLayout::Monthly));
    w.run_days(12); // into the middle of month 6
    w.sync();
    let at = now(&w);
    let (start, end) = read::current_period(&BudgetView::load(w.h.device(LAPTOP), BUDGET), at);

    let docs = [model::budget_doc(BUDGET), current_month_doc(&w)];
    let cold = cold_device(w.h.device_mut(LAPTOP), &docs);
    let partial = BudgetView::load(&cold, BUDGET);
    assert_eq!(partial.txns.len(), 1, "only the current month is loaded");

    let full = BudgetView::load(w.h.device(LAPTOP), BUDGET);
    assert_eq!(full.txns.len(), 6);
    let expected = spend_by_category(&full, start, end);
    assert!(!expected.is_empty());
    assert_eq!(spend_by_category(&partial, start, end), expected);

    // Why that holds: a row lives in the doc of its own date's month (a row
    // imported on the 1st but dated the 30th went to last month's doc).
    for (doc_id, doc) in &full.txns {
        for t in read::ledger(doc) {
            assert_eq!(
                model::txns_doc(BUDGET, t.date),
                *doc_id,
                "row {} is stored outside its month",
                t.id
            );
        }
    }
}

#[test]
fn fund_balances_come_from_cached_summaries_without_loading_history() {
    let mut w = Workload::run(small(TxnLayout::Monthly));
    let at = now(&w);
    let cats = read::categories(w.h.device(LAPTOP).doc(&model::budget_doc(BUDGET)));

    let mut cache = SummaryCache::default();
    let heads =
        w.h.device_mut(LAPTOP)
            .heads_with_prefix(&model::txns_prefix(BUDGET));
    let view = BudgetView::load(w.h.device(LAPTOP), BUDGET);
    assert_eq!(
        cache.refresh(&view, &heads),
        5,
        "first run summarizes every month"
    );
    assert_eq!(cache.refresh(&view, &heads), 0, "nothing changed");
    let from_scratch = fund_balances(&view, at);
    assert!(from_scratch.iter().all(Option::is_some));

    // A device holding ONLY the budget doc (plus the cache) gets the same
    // balances: no month doc needs to be loaded.
    let cold = cold_device(w.h.device_mut(LAPTOP), &[model::budget_doc(BUDGET)]);
    let budget = cold.doc(&model::budget_doc(BUDGET));
    let cached: Vec<_> = FUNDS
        .iter()
        .map(|i| cache.fund_balance(budget, &cats, &category_id(*i), at))
        .collect();
    assert_eq!(cached, from_scratch);

    // A late recategorization into a fund lands in an OLD month doc. Only that
    // doc is re-summarized, and the cached balance follows the change.
    let fund = category_id(FUNDS[0]);
    let (old_id, old_date) = {
        let view = BudgetView::load(w.h.device(LAPTOP), BUDGET);
        let t = view
            .ledger()
            .into_iter()
            .find(|t| {
                !t.deleted
                    && t.date.format("%Y-%m").to_string() == "2021-02"
                    && t.category.as_deref() != Some(fund.as_str())
            })
            .expect("a February row outside the fund");
        (t.id, t.date)
    };
    model::set_txn_category(w.h.device_mut(LAPTOP), BUDGET, old_date, &old_id, &fund);
    let heads =
        w.h.device_mut(LAPTOP)
            .heads_with_prefix(&model::txns_prefix(BUDGET));
    let view = BudgetView::load(w.h.device(LAPTOP), BUDGET);
    assert_eq!(
        cache.refresh(&view, &heads),
        1,
        "only February's doc changed"
    );
    let after = fund_balances(&view, at);
    assert_ne!(
        after, from_scratch,
        "the late edit moved spend into the fund"
    );
    let budget = w.h.device(LAPTOP).doc(&model::budget_doc(BUDGET));
    assert_eq!(cache.fund_balance(budget, &cats, &fund, at), after[0]);

    // Summaries keep RAW category ids, so renaming a category (a budget-doc
    // change) invalidates nothing.
    model::rename_category(w.h.device_mut(LAPTOP), BUDGET, &fund, "Car Fund");
    let view = BudgetView::load(w.h.device(LAPTOP), BUDGET);
    assert_eq!(cache.refresh(&view, &heads), 0);
}

#[test]
fn a_week_offline_merges_and_converges() {
    let mut w = Workload::run(small(TxnLayout::Monthly));
    let before = w.counts.clone();
    w.go_offline("bob-phone");
    w.run_days(7);
    w.sync(); // the other two devices keep syncing
    let during = w.counts.clone();
    assert!(
        during.txns() > before.txns()
            && during.recategorized + during.user_categorized
                > before.recategorized + before.user_categorized,
        "everyone kept working"
    );
    w.go_online("bob-phone");
    w.sync();
    assert_converged(&mut w.h);
    assert_eq!(
        BudgetView::load(w.h.device("bob-phone"), BUDGET)
            .ledger()
            .len() as u64,
        w.counts.txns()
    );
}

// --- measurement ----------------------------------------------------------------

fn kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

type Row = (&'static str, String);

fn changes_in(doc: &mut AutoCommit) -> usize {
    doc.get_changes(&[]).len()
}

/// Bytes of the changes `to` lacks, over every doc `from` holds: what the 2a
/// secure log would carry (before envelope overhead) to catch `to` up.
/// A set difference by change hash: `get_changes(to's heads)` would return
/// EVERYTHING when `to` has heads `from` has never seen.
fn missing_change_bytes(w: &mut Workload, from: &str, to: &str) -> usize {
    let docs: Vec<String> =
        w.h.device(from)
            .docs_with_prefix("")
            .map(|(d, _)| d.clone())
            .collect();
    let mut total = 0;
    for d in docs {
        let theirs: Vec<automerge::ChangeHash> = if w.h.device(to).has_doc(&d) {
            let doc = w.h.device_mut(to).doc_mut(&d);
            doc.get_changes(&[]).iter().map(|c| c.hash()).collect()
        } else {
            Vec::new()
        };
        let theirs: std::collections::HashSet<_> = theirs.into_iter().collect();
        total +=
            w.h.device_mut(from)
                .doc_mut(&d)
                .get_changes(&[])
                .iter()
                .filter(|c| !theirs.contains(&c.hash()))
                .map(|c| c.raw_bytes().len())
                .sum::<usize>();
    }
    total
}

const PROBES: u32 = 50;

/// Average time to add one manual transaction (write + commit) to the
/// current month, on a device holding 5 years of history.
fn write_cost(w: &mut Workload) -> Duration {
    let dev = w.h.device_mut(LAPTOP);
    let today = dev.clock.now().date_naive();
    let n = PROBES;
    let started = Instant::now();
    for i in 0..n {
        model::add_txn(
            dev,
            BUDGET,
            &model::NewTxn {
                id: &format!("probe-{i}"),
                date: today,
                amount: 1_00,
                description: "probe",
                category: None,
            },
        )
        .unwrap();
    }
    started.elapsed() / n
}

fn measure(layout: TxnLayout) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut add = |what: &'static str, v: String| rows.push((what, v));

    let started = Instant::now();
    let mut w = Workload::run(Spec::five_years(layout));
    add(
        "generate 5 years (3 devices, sync every 3 days)",
        ms(started.elapsed()),
    );
    let c = w.counts.clone();
    add(
        "transactions (bank + manual)",
        format!("{} ({} + {})", c.txns(), c.bank_rows, c.manual),
    );
    add(
        "edits (rule/user categorize, recategorize, late, amount, delete, note)",
        format!(
            "{}/{}, {}, {}, {}, {}, {}",
            c.rule_categorized,
            c.user_categorized,
            c.recategorized,
            c.late_edits,
            c.amount_edits,
            c.deletes,
            c.notes
        ),
    );

    // Size on disk.
    let all_docs: Vec<String> =
        w.h.device(LAPTOP)
            .docs_with_prefix("")
            .map(|(d, _)| d.clone())
            .collect();
    let dev = w.h.device_mut(LAPTOP);
    let mut sizes: Vec<(String, usize, usize)> = all_docs
        .iter()
        .map(|d| {
            let n = changes_in(dev.doc_mut(d));
            (d.clone(), dev.save(d).len(), n)
        })
        .collect();
    let total: usize = sizes.iter().map(|s| s.1).sum();
    let changes: usize = sizes.iter().map(|s| s.2).sum();
    add(
        "docs / Automerge changes",
        format!("{} / {changes}", all_docs.len()),
    );
    add("saved size, all docs", kib(total));
    let budget = model::budget_doc(BUDGET);
    let b = sizes.iter().find(|s| s.0 == budget).unwrap();
    add("budget doc", format!("{} ({} changes)", kib(b.1), b.2));
    sizes.retain(|s| s.0 != budget);
    let largest = sizes.iter().max_by_key(|s| s.1).unwrap();
    add(
        "largest txns doc",
        format!("{} ({} changes)", kib(largest.1), largest.2),
    );
    let bytes_per_txn = total as f64 / c.txns() as f64;
    add(
        "bytes per transaction (all history)",
        format!("{bytes_per_txn:.0}"),
    );

    let at = now(&w);
    let current = current_month_doc(&w);
    let period_start = read::current_period(&BudgetView::load(w.h.device(LAPTOP), BUDGET), at);

    // Cold start: everything from disk, then the current-period screen
    // (categories + this month's spend) and every fund balance, from scratch.
    let started = Instant::now();
    let cold = cold_device(w.h.device_mut(LAPTOP), &all_docs);
    let loaded = started.elapsed();
    let view = BudgetView::load(&cold, BUDGET);
    let screen = spend_by_category(&view, period_start.0, period_start.1);
    let balances = fund_balances(&view, at);
    add(
        "cold start, load ALL docs + current screen + fund balances",
        format!("{} (load {})", ms(started.elapsed()), ms(loaded)),
    );
    drop(view);

    if layout == TxnLayout::Monthly {
        // Summaries built once (first run on a device, or after a restore).
        let started = Instant::now();
        let mut cache = SummaryCache::default();
        let heads =
            w.h.device_mut(LAPTOP)
                .heads_with_prefix(&model::txns_prefix(BUDGET));
        let n = cache.refresh(&BudgetView::load(w.h.device(LAPTOP), BUDGET), &heads);
        add(
            "build summary cache (one-off, docs in memory)",
            format!("{} for {n} docs", ms(started.elapsed())),
        );

        let started = Instant::now();
        let cold = cold_device(w.h.device_mut(LAPTOP), &[budget.clone(), current.clone()]);
        let loaded = started.elapsed();
        let view = BudgetView::load(&cold, BUDGET);
        let partial_screen = spend_by_category(&view, period_start.0, period_start.1);
        let cats = read::categories(view.doc);
        let cached: Vec<_> = FUNDS
            .iter()
            .map(|i| cache.fund_balance(view.doc, &cats, &category_id(*i), at))
            .collect();
        add(
            "cold start, budget + current month + cached summaries",
            format!("{} (load {})", ms(started.elapsed()), ms(loaded)),
        );
        assert_eq!(partial_screen, screen);
        assert_eq!(cached, balances);
    }

    add(
        "add one transaction + commit, at year 5",
        format!("{:.0} µs", write_cost(&mut w).as_secs_f64() * 1e6),
    );

    // One edit on the laptop, synced to the other two devices (after first
    // syncing the probe writes above, so they aren't counted).
    w.sync();
    w.h.relay.stats = Default::default();
    let started = Instant::now();
    let today = w.h.device(LAPTOP).clock.now().date_naive();
    model::set_note(w.h.device_mut(LAPTOP), BUDGET, today, "probe-0", "one edit");
    w.sync();
    add(
        "sync ONE edit to 2 peers, at year 5",
        format!(
            "{}; {} in {} msgs",
            ms(started.elapsed()),
            kib(w.h.relay.stats.bytes as usize),
            w.h.relay.stats.messages
        ),
    );

    // A week offline: bob's phone keeps editing; the others keep importing,
    // editing and syncing among themselves.
    w.go_offline("bob-phone");
    w.run_days(7);
    w.sync();
    let to_bob = missing_change_bytes(&mut w, LAPTOP, "bob-phone");
    let from_bob = missing_change_bytes(&mut w, "bob-phone", LAPTOP);
    w.h.relay.stats = Default::default();
    let started = Instant::now();
    w.go_online("bob-phone");
    w.sync();
    let elapsed = started.elapsed();
    add(
        "week offline: merge (all 3 devices to quiescence)",
        format!(
            "{}; sync protocol {} in {} msgs",
            ms(elapsed),
            kib(w.h.relay.stats.bytes as usize),
            w.h.relay.stats.messages
        ),
    );
    add(
        "week offline: raw changes to / from bob (secure-log payload)",
        format!("{} / {}", kib(to_bob), kib(from_bob)),
    );

    // Reconnect with nothing new: per-doc handshake overhead.
    w.h.relay.stats = Default::default();
    w.go_offline("alice-phone");
    w.go_online("alice-phone");
    w.sync();
    add(
        "no-op reconnect (sync protocol)",
        format!(
            "{} in {} msgs",
            kib(w.h.relay.stats.bytes as usize),
            w.h.relay.stats.messages
        ),
    );

    // A brand-new device joins and syncs everything over the sync protocol.
    w.h.relay.stats = Default::default();
    let clock = SimClock::at(at);
    w.h.add_device("new-tablet", clock);
    model::open_budget(w.h.device_mut("new-tablet"), BUDGET);
    let started = Instant::now();
    workload::sync_household(&mut w.h, BUDGET);
    add(
        "new device, full initial sync (sync protocol)",
        format!(
            "{}; {} in {} msgs",
            ms(started.elapsed()),
            kib(w.h.relay.stats.bytes as usize),
            w.h.relay.stats.messages
        ),
    );
    assert_eq!(
        BudgetView::load(w.h.device("new-tablet"), BUDGET)
            .ledger()
            .len() as u64,
        w.counts.txns() + u64::from(PROBES)
    );
    rows
}

#[test]
#[ignore = "measurement; run with --release -- --ignored --nocapture"]
fn five_year_household_monthly_vs_single_doc() {
    let monthly = measure(TxnLayout::Monthly);
    let single = measure(TxnLayout::Single);
    println!("| Measure | Per-month docs | Single doc |\n|---|---|---|");
    for m in &monthly {
        let s = single
            .iter()
            .find(|s| s.0 == m.0)
            .map(|s| s.1.as_str())
            .unwrap_or("n/a");
        println!("| {} | {} | {} |", m.0, m.1, s);
    }
}

// --- Plaid `modified` moving a row across months (disposition follow-up) --------

mod plaid_modified {
    use super::*;
    use common::{date, household, offline, online, sync, B};
    use local_first_automerge_spike::model::{BankRevision, BankRow, BudgetMeta};
    use local_first_automerge_spike::Harness;

    const ACCOUNT: &str = "acct-1";
    const PTX: &str = "ptx-jan30";

    fn setup() -> (Harness, String) {
        let mut h = household(
            &["a", "b"],
            SimClock::ymd(2026, 2, 10),
            &BudgetMeta::monthly("Home"),
        );
        let a = h.device_mut("a");
        model::add_category(a, B, "food", "Groceries", 500_00);
        let id = model::import_bank_txn(
            a,
            B,
            &BankRow {
                external_account_id: ACCOUNT,
                provider_transaction_id: PTX,
                date: date(2026, 1, 30),
                amount: 40_00,
                description: "PENDING GROCER",
            },
        );
        model::set_txn_category(a, B, date(2026, 1, 30), &id, "food");
        // February already has a doc of its own.
        model::add_txn(
            a,
            B,
            &model::NewTxn {
                id: "feb-1",
                date: date(2026, 2, 5),
                amount: 10_00,
                description: "coffee",
                category: Some("food"),
            },
        )
        .unwrap();
        sync(&mut h);
        (h, id)
    }

    fn revision(seq: u64, d: NaiveDate, amount: Cents) -> BankRevision<'static> {
        BankRevision {
            external_account_id: ACCOUNT,
            provider_transaction_id: PTX,
            first_seen: date(2026, 1, 30),
            seq,
            date: d,
            amount,
            description: "GROCER #12",
        }
    }

    fn spend(h: &Harness, dev: &str, month: u32) -> BTreeMap<String, Cents> {
        let v = BudgetView::load(h.device(dev), B);
        spend_by_category(&v, date(2026, month, 1), date(2026, month + 1, 1))
    }

    #[test]
    fn a_row_revised_into_next_month_counts_there_but_stays_in_its_first_seen_doc() {
        let (mut h, id) = setup();
        assert!(model::apply_bank_revision(
            h.device_mut("a"),
            B,
            &revision(2, date(2026, 2, 2), 42_00)
        ));
        sync(&mut h);
        assert_converged(&mut h);

        // One row, still in January's doc, now dated and counted in February.
        let jan = model::txns_doc(B, date(2026, 1, 1));
        let v = BudgetView::load(h.device("b"), B);
        let row = v.ledger().into_iter().find(|t| t.id == id).unwrap();
        assert_eq!((row.date, row.amount), (date(2026, 2, 2), 42_00));
        assert_eq!(
            v.txns
                .iter()
                .filter(|(_, d)| read::ledger(d).iter().any(|t| t.id == id))
                .count(),
            1
        );
        assert!(read::ledger(v.doc_of(&jan)).iter().any(|t| t.id == id));
        assert_eq!(spend(&h, "b", 1), BTreeMap::new());
        assert_eq!(spend(&h, "b", 2), BTreeMap::from([("food".into(), 52_00)]));

        // Reading February: February's doc alone misses the row; the `moved`
        // index names January's doc as well, and with it the total is right.
        let budget = model::budget_doc(B);
        let feb_docs = read::docs_for_period(h.device("b").doc(&budget), B, "2026-02");
        assert_eq!(feb_docs, vec![model::txns_doc(B, date(2026, 2, 1)), jan]);
        let feb_only = cold_device(h.device_mut("b"), &[budget.clone(), feb_docs[0].clone()]);
        let v = BudgetView::load(&feb_only, B);
        assert_eq!(
            spend_by_category(&v, date(2026, 2, 1), date(2026, 3, 1)),
            BTreeMap::from([("food".into(), 10_00)]),
            "without the index, the revised row is missed"
        );
        let mut docs = vec![budget];
        docs.extend(feb_docs);
        let indexed = cold_device(h.device_mut("b"), &docs);
        let v = BudgetView::load(&indexed, B);
        assert_eq!(
            spend_by_category(&v, date(2026, 2, 1), date(2026, 3, 1)),
            BTreeMap::from([("food".into(), 52_00)])
        );
    }

    #[test]
    fn a_replayed_older_revision_never_overwrites_a_newer_one() {
        let (mut h, id) = setup();
        assert!(model::apply_bank_revision(
            h.device_mut("a"),
            B,
            &revision(3, date(2026, 2, 2), 42_00)
        ));
        sync(&mut h);
        // b replays an older page of the feed after already syncing seq 3: it
        // is refused locally, so no causal overwrite hides seq 3...
        assert!(!model::apply_bank_revision(
            h.device_mut("b"),
            B,
            &revision(2, date(2026, 1, 31), 41_00)
        ));
        // ...and replaying the original import only rewrites identical facts.
        model::import_bank_txn(
            h.device_mut("b"),
            B,
            &BankRow {
                external_account_id: ACCOUNT,
                provider_transaction_id: PTX,
                date: date(2026, 1, 30),
                amount: 40_00,
                description: "PENDING GROCER",
            },
        );
        sync(&mut h);
        assert_converged(&mut h);
        for dev in ["a", "b"] {
            let row = BudgetView::load(h.device(dev), B)
                .ledger()
                .into_iter()
                .find(|t| t.id == id)
                .unwrap();
            assert_eq!((row.date, row.amount), (date(2026, 2, 2), 42_00), "{dev}");
        }
    }

    #[test]
    fn concurrent_revisions_resolve_to_the_highest_sequence_not_lww() {
        // Both assignments, so the result can't be an accident of which actor
        // Automerge's LWW happens to favor.
        for (a_seq, b_seq) in [(2, 3), (3, 2)] {
            let (mut h, id) = setup();
            offline(&mut h, &["a", "b"]);
            let rev = |seq| {
                if seq == 3 {
                    revision(3, date(2026, 1, 31), 45_00)
                } else {
                    revision(2, date(2026, 2, 2), 42_00)
                }
            };
            assert!(model::apply_bank_revision(
                h.device_mut("a"),
                B,
                &rev(a_seq)
            ));
            assert!(model::apply_bank_revision(
                h.device_mut("b"),
                B,
                &rev(b_seq)
            ));
            online(&mut h, &["a", "b"]);
            sync(&mut h);
            assert_converged(&mut h);
            for dev in ["a", "b"] {
                let row = BudgetView::load(h.device(dev), B)
                    .ledger()
                    .into_iter()
                    .find(|t| t.id == id)
                    .unwrap();
                assert_eq!(
                    (row.date, row.amount),
                    (date(2026, 1, 31), 45_00),
                    "{dev}, a={a_seq} b={b_seq}"
                );
            }
        }
    }
}
