//! Week 1, the go/no-go core: the 🔴 scenarios (today's schema REJECTS one of
//! the writes) plus #10 and #12, which must pass outright.
//!
//! Each test: devices go offline, make the scenario's edits, reconnect, sync,
//! then assert on (a) convergence and (b) what the derived read shows the user.

// Money literals are cents grouped as dollars_cents: `400_00` is $400.00.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use automerge::ReadDoc;
use chrono::Duration;
use common::*;
use local_first_automerge_spike::model::{self, BudgetMeta, NewTxn};
use local_first_automerge_spike::read::{self, BudgetView};
use local_first_automerge_spike::snapshot::conflicts;
use local_first_automerge_spike::{Harness, SimClock};

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

// --- #5 🔴 duplicate category name --------------------------------------------------

#[test]
fn s05_duplicate_category_names_fold_into_one_at_read_time() {
    let mut h = household(
        &["a", "b"],
        SimClock::ymd(2026, 3, 10),
        &BudgetMeta::monthly("Home"),
    );
    offline(&mut h, &["a", "b"]);

    model::add_category(h.device_mut("a"), B, "cat-a", "Groceries", 400_00);
    model::add_txn(
        h.device_mut("a"),
        B,
        &txn("t-a", date(2026, 3, 11), 50_00, Some("cat-a")),
    )
    .unwrap();
    model::add_category(h.device_mut("b"), B, "cat-b", " groceries", 500_00);
    model::add_txn(
        h.device_mut("b"),
        B,
        &txn("t-b", date(2026, 3, 12), 30_00, Some("cat-b")),
    )
    .unwrap();

    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    // Merge keeps both categories: two ids, no conflict inside the CRDT.
    let doc = h.device("a").doc(&model::budget_doc(B));
    assert_eq!(doc.keys(model::map(doc, "categories")).count(), 2);

    // The read folds them: one visible "Groceries" (lowest id), both
    // transactions count toward it, and the other limit is kept for a prompt.
    for dev in ["a", "b"] {
        let view = BudgetView::load(h.device(dev), B);
        let cats = read::categories(view.doc);
        assert_eq!(cats.len(), 1, "{cats:?}");
        assert_eq!(cats[0].id, "cat-a");
        assert_eq!(cats[0].limit, 400_00);
        assert_eq!(cats[0].folded, vec![("cat-b".to_string(), 500_00)]);
        assert_eq!(
            read::spent(
                &view,
                &cats,
                Some("cat-a"),
                date(2026, 3, 1),
                date(2026, 4, 1)
            ),
            80_00
        );
    }

    // Folding is reversible and needs no special "merge" record: renaming the
    // duplicate un-folds it.
    model::rename_category(h.device_mut("b"), B, "cat-b", "Groceries (Costco)");
    sync(&mut h);
    let view = BudgetView::load(h.device("a"), B);
    let cats = read::categories(view.doc);
    assert_eq!(cats.len(), 2);
    assert_eq!(
        read::spent(
            &view,
            &cats,
            Some("cat-a"),
            date(2026, 3, 1),
            date(2026, 4, 1)
        ),
        50_00
    );
}

// --- #8 🔴 close vs offline add ---------------------------------------------------------

#[test]
fn s08_writes_a_device_made_without_seeing_the_close_are_kept_and_flagged() {
    let mut h = household(
        &["a", "b"],
        SimClock::ymd(2026, 3, 10),
        &BudgetMeta::project("Kitchen remodel"),
    );
    model::add_txn(
        h.device_mut("a"),
        B,
        &txn("t1", date(2026, 3, 10), 1_000_00, None),
    )
    .unwrap();
    sync(&mut h);

    offline(&mut h, &["b"]);
    model::close_budget(h.device_mut("a"), B);
    assert!(
        model::add_txn(h.device_mut("a"), B, &txn("tx", date(2026, 3, 11), 1, None)).is_err(),
        "closer refuses locally"
    );

    // B has not seen the close: its local guard passes.
    let b = h.device_mut("b");
    b.clock.advance(Duration::days(25));
    model::add_txn(b, B, &txn("t2", date(2026, 3, 20), 200_00, None)).unwrap();
    model::edit_amount(b, B, date(2026, 3, 10), "t1", 1_100_00);
    model::add_txn(b, B, &txn("t3", date(2026, 4, 2), 75_00, None)).unwrap(); // a month doc A never held

    online(&mut h, &["b"]);
    sync(&mut h);
    assert_converged(&mut h);

    for dev in ["a", "b"] {
        let view = BudgetView::load(h.device(dev), B);
        assert!(view.is_closed());
        // Policy: accept and flag. The money was really spent, so dropping it
        // would make the closed total wrong; instead the owner sees exactly
        // which rows arrived after the close.
        assert_eq!(
            read::changed_after_close(&view).unwrap(),
            vec!["t1", "t2", "t3"]
        );
        assert_eq!(
            read::total_spent(&view, date(2026, 1, 1), date(2027, 1, 1)),
            1_375_00
        );
    }
    // Once B has seen the close it refuses new writes like A does.
    assert!(model::add_txn(h.device_mut("b"), B, &txn("t4", date(2026, 4, 3), 1, None)).is_err());
}

