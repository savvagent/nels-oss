//! Week 1, the remaining scenarios (1–4, 6, 7, 11, 13, 14). The 🔴 ones plus
//! #10 and #12 are in `week1_red.rs`.

// Money literals are cents grouped as dollars_cents: `400_00` is $400.00.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use automerge::ReadDoc;
use common::*;
use local_first_automerge_spike::model::{self, BankRow, BudgetMeta, NewTxn};
use local_first_automerge_spike::read::{self, BudgetView};
use local_first_automerge_spike::snapshot::conflicts;
use local_first_automerge_spike::{Harness, SimClock};

const T1: &str = "t1";

fn txn<'a>(
    id: &'a str,
    date: chrono::NaiveDate,
    amount: i64,
    category: Option<&'a str>,
) -> NewTxn<'a> {
    NewTxn {
        id,
        date,
        amount,
        description: id,
        category,
    }
}

/// A two-device household with one category and one transaction `t1`
/// ($40 on 2026-03-10 in "cat-food"), already synced.
fn seeded() -> Harness {
    let mut h = household(
        &["a", "b"],
        SimClock::ymd(2026, 3, 10),
        &BudgetMeta::monthly("Home"),
    );
    model::add_category(h.device_mut("a"), B, "cat-food", "Food", 400_00);
    model::add_category(h.device_mut("a"), B, "cat-fuel", "Fuel", 200_00);
    model::add_txn(
        h.device_mut("a"),
        B,
        &txn(T1, date(2026, 3, 10), 40_00, Some("cat-food")),
    )
    .unwrap();
    sync(&mut h);
    h
}

fn t1(h: &Harness, dev: &str) -> read::Txn {
    let v = BudgetView::load(h.device(dev), B);
    v.transactions().into_iter().find(|t| t.id == T1).unwrap()
}

fn row(amount: i64) -> BankRow<'static> {
    BankRow {
        external_account_id: "acct-1",
        provider_transaction_id: "plaid-abc",
        date: date(2026, 3, 11),
        amount,
        description: "COSTCO #123",
    }
}

// --- #1 same bank row imported twice ---------------------------------------------

#[test]
fn s01_the_same_bank_row_imported_on_two_devices_is_one_transaction() {
    let mut h = seeded();
    offline(&mut h, &["a", "b"]);
    let id_a = model::import_bank_txn(h.device_mut("a"), B, &row(87_12));
    let id_b = model::import_bank_txn(h.device_mut("b"), B, &row(87_12));
    assert_eq!(
        id_a, id_b,
        "deterministic id from (account, provider txn id)"
    );
    // Each device also edits the row before syncing.
    model::set_txn_category(h.device_mut("a"), B, date(2026, 3, 11), &id_a, "cat-food");
    model::edit_amount(h.device_mut("b"), B, date(2026, 3, 11), &id_b, 80_00);

    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let v = BudgetView::load(h.device("a"), B);
    let imported: Vec<_> = v
        .transactions()
        .into_iter()
        .filter(|t| t.provider_id.is_some())
        .collect();
    assert_eq!(imported.len(), 1);
    // Both pre-sync edits survive: the import only wrote facts, so neither
    // edit raced an import write.
    assert_eq!(imported[0].category.as_deref(), Some("cat-food"));
    assert_eq!(imported[0].amount, 80_00);
    // The facts were written twice with identical values: a conflict exists
    // but is invisible because both values are equal.
    let doc = v.txns[0].1;
    assert_eq!(conflicts(doc, &model::map(doc, "amount"), &id_a), 2);
}

// --- #2 same amount edited twice -------------------------------------------------

#[test]
fn s02_concurrent_amount_edits_pick_one_winner_and_keep_the_loser_visible() {
    let mut h = seeded();
    offline(&mut h, &["a", "b"]);
    model::edit_amount(h.device_mut("a"), B, date(2026, 3, 10), T1, 45_00);
    model::edit_amount(h.device_mut("b"), B, date(2026, 3, 10), T1, 55_00);
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let (ta, tb) = (t1(&h, "a"), t1(&h, "b"));
    assert_eq!(ta, tb);
    assert!(ta.amount == 45_00 || ta.amount == 55_00);
    assert_eq!(
        ta.amount_edit_conflict,
        vec![45_00, 55_00],
        "the read can surface both"
    );

    // Any later edit (made after seeing both) resolves it.
    model::edit_amount(h.device_mut("b"), B, date(2026, 3, 10), T1, 50_00);
    sync(&mut h);
    assert_eq!(t1(&h, "a").amount, 50_00);
    assert!(t1(&h, "a").amount_edit_conflict.is_empty());
}

