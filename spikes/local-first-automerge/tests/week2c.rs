//! Week 2c: erasure. Can "delete this transaction" / "delete my account"
//! actually remove data, given that a CRDT keeps its history?
//!
//! Like 2a, everything after setup syncs ONLY through `SecureNet`, because
//! compaction is a relay operation: the relay has to drop the old history
//! too.

// Money literals are cents grouped as dollars_cents: `400_00` is $400.00.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use automerge::ReadDoc;
use common::{date, B};
use local_first_automerge_spike::erasure;
use local_first_automerge_spike::model::{self, BudgetMeta, NewTxn};
use local_first_automerge_spike::read::{self, BudgetView};
use local_first_automerge_spike::secure::{Kind, SecureNet};
use local_first_automerge_spike::snapshot::render;
use local_first_automerge_spike::workload::{self, Spec, Workload, BUDGET, DEVICES};
use local_first_automerge_spike::{Harness, Rejection, Role, SimClock};

const OWNER: &str = "a";
const EDITOR: &str = "b";
const VIEWER: &str = "v";
const MARCH: &str = "txns/b1/2026-03";
/// The description we want gone. Distinctive, so a history search can't
/// match anything else.
const SECRET: &str = "Dr Okafor fertility clinic";

fn setup(meta: &BudgetMeta, members: &[(&str, Role)]) -> (Harness, SecureNet) {
    let mut h = Harness::new();
    let mut net = SecureNet::new();
    for (d, _) in members {
        h.add_device(d, SimClock::ymd(2026, 3, 10));
        net.enroll(d);
    }
    model::create_budget(h.device_mut(OWNER), B, meta);
    model::add_category(h.device_mut(OWNER), B, "cat-food", "Groceries", 400_00);
    model::add_category(h.device_mut(OWNER), B, "cat-med", "Medical", 200_00);
    add(&mut h, OWNER, "t1", 40_00, "Corner shop", "cat-food");
    add(&mut h, OWNER, "secret", 150_00, SECRET, "cat-med");
    net.create_group(&h, OWNER, B, members).unwrap();
    net.sync(&mut h, B);
    (h, net)
}

fn household() -> (Harness, SecureNet) {
    setup(
        &BudgetMeta::monthly("Home"),
        &[
            (OWNER, Role::Owner),
            (EDITOR, Role::Edit),
            (VIEWER, Role::View),
        ],
    )
}

fn add(h: &mut Harness, dev: &str, id: &str, cents: i64, desc: &str, cat: &str) {
    model::add_txn(
        h.device_mut(dev),
        B,
        &NewTxn {
            id,
            date: date(2026, 3, 10),
            amount: cents,
            description: desc,
            category: Some(cat),
        },
    )
    .unwrap();
}

fn delete_secret(h: &mut Harness, dev: &str) {
    model::delete_txn(h.device_mut(dev), B, date(2026, 3, 10), "secret");
}

/// Does `needle` appear anywhere in any doc `dev` holds, history included?
fn mentions(h: &mut Harness, dev: &str, needle: &str) -> bool {
    let docs: Vec<String> = h
        .device(dev)
        .docs_with_prefix("")
        .map(|(d, _)| d.clone())
        .collect();
    docs.iter()
        .any(|d| erasure::history_mentions(h.device_mut(dev).doc_mut(d), needle))
}

fn change_count(h: &mut Harness, dev: &str, doc: &str) -> usize {
    h.device_mut(dev).doc_mut(doc).get_changes(&[]).len()
}

/// What a user sees: live rows `(id, amount, category)`, March spend,
/// category names.
type Numbers = (Vec<(String, i64, Option<String>)>, i64, Vec<String>);

fn numbers(h: &Harness, dev: &str) -> Numbers {
    let v = BudgetView::load(h.device(dev), B);
    let rows = v
        .transactions()
        .into_iter()
        .filter(|t| !t.deleted)
        .map(|t| (t.id, t.amount, t.category))
        .collect();
    let spent = read::total_spent(&v, date(2026, 3, 1), date(2026, 3, 31));
    let cats = read::categories(v.doc)
        .into_iter()
        .map(|c| c.name)
        .collect();
    (rows, spent, cats)
}