#[test]
fn s08_nothing_is_flagged_when_every_write_preceded_the_close() {
    let mut h = household(
        &["a", "b"],
        SimClock::ymd(2026, 3, 10),
        &BudgetMeta::project("Kitchen remodel"),
    );
    model::add_txn(
        h.device_mut("b"),
        B,
        &txn("t1", date(2026, 3, 10), 1_000_00, None),
    )
    .unwrap();
    sync(&mut h);
    model::close_budget(h.device_mut("a"), B);
    sync(&mut h);
    let view = BudgetView::load(h.device("b"), B);
    assert_eq!(
        read::changed_after_close(&view).unwrap(),
        Vec::<String>::new()
    );
}

// --- #9 🔴 rollup cycle -----------------------------------------------------------------

fn two_budgets() -> Harness {
    let mut h = Harness::new();
    h.add_device("a", SimClock::ymd(2026, 3, 10));
    h.add_device("b", SimClock::ymd(2026, 3, 10));
    for id in ["x", "y", "z"] {
        model::create_budget(h.device_mut("a"), id, &BudgetMeta::monthly("budget"));
        model::open_budget(h.device_mut("b"), id);
    }
    sync(&mut h);
    let spend = [("x", 100_00), ("y", 20_00), ("z", 3_00)];
    for (id, amount) in spend {
        model::add_txn(
            h.device_mut("a"),
            id,
            &txn(&format!("t-{id}"), date(2026, 3, 10), amount, None),
        )
        .unwrap();
    }
    sync(&mut h);
    h
}

fn views<'a>(h: &'a Harness, dev: &str) -> Vec<BudgetView<'a>> {
    ["x", "y", "z"]
        .iter()
        .map(|id| BudgetView::load(h.device(dev), id))
        .collect()
}

#[test]
fn s09_a_merged_rollup_cycle_is_broken_deterministically_without_double_counting() {
    let mut h = two_budgets();
    offline(&mut h, &["a", "b"]);
    model::set_rollup_parent(h.device_mut("a"), "x", "y");
    model::set_rollup_parent(h.device_mut("b"), "y", "x");
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let (m3, m4) = (date(2026, 3, 1), date(2026, 4, 1));
    for dev in ["a", "b"] {
        let vs = views(&h, dev);
        let r = read::resolve_rollup(&vs);
        // Both links are in the data; the read keeps x -> y and ignores y -> x.
        assert_eq!(
            r.parent_of.into_iter().collect::<Vec<_>>(),
            vec![("x".into(), "y".into())]
        );
        assert_eq!(
            r.ignored,
            vec![(
                "y".into(),
                "x".into(),
                "parent is itself rolled up (single-level)"
            )]
        );
        let r = read::resolve_rollup(&vs);
        assert_eq!(read::rollup_spent(&vs, &r, "y", m3, m4), 120_00);
        assert_eq!(read::rollup_spent(&vs, &r, "x", m3, m4), 100_00);
        // Top-level totals account for every dollar exactly once.
        let tops: i64 = ["y", "z"]
            .iter()
            .map(|id| read::rollup_spent(&vs, &r, id, m3, m4))
            .sum();
        assert_eq!(tops, 123_00);
    }
}

#[test]
fn s09_a_merged_two_level_chain_is_cut_back_to_single_level() {
    let mut h = two_budgets();
    offline(&mut h, &["a", "b"]);
    model::set_rollup_parent(h.device_mut("a"), "x", "y");
    model::set_rollup_parent(h.device_mut("b"), "y", "z");
    online(&mut h, &["a", "b"]);
    sync(&mut h);

    let vs = views(&h, "b");
    let r = read::resolve_rollup(&vs);
    assert_eq!(
        r.parent_of.into_iter().collect::<Vec<_>>(),
        vec![("x".into(), "y".into())]
    );
    let r = read::resolve_rollup(&vs);
    assert_eq!(
        r.ignored,
        vec![(
            "y".into(),
            "z".into(),
            "child already has children (single-level)"
        )]
    );
}

// --- #10 🔴 fund advancement across a boundary (MUST PASS) --------------------------------

