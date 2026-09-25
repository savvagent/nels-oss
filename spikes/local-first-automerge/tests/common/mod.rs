//! Shared fixtures for the Week 1 scenarios.

#![allow(dead_code)]

use chrono::NaiveDate;
use local_first_automerge_spike::model::{self, BudgetMeta};
use local_first_automerge_spike::snapshot::render;
use local_first_automerge_spike::{Harness, SimClock};

pub const B: &str = "b1";

pub fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// A household: the first device creates budget `b1` with `meta`, every other
/// device opens it (from the shared genesis) and they sync.
pub fn household(devices: &[&str], clock: SimClock, meta: &BudgetMeta) -> Harness {
    let mut h = Harness::new();
    for id in devices {
        h.add_device(id, clock);
    }
    model::create_budget(h.device_mut(devices[0]), B, meta);
    for id in &devices[1..] {
        model::open_budget(h.device_mut(id), B);
    }
    sync(&mut h);
    h
}

/// Sync to quiescence, including discovery: after each pass every device joins
/// any transactions doc the budget's `periods` index lists, then syncs again.
pub fn sync(h: &mut Harness) {
    loop {
        h.sync_all();
        let mut joined = 0;
        for id in h.device_ids() {
            if h.device(&id).is_online() {
                let budgets: Vec<String> = h
                    .device(&id)
                    .docs_with_prefix("budget/")
                    .map(|(d, _)| d.trim_start_matches("budget/").to_string())
                    .collect();
                for b in budgets {
                    joined += model::join_listed_periods(h.device_mut(&id), &b);
                }
            }
        }
        if joined == 0 {
            return;
        }
    }
}

pub fn offline(h: &mut Harness, ids: &[&str]) {
    for id in ids {
        h.go_offline(id);
    }
}

pub fn online(h: &mut Harness, ids: &[&str]) {
    for id in ids {
        h.go_online(id);
    }
}

/// Every device holds the same set of docs, and for each doc the same heads
/// and the same visible state. (Not `save()` bytes: see
/// `tests/automerge_semantics.rs`.)
pub fn assert_converged(h: &mut Harness) {
    let ids = h.device_ids();
    let doc_ids = |h: &Harness, id: &str| -> Vec<String> {
        h.device(id)
            .docs_with_prefix("")
            .map(|(d, _)| d.clone())
            .filter(|d| !d.starts_with("private/"))
            .collect()
    };
    let reference = doc_ids(h, &ids[0]);
    for id in &ids[1..] {
        assert_eq!(
            doc_ids(h, id),
            reference,
            "{id} holds a different set of docs"
        );
    }
    for doc in &reference {
        let heads0 = h.device_mut(&ids[0]).heads(doc);
        let render0 = render(h.device(&ids[0]).doc(doc));
        for id in &ids[1..] {
            assert_eq!(
                h.device_mut(id).heads(doc),
                heads0,
                "{id} heads differ on {doc}"
            );
            assert_eq!(
                render(h.device(id).doc(doc)),
                render0,
                "{id} state differs on {doc}"
            );
        }
    }
}