fn note(h: &Harness, dev: &str, id: &str) -> Option<String> {
    let doc = h.device(dev).doc(MARCH);
    model::get_str(doc, &model::map(doc, "note"), id)
}

/// Same docs, heads and visible state on each listed device.
fn assert_same(h: &mut Harness, devices: &[&str]) {
    let docs: Vec<String> = h
        .device(devices[0])
        .docs_with_prefix("")
        .map(|(d, _)| d.clone())
        .collect();
    for doc in &docs {
        let heads = h.device_mut(devices[0]).heads(doc);
        let state = render(h.device(devices[0]).doc(doc));
        for d in &devices[1..] {
            assert_eq!(h.device_mut(d).heads(doc), heads, "{d} heads on {doc}");
            assert_eq!(render(h.device(d).doc(doc)), state, "{d} state on {doc}");
        }
    }
}

// --- the problem --------------------------------------------------------------------

#[test]
fn a_deleted_transaction_is_still_in_the_doc_twice_over() {
    let (mut h, mut net) = household();
    let before_delete = h.device_mut(EDITOR).heads(MARCH);
    delete_secret(&mut h, EDITOR);
    net.sync(&mut h, B);

    // The UI hides it...
    let (rows, _, _) = numbers(&h, VIEWER);
    assert!(rows.iter().all(|(id, _, _)| id != "secret"));
    // ...but the tombstone sits beside the facts in the CURRENT state...
    let doc = h.device(VIEWER).doc(MARCH);
    assert_eq!(
        model::get_str(doc, &model::map(doc, "description"), "secret").as_deref(),
        Some(SECRET)
    );
    // ...and every earlier state has it live.
    let mut probe = h.device(VIEWER).doc(MARCH).clone();
    let past = probe.fork_at(&before_delete).unwrap();
    assert!(read::transactions(&past)
        .iter()
        .any(|t| t.id == "secret" && !t.deleted));
    assert!(mentions(&mut h, VIEWER, SECRET));
}

#[test]
fn a_rotation_snapshot_is_not_erasure() {
    // 2a's key rotation compacts the RELAY's log, but its snapshot is a
    // `save()`: full history. A member added afterwards receives the deleted
    // row with it.
    let (mut h, mut net) = household();
    delete_secret(&mut h, EDITOR);
    net.sync(&mut h, B);
    h.add_device("new", SimClock::ymd(2026, 3, 12));
    net.enroll("new");
    net.set_members(
        &mut h,
        OWNER,
        B,
        &[
            (OWNER, Role::Owner),
            (EDITOR, Role::Edit),
            ("new", Role::View),
        ],
    )
    .unwrap()
    .expect("removing the viewer rotates");
    net.pull(&mut h, "new", B).unwrap();
    assert!(mentions(&mut h, "new", SECRET));
}

// --- compaction -----------------------------------------------------------------------

#[test]
fn compaction_erases_a_deleted_row_from_every_member_and_the_relay() {
    let (mut h, mut net) = household();
    model::set_note(
        h.device_mut(EDITOR),
        B,
        date(2026, 3, 10),
        "secret",
        "ask about IVF",
    );
    net.sync(&mut h, B);
    delete_secret(&mut h, EDITOR);
    net.sync(&mut h, B);
    let before = numbers(&h, VIEWER);
    let docs = h.device(OWNER).docs_with_prefix("").count();

    let cost = net.compact(&mut h, OWNER, B).unwrap();
    assert_eq!(cost.docs, docs);
    assert!(cost.bytes_after < cost.bytes_before);
    net.sync(&mut h, B);

    for dev in [OWNER, EDITOR, VIEWER] {
        assert!(!mentions(&mut h, dev, SECRET), "{dev} can still recover it");
        assert!(
            !mentions(&mut h, dev, "ask about IVF"),
            "{dev} kept the note"
        );
        // Genesis plus the owner's compaction change, nothing else.
        assert_eq!(change_count(&mut h, dev, MARCH), 2, "{dev}");
        assert_eq!(numbers(&h, dev), before, "{dev} numbers changed");
        // Only the tombstone survives: no row to list, but the marker stays
        // so a device or an import that writes the facts again can't bring
        // it back as live.
        let doc = h.device(dev).doc(MARCH);
        assert!(model::get_str(doc, &model::map(doc, "date"), "secret").is_none());
        assert!(doc
            .get(model::map(doc, "deleted"), "secret")
            .unwrap()
            .is_some());
    }
    assert_same(&mut h, &[OWNER, EDITOR, VIEWER]);

    // The relay holds exactly one envelope per doc: the compacted one.
    let stored = net.relay.leak_all(B);
    assert_eq!(stored.len(), docs);
    assert!(stored.iter().all(|e| e.kind == Kind::Compacted));

    // And the group keeps working afterwards.
    add(&mut h, EDITOR, "t2", 12_00, "Bakery", "cat-food");
    net.sync(&mut h, B);
    assert_eq!(numbers(&h, VIEWER).1, 40_00 + 12_00);
}