#[test]
fn s10_two_devices_crossing_a_boundary_offline_compute_the_same_fund_balance() {
    let mut h = household(
        &["a", "b"],
        SimClock::ymd(2026, 2, 10),
        &BudgetMeta::monthly("Home"),
    );
    model::add_category(h.device_mut("a"), B, "car", "Car repairs", 400_00);
    model::set_fund(h.device_mut("a"), B, "car", true);
    model::add_txn(
        h.device_mut("a"),
        B,
        &txn("feb", date(2026, 2, 12), 300_00, Some("car")),
    )
    .unwrap();
    sync(&mut h);

    // Negative control: the server's model as a CRDT. A stored balance that
    // each device "advances" at the boundary is applied twice.
    for dev in ["a", "b"] {
        h.device_mut(dev)
            .change(&model::budget_doc(B), "naive stored balance", |d| {
                use automerge::transaction::Transactable;
                d.put(
                    automerge::ROOT,
                    "naive_fund_balance",
                    automerge::ScalarValue::counter(0),
                )
                .unwrap();
            });
    }
    sync(&mut h);

    offline(&mut h, &["a", "b"]);
    h.device_mut("a").clock = SimClock::ymd(2026, 3, 1);
    h.device_mut("b").clock = SimClock::ymd(2026, 3, 4);
    for dev in ["a", "b"] {
        // Each device, independently, crosses into March.
        h.device_mut(dev)
            .change(&model::budget_doc(B), "naive advance", |d| {
                use automerge::transaction::Transactable;
                d.increment(automerge::ROOT, "naive_fund_balance", 100_00)
                    .unwrap();
            });
    }
    // Derived: before any sync, both already agree.
    let balance = |h: &Harness, dev: &str| {
        let v = BudgetView::load(h.device(dev), B);
        read::fund_balance(
            &v,
            &read::categories(v.doc),
            "car",
            h.device(dev).clock.now(),
        )
    };
    assert_eq!(balance(&h, "a"), Some(100_00));
    assert_eq!(balance(&h, "b"), Some(100_00));

    model::add_txn(
        h.device_mut("b"),
        B,
        &txn("mar", date(2026, 3, 4), 50_00, Some("car")),
    )
    .unwrap();
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    assert_eq!(balance(&h, "a"), Some(100_00));
    assert_eq!(
        balance(&h, "b"),
        Some(100_00),
        "March spend doesn't touch the balance until March completes"
    );
    let naive = model::get_i64(
        h.device("a").doc(&model::budget_doc(B)),
        &automerge::ROOT,
        "naive_fund_balance",
    );
    assert_eq!(
        naive,
        Some(200_00),
        "the stored-balance model double-advances"
    );

    // Into April: March completes on both, identically.
    for dev in ["a", "b"] {
        h.device_mut(dev).clock = SimClock::ymd(2026, 4, 2);
    }
    assert_eq!(balance(&h, "a"), Some(100_00 + 350_00));
    assert_eq!(balance(&h, "b"), Some(100_00 + 350_00));
}

#[test]
fn s10_disabled_periods_are_frozen_and_re_enabling_resumes() {
    let mut h = household(
        &["a"],
        SimClock::ymd(2026, 1, 5),
        &BudgetMeta::monthly("Home"),
    );
    let a = h.device_mut("a");
    model::add_category(a, B, "car", "Car", 100_00);
    model::set_fund(a, B, "car", true); // Jan counts
    a.clock = SimClock::ymd(2026, 2, 5);
    model::set_fund(a, B, "car", false); // Feb frozen
    a.clock = SimClock::ymd(2026, 3, 5);
    model::set_fund(a, B, "car", true); // Mar resumes
    a.clock = SimClock::ymd(2026, 4, 5);
    let v = BudgetView::load(h.device("a"), B);
    let cats = read::categories(v.doc);
    assert_eq!(
        read::fund_balance(&v, &cats, "car", h.device("a").clock.now()),
        Some(200_00)
    );
}

// --- #12 auto-renew across a boundary (MUST PASS) ----------------------------------------

#[test]
fn s12_auto_renew_is_derived_and_the_renewal_log_has_one_entry_per_period() {
    let meta = BudgetMeta {
        auto_renew: true,
        ..BudgetMeta::monthly("Home")
    };
    let mut h = household(&["a", "b"], SimClock::ymd(2026, 3, 30), &meta);
    offline(&mut h, &["a", "b"]);
    h.device_mut("a").clock = SimClock::ymd(2026, 4, 1);
    h.device_mut("b").clock = SimClock::ymd(2026, 4, 3);

    for dev in ["a", "b"] {
        let now = h.device(dev).clock.now();
        let v = BudgetView::load(h.device(dev), B);
        assert_eq!(
            read::current_period(&v, now),
            (date(2026, 4, 1), date(2026, 5, 1))
        );
        assert!(model::record_renewal(h.device_mut(dev), B));
    }
    online(&mut h, &["a", "b"]);
    sync(&mut h);
    assert_converged(&mut h);

    let v = BudgetView::load(h.device("a"), B);
    assert_eq!(
        read::renewals(&v),
        vec!["2026-04-01"],
        "one renewal, not two"
    );
    // Both devices wrote the key; the conflict is harmless (same key, and the
    // value is only which device noticed first).
    assert_eq!(
        conflicts(v.doc, &model::map(v.doc, "renewals"), "2026-04-01"),
        2
    );
}

#[test]
fn s12_closed_and_project_budgets_never_renew() {
    let meta = BudgetMeta {
        auto_renew: true,
        ..BudgetMeta::project("Trip")
    };
    let mut h = household(&["a"], SimClock::ymd(2026, 3, 30), &meta);
    assert!(!model::record_renewal(h.device_mut("a"), B));

    let meta = BudgetMeta {
        auto_renew: true,
        ..BudgetMeta::monthly("Home")
    };
    let mut h = household(&["a"], SimClock::ymd(2026, 3, 30), &meta);
    model::close_budget(h.device_mut("a"), B);
    assert!(!model::record_renewal(h.device_mut("a"), B));
}