// --- #3 amount vs category ---------------------------------------------------------

#[test]
fn s03_edits_to_different_fields_both_survive() {
    let mut h = seeded();
    offline(&mut h, &["a", "b"]);
    model::edit_amount(h.device_mut("a"), B, date(2026, 3, 10), T1, 42_50);
    model::set_txn_category(h.device_mut("b"), B, date(2026, 3, 10), T1, "cat-fuel");
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let t = t1(&h, "a");
    assert_eq!((t.amount, t.category.as_deref()), (42_50, Some("cat-fuel")));
}

// --- #4 delete vs edit -----------------------------------------------------------------

#[test]
fn s04_delete_wins_but_a_concurrent_edit_is_kept_and_flagged() {
    let mut h = seeded();
    offline(&mut h, &["a", "b"]);
    model::delete_txn(h.device_mut("a"), B, date(2026, 3, 10), T1);
    model::edit_amount(h.device_mut("b"), B, date(2026, 3, 10), T1, 99_00);
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let t = t1(&h, "b");
    assert!(t.deleted, "hidden from totals");
    assert_eq!(
        t.amount, 99_00,
        "B's edit is still in the data, not silently dropped"
    );
    assert!(
        t.edited_concurrently_with_delete,
        "so the UI can offer 'restore'"
    );
    let v = BudgetView::load(h.device("a"), B);
    assert_eq!(read::total_spent(&v, date(2026, 3, 1), date(2026, 4, 1)), 0);
}

#[test]
fn s04_an_edit_before_the_delete_is_not_flagged() {
    let mut h = seeded();
    model::edit_amount(h.device_mut("b"), B, date(2026, 3, 10), T1, 99_00);
    sync(&mut h);
    model::delete_txn(h.device_mut("a"), B, date(2026, 3, 10), T1);
    sync(&mut h);
    let t = t1(&h, "b");
    assert!(t.deleted && !t.edited_concurrently_with_delete);
}

// --- #6 delete category vs assign ----------------------------------------------------

#[test]
fn s06_a_transaction_assigned_to_a_concurrently_deleted_category_becomes_uncategorized() {
    let mut h = seeded();
    offline(&mut h, &["a", "b"]);
    model::delete_category(h.device_mut("a"), B, "cat-fuel");
    model::set_txn_category(h.device_mut("b"), B, date(2026, 3, 10), T1, "cat-fuel");
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let v = BudgetView::load(h.device("a"), B);
    let cats = read::categories(v.doc);
    assert_eq!(
        cats.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec!["cat-food"]
    );
    // The stored id dangles; the read maps it to Uncategorized, like today's
    // `ON DELETE SET NULL`. The transaction and its money are not lost.
    assert_eq!(t1(&h, "a").category.as_deref(), Some("cat-fuel"));
    let (m3, m4) = (date(2026, 3, 1), date(2026, 4, 1));
    assert_eq!(read::spent(&v, &cats, None, m3, m4), 40_00);
    assert_eq!(read::total_spent(&v, m3, m4), 40_00);
}

// --- #7 concurrent rename ------------------------------------------------------------------

#[test]
fn s07_concurrent_renames_pick_one_name() {
    let mut h = seeded();
    offline(&mut h, &["a", "b"]);
    model::rename_category(h.device_mut("a"), B, "cat-food", "Groceries");
    model::rename_category(h.device_mut("b"), B, "cat-food", "Food & Drink");
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let v = BudgetView::load(h.device("a"), B);
    let name = &read::categories(v.doc)
        .into_iter()
        .find(|c| c.id == "cat-food")
        .unwrap()
        .name;
    assert!(name == "Groceries" || name == "Food & Drink");
    let cats = model::map(v.doc, "categories");
    let (_, food) = v.doc.get(&cats, "cat-food").unwrap().unwrap();
    assert_eq!(conflicts(v.doc, &food, "name"), 2);
}

// --- #11 fund limit edit vs late prior-period txn ---------------------------------------------