#[test]
fn a_member_who_joins_after_compaction_gets_no_history() {
    let (mut h, mut net) = household();
    delete_secret(&mut h, EDITOR);
    net.sync(&mut h, B);
    net.compact(&mut h, OWNER, B).unwrap();
    h.add_device("new", SimClock::ymd(2026, 3, 12));
    net.enroll("new");
    net.set_members(
        &mut h,
        OWNER,
        B,
        &[
            (OWNER, Role::Owner),
            (EDITOR, Role::Edit),
            (VIEWER, Role::View),
            ("new", Role::View),
        ],
    )
    .unwrap();
    net.pull(&mut h, "new", B).unwrap();
    assert!(!mentions(&mut h, "new", SECRET));
    assert_eq!(numbers(&h, "new"), numbers(&h, OWNER));
}

#[test]
fn an_offline_edit_is_rebased_onto_the_compaction_not_lost() {
    let (mut h, mut net) = household();
    h.go_offline(EDITOR);
    let d = h.device_mut(EDITOR);
    model::set_note(d, B, date(2026, 3, 10), "t1", "split with Bob");
    model::set_txn_category(d, B, date(2026, 3, 10), "t1", "cat-med");
    add(&mut h, EDITOR, "t2", 12_00, "Bakery", "cat-food");

    net.compact(&mut h, OWNER, B).unwrap();
    net.sync(&mut h, B); // the editor is still offline
    h.go_online(EDITOR);
    // The editor publishes under the old roster, is refused (StaleRoster),
    // fetches the compaction, rebases, and republishes.
    net.sync(&mut h, B);

    assert_same(&mut h, &[OWNER, EDITOR, VIEWER]);
    for dev in [OWNER, EDITOR, VIEWER] {
        assert_eq!(note(&h, dev, "t1").as_deref(), Some("split with Bob"));
        let (rows, spent, _) = numbers(&h, dev);
        assert!(rows.contains(&("t1".into(), 40_00, Some("cat-med".into()))));
        assert!(rows.iter().any(|(id, _, _)| id == "t2"));
        assert_eq!(spent, 40_00 + 12_00 + 150_00);
        // Genesis, compaction, and ONE rebase change: the editor's original
        // changes are not in anyone's history.
        assert_eq!(change_count(&mut h, dev, MARCH), 3, "{dev}");
    }
    assert_eq!(net.client(EDITOR).dropped_on_rebase, 0);
    assert!(net
        .relay
        .rejected
        .iter()
        .any(|r| matches!(r, Rejection::StaleRoster { .. })));
}

