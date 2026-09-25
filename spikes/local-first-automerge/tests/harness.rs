//! Day 1: prove the harness itself before any scenario relies on it.

use automerge::transaction::Transactable;
use automerge::{ObjType, ReadDoc, ROOT};
use chrono::Duration;
use local_first_automerge_spike::snapshot::{conflicts, render};
use local_first_automerge_spike::{Harness, SimClock};

const BUDGET: &str = "budget/b1";

fn two_devices() -> Harness {
    let mut h = Harness::new();
    h.add_device("alice-laptop", SimClock::ymd(2026, 3, 15));
    h.add_device("bob-phone", SimClock::ymd(2026, 3, 15));
    h.device_mut("alice-laptop")
        .change(BUDGET, "create budget", |d| {
            d.put_object(ROOT, "categories", ObjType::Map).unwrap();
            d.put(ROOT, "name", "Household").unwrap();
        });
    h.device_mut("bob-phone").join(BUDGET);
    h.sync_all();
    h
}

fn categories(h: &Harness, device: &str) -> automerge::ObjId {
    let doc = h.device(device).doc(BUDGET);
    doc.get(ROOT, "categories").unwrap().unwrap().1
}

#[test]
fn a_joined_device_receives_existing_state() {
    let h = two_devices();
    assert_eq!(
        render(h.device("alice-laptop").doc(BUDGET)),
        render(h.device("bob-phone").doc(BUDGET)),
    );
    assert!(h.relay.stats.messages > 0 && h.relay.stats.bytes > 0);
}

#[test]
fn offline_edits_on_both_sides_converge_after_reconnect() {
    let mut h = two_devices();
    h.go_offline("bob-phone");

    let cats = categories(&h, "alice-laptop");
    h.device_mut("alice-laptop")
        .change(BUDGET, "add groceries", |d| {
            d.put(&cats, "cat-groceries", "Groceries").unwrap();
        });
    let cats = categories(&h, "bob-phone");
    h.device_mut("bob-phone").change(BUDGET, "add fuel", |d| {
        d.put(&cats, "cat-fuel", "Fuel").unwrap();
    });

    // Nothing crosses while Bob is offline.
    h.sync_all();
    assert!(h
        .device("alice-laptop")
        .doc(BUDGET)
        .get(&cats, "cat-fuel")
        .unwrap()
        .is_none());

    h.go_online("bob-phone");
    h.sync_all();

    let a = render(h.device("alice-laptop").doc(BUDGET));
    assert_eq!(a, render(h.device("bob-phone").doc(BUDGET)));
    assert!(a.contains("Groceries") && a.contains("Fuel"), "{a}");
    assert_eq!(
        h.device_mut("alice-laptop").heads(BUDGET),
        h.device_mut("bob-phone").heads(BUDGET)
    );
}

#[test]
fn concurrent_writes_to_one_key_keep_a_detectable_conflict() {
    let mut h = two_devices();
    h.go_offline("bob-phone");
    h.device_mut("alice-laptop")
        .change(BUDGET, "rename", |d| d.put(ROOT, "name", "Home").unwrap());
    h.device_mut("bob-phone")
        .change(BUDGET, "rename", |d| d.put(ROOT, "name", "Family").unwrap());
    h.go_online("bob-phone");
    h.sync_all();

    let a = h.device("alice-laptop").doc(BUDGET);
    let b = h.device("bob-phone").doc(BUDGET);
    assert_eq!(render(a), render(b), "both sides pick the same winner");
    assert_eq!(
        conflicts(a, &ROOT, "name"),
        2,
        "the losing value is still visible"
    );
}

#[test]
fn three_devices_converge_regardless_of_sync_order() {
    let run = |order: &[(&str, &str)]| {
        let mut h = Harness::new();
        for id in ["a", "b", "c"] {
            h.add_device(id, SimClock::ymd(2026, 3, 15)).join(BUDGET);
            h.go_offline(id);
        }
        for id in ["a", "b", "c"] {
            h.device_mut(id).change(BUDGET, "edit", |d| {
                d.put(ROOT, format!("from-{id}"), id).unwrap();
                d.put(ROOT, "contested", id).unwrap();
            });
        }
        for id in ["a", "b", "c"] {
            h.go_online(id);
        }
        let pairs: Vec<_> = order
            .iter()
            .map(|(x, y)| (x.to_string(), y.to_string()))
            .collect();
        h.sync_in_order(&pairs);
        let rendered: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|id| render(h.device(id).doc(BUDGET)))
            .collect();
        assert!(rendered.iter().all(|r| r == &rendered[0]), "{rendered:?}");
        rendered[0].clone()
    };

    let forward = run(&[("a", "b"), ("b", "c"), ("a", "c")]);
    let reverse = run(&[("c", "a"), ("c", "b"), ("b", "a")]);
    assert_eq!(forward, reverse);
}

#[test]
fn a_private_document_never_reaches_a_device_that_has_not_joined_it() {
    let mut h = two_devices();
    h.device_mut("alice-laptop")
        .change("private/alice", "retirement", |d| {
            d.put(ROOT, "balance", 250_000.0).unwrap();
        });
    h.sync_all();
    assert!(!h.device("bob-phone").has_doc("private/alice"));
}

#[test]
fn each_device_has_its_own_clock_and_commits_use_it() {
    let mut h = two_devices();
    h.device_mut("alice-laptop")
        .clock
        .advance(Duration::days(20));
    assert_eq!(
        h.device("alice-laptop")
            .clock
            .now()
            .format("%Y-%m")
            .to_string(),
        "2026-04"
    );
    assert_eq!(
        h.device("bob-phone")
            .clock
            .now()
            .format("%Y-%m")
            .to_string(),
        "2026-03"
    );

    h.device_mut("alice-laptop")
        .change(BUDGET, "april edit", |d| d.put(ROOT, "k", 1).unwrap());
    let now = h.device("alice-laptop").clock.now().timestamp();
    let change = h
        .device_mut("alice-laptop")
        .doc_mut(BUDGET)
        .get_last_local_change()
        .unwrap();
    assert_eq!(change.timestamp(), now);
}