#[test]
fn s11_a_limit_change_applies_to_its_own_period_and_a_late_txn_lands_in_its_own() {
    let mut h = household(
        &["a", "b"],
        SimClock::ymd(2026, 2, 5),
        &BudgetMeta::monthly("Home"),
    );
    model::add_category(h.device_mut("a"), B, "car", "Car", 400_00);
    model::set_fund(h.device_mut("a"), B, "car", true);
    model::add_txn(
        h.device_mut("a"),
        B,
        &txn("feb-1", date(2026, 2, 6), 300_00, Some("car")),
    )
    .unwrap();
    sync(&mut h);

    for dev in ["a", "b"] {
        h.device_mut(dev).clock = SimClock::ymd(2026, 3, 10);
    }
    offline(&mut h, &["a", "b"]);
    model::set_limit(h.device_mut("a"), B, "car", 600_00); // March's limit
                                                           // A February receipt entered late, in March.
    model::add_txn(
        h.device_mut("b"),
        B,
        &txn("feb-late", date(2026, 2, 20), 100_00, Some("car")),
    )
    .unwrap();
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let balance = |h: &Harness| {
        let v = BudgetView::load(h.device("a"), B);
        read::fund_balance(
            &v,
            &read::categories(v.doc),
            "car",
            h.device("a").clock.now(),
        )
    };
    // February: 400 − (300 + 100). The late transaction counts where it
    // belongs, and March's new limit does not reach back into February.
    assert_eq!(balance(&h), Some(0));
    h.device_mut("a").clock = SimClock::ymd(2026, 4, 1);
    assert_eq!(balance(&h), Some(600_00), "March: 600 − 0");

    let v = BudgetView::load(h.device("a"), B);
    assert_eq!(read::limit_in(v.doc, "car", date(2026, 2, 1)), 400_00);
    assert_eq!(read::limit_in(v.doc, "car", date(2026, 3, 1)), 600_00);
    assert_eq!(
        read::limit_in(v.doc, "car", date(2026, 7, 1)),
        600_00,
        "carries forward"
    );
}

// --- #13 viewer edit (merge only; enforcement is Week 2) ------------------------------------

#[test]
fn s13_a_viewer_edit_merges_like_any_other_unless_the_relay_rejects_it() {
    let mut h = seeded();
    model::add_member(h.device_mut("a"), B, "owner-a", "owner");
    model::add_member(h.device_mut("a"), B, "viewer-b", "view");
    sync(&mut h);

    // Nothing in the document stops a viewer's device from writing: roles are
    // data, not permissions. Week 2 has to enforce them at the relay and on
    // receive.
    model::rename_category(h.device_mut("b"), B, "cat-food", "Viewer was here");
    sync(&mut h);
    let v = BudgetView::load(h.device("a"), B);
    assert!(read::categories(v.doc)
        .iter()
        .any(|c| c.name == "Viewer was here"));
}

// --- #14 three devices, three-way concurrent edits on 1–9 ------------------------------------

/// Everything a user would see, as one string, so runs can be compared.
fn derived(h: &Harness, dev: &str) -> String {
    let now = h.device(dev).clock.now();
    let vs: Vec<BudgetView> = [B, "b2"]
        .iter()
        .map(|id| BudgetView::load(h.device(dev), id))
        .collect();
    let cats = read::categories(vs[0].doc);
    let r = read::resolve_rollup(&vs);
    let (m3, m4) = (date(2026, 3, 1), date(2026, 4, 1));
    format!(
        "cats={cats:?}\ntxns={:?}\nrollup={r:?}\nb1={} b2={}\nfund={:?}",
        vs[0].transactions(),
        read::rollup_spent(&vs, &r, B, m3, m4),
        read::rollup_spent(&vs, &r, "b2", m3, m4),
        read::fund_balance(&vs[0], &cats, "cat-food", now),
    )
}