#[test]
fn a_rebased_edit_to_an_erased_row_is_dropped() {
    // Week 1 scenario 4 flags "edited concurrently with a delete" and keeps
    // both. After a compaction the edit can't be kept: re-applying it would
    // write the note back into a doc that was supposed to have forgotten the
    // row. Erasure wins.
    let (mut h, mut net) = household();
    h.go_offline(EDITOR);
    model::set_note(
        h.device_mut(EDITOR),
        B,
        date(2026, 3, 10),
        "secret",
        "call the clinic",
    );
    delete_secret(&mut h, OWNER);
    net.compact(&mut h, OWNER, B).unwrap();
    h.go_online(EDITOR);
    net.sync(&mut h, B);

    assert_eq!(net.client(EDITOR).dropped_on_rebase, 1);
    assert_same(&mut h, &[OWNER, EDITOR, VIEWER]);
    for dev in [OWNER, EDITOR, VIEWER] {
        assert!(!mentions(&mut h, dev, "call the clinic"), "{dev}");
        assert!(!mentions(&mut h, dev, SECRET), "{dev}");
        assert_eq!(note(&h, dev, "secret"), None);
    }
}

#[test]
fn pre_compaction_envelopes_are_refused_even_from_a_compromised_relay() {
    let (mut h, mut net) = household();
    delete_secret(&mut h, EDITOR);
    net.sync(&mut h, B);
    // Everything the relay held before compaction, including the envelope
    // that carries the row's facts.
    let old = net.relay.leak_all(B);
    assert!(old
        .iter()
        .any(|e| e.doc_id == MARCH && e.kind == Kind::Changes));
    net.compact(&mut h, OWNER, B).unwrap();
    net.sync(&mut h, B);

    // A compromised relay (or a backup of it) replays the old log.
    for env in old {
        net.relay.inject_unchecked(env);
    }
    net.pull(&mut h, VIEWER, B).unwrap();
    assert!(net
        .client(VIEWER)
        .rejected
        .iter()
        .all(|r| *r == Rejection::PreCompaction));
    assert!(!net.client(VIEWER).rejected.is_empty());
    assert!(!mentions(&mut h, VIEWER, SECRET));
}

#[test]
fn a_compaction_that_races_a_publish_is_refused_then_retried() {
    let (mut h, mut net) = household();
    // The owner is caught up; then the editor publishes before the owner
    // compacts. Accepting would make the relay delete an accepted write.
    add(&mut h, EDITOR, "t3", 7_00, "Coffee", "cat-food");
    net.push(&mut h, EDITOR, B).unwrap();
    let stored = net.relay.leak_all(B).len();
    assert_eq!(
        net.compact_as_of_last_fetch(&mut h, OWNER, B).unwrap_err(),
        Rejection::CompactionRaced
    );
    assert_eq!(net.relay.leak_all(B).len(), stored, "relay unchanged");

    net.compact(&mut h, OWNER, B).unwrap();
    net.sync(&mut h, B);
    assert_same(&mut h, &[OWNER, EDITOR, VIEWER]);
    assert!(numbers(&h, VIEWER).0.iter().any(|(id, _, _)| id == "t3"));
}

#[test]
fn a_closed_budget_keeps_its_after_close_answer_through_compaction() {
    // `close_seen` stores change hashes, and compaction deletes every change
    // hash. The compactor settles the answer first and writes it down.
    let (mut h, mut net) = setup(
        &BudgetMeta::project("Kitchen"),
        &[(OWNER, Role::Owner), (EDITOR, Role::Edit)],
    );
    h.go_offline(EDITOR);
    add(&mut h, EDITOR, "late", 99_00, "Tiles", "cat-food");
    model::close_budget(h.device_mut(OWNER), B);
    net.sync(&mut h, B);
    h.go_online(EDITOR);
    net.sync(&mut h, B);
    let late = |h: &Harness, dev: &str| {
        read::changed_after_close(&BudgetView::load(h.device(dev), B)).unwrap()
    };
    assert_eq!(late(&h, OWNER), vec!["late".to_string()]);

    net.compact(&mut h, OWNER, B).unwrap();
    net.sync(&mut h, B);
    for dev in [OWNER, EDITOR] {
        assert_eq!(late(&h, dev), vec!["late".to_string()], "{dev}");
    }
}

// --- measurement ----------------------------------------------------------------------