fn three_way(order: &[(&str, &str)]) -> (Vec<String>, String) {
    let mut h = household(
        &["a", "b", "c"],
        SimClock::ymd(2026, 3, 10),
        &BudgetMeta::monthly("Home"),
    );
    model::create_budget(h.device_mut("a"), "b2", &BudgetMeta::monthly("Kids"));
    for dev in ["b", "c"] {
        model::open_budget(h.device_mut(dev), "b2");
    }
    model::add_category(h.device_mut("a"), B, "cat-food", "Food", 400_00);
    model::add_category(h.device_mut("a"), B, "cat-fuel", "Fuel", 200_00);
    model::set_fund(h.device_mut("a"), B, "cat-food", true);
    model::add_txn(
        h.device_mut("a"),
        B,
        &txn(T1, date(2026, 3, 10), 40_00, Some("cat-food")),
    )
    .unwrap();
    model::add_txn(
        h.device_mut("a"),
        "b2",
        &txn("k1", date(2026, 3, 10), 9_00, None),
    )
    .unwrap();
    sync(&mut h);
    offline(&mut h, &["a", "b", "c"]);

    // a
    let d = h.device_mut("a");
    let bank = model::import_bank_txn(d, B, &row(87_12)); // #1
    model::edit_amount(d, B, date(2026, 3, 10), T1, 41_00); // #2
    model::add_category(d, B, "cat-g-a", "Groceries", 300_00); // #5
    model::delete_category(d, B, "cat-fuel"); // #6
    model::rename_category(d, B, "cat-food", "Groceries & Food"); // #7
    model::set_rollup_parent(d, B, "b2"); // #9
                                          // b
    let d = h.device_mut("b");
    model::import_bank_txn(d, B, &row(87_12)); // #1
    model::set_txn_category(d, B, date(2026, 3, 11), &bank, "cat-food"); // #1/#3
    model::edit_amount(d, B, date(2026, 3, 10), T1, 42_00); // #2
    model::add_category(d, B, "cat-g-b", "groceries", 350_00); // #5
    model::set_txn_category(d, B, date(2026, 3, 10), T1, "cat-fuel"); // #3/#6
    model::set_rollup_parent(d, "b2", B); // #9
                                          // c
    let d = h.device_mut("c");
    d.clock = SimClock::ymd(2026, 4, 2); // #10: crossed the boundary
    model::delete_txn(d, B, date(2026, 3, 10), T1); // #4
    model::rename_category(d, B, "cat-food", "Eating"); // #7
    model::set_limit(d, B, "cat-food", 500_00); // #11
    model::add_txn(
        d,
        B,
        &txn("late", date(2026, 3, 30), 25_00, Some("cat-food")),
    )
    .unwrap(); // #11

    online(&mut h, &["a", "b", "c"]);
    let pairs: Vec<(String, String)> = order
        .iter()
        .map(|(x, y)| (x.to_string(), y.to_string()))
        .collect();
    loop {
        h.sync_in_order(&pairs);
        let mut joined = 0;
        for dev in ["a", "b", "c"] {
            joined += model::join_listed_periods(h.device_mut(dev), B)
                + model::join_listed_periods(h.device_mut(dev), "b2");
        }
        if joined == 0 {
            break;
        }
    }
    assert_converged(&mut h);

    // Put every device on the same clock so derived reads are comparable.
    for dev in ["a", "b", "c"] {
        h.device_mut(dev).clock = SimClock::ymd(2026, 4, 2);
    }
    let per_device: Vec<String> = ["a", "b", "c"].iter().map(|d| derived(&h, d)).collect();
    let heads = h.device_mut("a").heads(&model::budget_doc(B));
    (per_device, format!("{heads:?}"))
}

#[test]
fn s14_three_way_concurrent_edits_converge_in_every_sync_order() {
    let pairs = [("a", "b"), ("b", "c"), ("a", "c")];
    let mut results = Vec::new();
    for perm in permutations(&pairs) {
        let (per_device, heads) = three_way(&perm);
        assert!(
            per_device.iter().all(|d| d == &per_device[0]),
            "devices disagree for {perm:?}"
        );
        results.push((per_device[0].clone(), heads));
    }
    assert_eq!(results.len(), 6);
    assert!(
        results.iter().all(|r| r == &results[0]),
        "sync order changed the outcome"
    );

    // And the outcome makes sense: one Groceries (folded), the bank row once,
    // t1 deleted, a single valid rollup link.
    let view = &results[0].0;
    println!("{view}");
    assert!(view.contains("folded: [(\"cat-g-b\", 35000)]"), "{view}");
    assert_eq!(view.matches("provider_id: Some").count(), 1, "{view}");
}

fn permutations<T: Copy>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, first);
            out.push(p);
        }
    }
    out
}