#[test]
#[ignore = "measurement: cargo test --release --test week2c -- --ignored --nocapture"]
fn compaction_cost_on_a_five_year_household() {
    let started = std::time::Instant::now();
    let mut w = Workload::run(Spec::five_years(model::TxnLayout::Monthly));
    let generated = started.elapsed();
    let h = &mut w.h;
    let mut net = SecureNet::new();
    for d in DEVICES {
        net.enroll(d);
    }
    let owner = DEVICES[0];
    net.create_group(
        h,
        owner,
        BUDGET,
        &[
            (DEVICES[0], Role::Owner),
            (DEVICES[1], Role::Edit),
            (DEVICES[2], Role::Edit),
        ],
    )
    .unwrap();
    let started = std::time::Instant::now();
    net.sync(h, BUDGET);
    let initial = started.elapsed();
    let relay_before = net.relay.stored_bytes(BUDGET);
    let deleted = BudgetView::load(h.device(owner), BUDGET)
        .ledger()
        .iter()
        .filter(|t| t.deleted)
        .count();
    let balances = |h: &Harness, dev: &str| {
        let v = BudgetView::load(h.device(dev), BUDGET);
        let cats = read::categories(v.doc);
        let now = h.device(dev).clock.now();
        workload::FUNDS
            .iter()
            .map(|i| read::fund_balance(&v, &cats, &workload::category_id(*i), now))
            .collect::<Vec<_>>()
    };
    let before = balances(h, DEVICES[2]);

    // Erasing ONE transaction only needs its month doc compacted: time that
    // alone on the largest month doc (not wired through the relay here).
    let (largest, one_doc) = {
        let dev = h.device(owner);
        let (id, doc) = dev
            .docs_with_prefix(&model::txns_prefix(BUDGET))
            .max_by_key(|(_, d)| (*d).clone().save().len())
            .unwrap();
        let (id, mut doc) = (id.clone(), doc.clone());
        let t = std::time::Instant::now();
        let mut out = erasure::compact(&mut doc, &id, h.device(owner).actor().clone(), 0, |_| {});
        let _ = out.save();
        (id, t.elapsed())
    };

    let cost = net.compact(h, owner, BUDGET).unwrap();
    let started = std::time::Instant::now();
    net.pull(h, DEVICES[2], BUDGET).unwrap();
    let member_apply = started.elapsed();
    net.sync(h, BUDGET);

    println!(
        "5y household ({} txns, {deleted} deleted), generate {generated:?}, initial secure sync {initial:?}\n\
         compaction of {} docs (owner: build+save+seal) {:?}\n\
         saved size {} KiB -> {} KiB; relay stored {} KiB -> {} KiB\n\
         member replaces all docs in {member_apply:?}\n\
         one month doc alone ({largest}): build+save {one_doc:?}",
        w.counts.txns(),
        cost.docs,
        cost.elapsed,
        cost.bytes_before / 1024,
        cost.bytes_after / 1024,
        relay_before / 1024,
        net.relay.stored_bytes(BUDGET) / 1024,
    );
    assert_eq!(balances(h, DEVICES[2]), before);
}

#[test]
fn a_bank_reimport_does_not_write_an_erased_row_back() {
    // The server still runs bank linking and can replay its feed (a new
    // device's first sync, a refresh). Without a check, re-importing would
    // put the erased description straight back into the doc.
    let (mut h, mut net) = household();
    let row = model::BankRow {
        external_account_id: "acct-1",
        provider_transaction_id: "p-77",
        date: date(2026, 3, 11),
        amount: 150_00,
        description: SECRET,
    };
    let id =
        model::import_bank_batch(h.device_mut(OWNER), B, std::slice::from_ref(&row))[0].clone();
    model::delete_txn(h.device_mut(OWNER), B, date(2026, 3, 11), &id);
    // Drop the manual row that also carries the secret, so only the bank row
    // is in play.
    delete_secret(&mut h, OWNER);
    net.sync(&mut h, B);
    net.compact(&mut h, OWNER, B).unwrap();
    net.sync(&mut h, B);
    assert!(!mentions(&mut h, EDITOR, SECRET));

    model::import_bank_batch(h.device_mut(EDITOR), B, &[row]);
    net.sync(&mut h, B);
    for dev in [OWNER, EDITOR, VIEWER] {
        assert!(!mentions(&mut h, dev, SECRET), "{dev}");
    }
}
