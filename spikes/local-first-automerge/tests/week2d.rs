//! Week 2d: version skew. Can two household members on different app
//! versions share a budget without one clobbering the other?
//!
//! `a` runs v1 (this crate's `model`/`read`, nothing else). `b` and `c` run
//! v2, which adds a category `color` (a new field in an existing object), a
//! `tags` column on transactions (a new CONTAINER), and a feature that
//! changes what existing numbers mean. Everything after setup syncs through
//! the secure relay, the transport we'd actually ship.

// Money literals are cents grouped as dollars_cents: `400_00` is $400.00.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjType, ReadDoc, ROOT};
use common::{date, B};
use local_first_automerge_spike::erasure;
use local_first_automerge_spike::model::{self, BudgetMeta, NewTxn};
use local_first_automerge_spike::read::{self, BudgetView};
use local_first_automerge_spike::schema::{self, UpgradeRequired, V1};
use local_first_automerge_spike::secure::{encode_changes, Kind, SecureNet};
use local_first_automerge_spike::snapshot::render;
use local_first_automerge_spike::{Harness, Rejection, Role, SimClock};

const V1_DEV: &str = "a"; // also the owner
const V2_DEV: &str = "b";
const V2_OTHER: &str = "c";
const MARCH: &str = "txns/b1/2026-03";

fn household() -> (Harness, SecureNet) {
    let mut h = Harness::new();
    let mut net = SecureNet::new();
    for d in [V1_DEV, V2_DEV, V2_OTHER] {
        h.add_device(d, SimClock::ymd(2026, 3, 10));
        net.enroll(d);
    }
    model::create_budget(h.device_mut(V1_DEV), B, &BudgetMeta::monthly("Home"));
    model::add_category(h.device_mut(V1_DEV), B, "cat-food", "Groceries", 400_00);
    add(&mut h, V1_DEV, "t1", 40_00);
    add(&mut h, V1_DEV, "t2", 25_00);
    net.create_group(
        &h,
        V1_DEV,
        B,
        &[
            (V1_DEV, Role::Owner),
            (V2_DEV, Role::Edit),
            (V2_OTHER, Role::Edit),
        ],
    )
    .unwrap();
    net.sync(&mut h, B);
    (h, net)
}

fn add(h: &mut Harness, dev: &str, id: &str, cents: i64) {
    model::add_txn(
        h.device_mut(dev),
        B,
        &NewTxn {
            id,
            date: date(2026, 3, 10),
            amount: cents,
            description: id,
            category: Some("cat-food"),
        },
    )
    .unwrap();
}

// --- v2 write paths (what the newer client does) ----------------------------------

fn v2_set_color(h: &mut Harness, dev: &str, cat: &str, color: &str) {
    h.device_mut(dev)
        .change(&model::budget_doc(B), "set color", |d| {
            let (_, c) = d.get(model::map(d, "categories"), cat).unwrap().unwrap();
            d.put(&c, "color", color).unwrap();
        });
}

fn v2_tag(h: &mut Harness, dev: &str, id: &str, tags: &str) {
    let d = h.device_mut(dev);
    schema::upgrade(d, MARCH, 2);
    d.change(MARCH, "tag", |doc| {
        doc.put(model::map(doc, "tags"), id, tags).unwrap();
    });
}

fn color(h: &Harness, dev: &str, cat: &str) -> Option<String> {
    let doc = h.device(dev).doc(&model::budget_doc(B));
    let (_, c) = doc.get(model::map(doc, "categories"), cat).unwrap()?;
    model::get_str(doc, &c, "color")
}

fn tag(h: &Harness, dev: &str, id: &str) -> Option<String> {
    let doc = h.device(dev).doc(MARCH);
    let (_, tags) = doc.get(ROOT, "tags").unwrap()?;
    model::get_str(doc, &tags, id)
}

fn live_ids(h: &Harness, dev: &str) -> Vec<String> {
    BudgetView::load(h.device(dev), B)
        .ledger()
        .into_iter()
        .filter(|t| !t.deleted)
        .map(|t| t.id)
        .collect()
}

fn assert_same(h: &mut Harness) {
    let devices = [V1_DEV, V2_DEV, V2_OTHER];
    let docs: Vec<String> = h
        .device(V1_DEV)
        .docs_with_prefix("")
        .map(|(d, _)| d.clone())
        .collect();
    for doc in &docs {
        let heads = h.device_mut(V1_DEV).heads(doc);
        let state = render(h.device(V1_DEV).doc(doc));
        for d in &devices[1..] {
            assert_eq!(h.device_mut(d).heads(doc), heads, "{d} heads on {doc}");
            assert_eq!(render(h.device(d).doc(doc)), state, "{d} state on {doc}");
        }
    }
}

// --- new fields: safe by construction ------------------------------------------------

#[test]
fn a_v1_client_preserves_a_v2_field_it_does_not_know() {
    let (mut h, mut net) = household();
    v2_set_color(&mut h, V2_DEV, "cat-food", "teal");
    net.sync(&mut h, B);
    // v1 edits the same category's other fields, field by field.
    model::rename_category(h.device_mut(V1_DEV), B, "cat-food", "Food");
    model::set_limit(h.device_mut(V1_DEV), B, "cat-food", 450_00);
    net.sync(&mut h, B);

    assert_same(&mut h);
    assert_eq!(color(&h, V2_DEV, "cat-food").as_deref(), Some("teal"));
    let cats = read::categories(BudgetView::load(h.device(V2_DEV), B).doc);
    assert_eq!((cats[0].name.as_str(), cats[0].limit), ("Food", 450_00));
}

#[test]
fn rewriting_a_whole_object_clobbers_fields_the_writer_does_not_know() {
    // Negative control for rule "field-level writes only": a v1 "save the
    // category form" that replaces the object with the fields it knows.
    let (mut h, mut net) = household();
    v2_set_color(&mut h, V2_DEV, "cat-food", "teal");
    net.sync(&mut h, B);
    h.device_mut(V1_DEV)
        .change(&model::budget_doc(B), "save category form", |d| {
            let cats = model::map(d, "categories");
            let c = d.put_object(&cats, "cat-food", ObjType::Map).unwrap();
            d.put(&c, "name", "Food").unwrap();
            d.put(&c, "category_type", "expense").unwrap();
            d.put(&c, "category_limit", 400_00).unwrap();
        });
    net.sync(&mut h, B);
    assert_eq!(color(&h, V2_DEV, "cat-food"), None, "b's color is gone");
}

// --- new containers --------------------------------------------------------------------

#[test]
fn two_genesis_versions_can_never_sync_again() {
    // Why genesis is frozen. A v2 that simply added `tags` to its genesis
    // gives a month doc two DIFFERENT changes by the genesis actor at seq 1.
    // Automerge refuses the merge outright, and it stays refused: that doc
    // can't sync between those two devices ever again.
    let v1 = |d: &mut AutoCommit| {
        for name in model::TXN_COLUMNS {
            d.put_object(ROOT, name, ObjType::Map).unwrap();
        }
    };
    let v2 = |d: &mut AutoCommit| {
        for name in model::TXN_COLUMNS.iter().chain(&["tags"]) {
            d.put_object(ROOT, *name, ObjType::Map).unwrap();
        }
    };
    let mut x = local_first_automerge_spike::device::genesis_doc(v1);
    let mut y = local_first_automerge_spike::device::genesis_doc(v2);
    let err = x.merge(&mut y).unwrap_err().to_string();
    assert!(
        err.contains("DuplicateSeqNumber") || err.contains("duplicate seq"),
        "{err}"
    );
    // Retrying after more edits changes nothing.
    x.put(model::map(&x, "date"), "t9", "2026-04-02").unwrap();
    assert!(x.merge(&mut y).is_err());
}

#[test]
fn independent_upgrades_create_one_container_and_v1_keeps_up() {
    let (mut h, mut net) = household();
    h.go_offline(V2_DEV);
    h.go_offline(V2_OTHER);
    // Both v2 devices upgrade the March doc without seeing each other.
    v2_tag(&mut h, V2_DEV, "t1", "work");
    v2_tag(&mut h, V2_OTHER, "t2", "gift");
    // And b keeps writing ordinary rows; these depend on its upgrade.
    add(&mut h, V2_DEV, "t3", 9_00);
    // v1 meanwhile writes to a row b tagged.
    model::set_note(h.device_mut(V1_DEV), B, date(2026, 3, 10), "t1", "lunch");
    h.go_online(V2_DEV);
    h.go_online(V2_OTHER);
    net.sync(&mut h, B);

    assert_same(&mut h);
    for dev in [V1_DEV, V2_DEV, V2_OTHER] {
        let doc = h.device(dev).doc(MARCH);
        assert_eq!(doc.get_all(ROOT, "tags").unwrap().len(), 1, "{dev}");
        assert_eq!(tag(&h, dev, "t1").as_deref(), Some("work"), "{dev}");
        assert_eq!(tag(&h, dev, "t2").as_deref(), Some("gift"), "{dev}");
        assert!(live_ids(&h, dev).contains(&"t3".to_string()), "{dev}");
    }
    // v1 reads its own numbers undisturbed.
    let v = BudgetView::load(h.device(V1_DEV), B);
    assert_eq!(
        read::total_spent(&v, date(2026, 3, 1), date(2026, 3, 31)),
        40_00 + 25_00 + 9_00
    );
    assert!(net.client(V1_DEV).rejected.is_empty());
}

#[test]
fn a_forged_upgrade_that_recreates_a_container_is_refused() {
    // A schema change carries no signer binding, so a malicious writer could
    // try to pass off a change that re-creates `date` (hiding every row) as
    // an upgrade. Receivers check its shape.
    let (mut h, mut net) = household();
    let mut forge = model::genesis_for(MARCH);
    forge.set_actor(ActorId::from(b"nels-schema-v9".to_vec()));
    forge.put_object(ROOT, "date", ObjType::Map).unwrap();
    forge.commit_with(CommitOptions::default().with_time(0));
    let c = forge.get_last_local_change().unwrap();
    let env = net.seal(V2_DEV, B, MARCH, Kind::Changes, &encode_changes(&[c]));
    net.relay.publish(env).unwrap(); // the relay can't see inside
    net.pull(&mut h, V1_DEV, B).unwrap();

    assert_eq!(net.client(V1_DEV).rejected, vec![Rejection::BadUpgrade]);
    assert_eq!(live_ids(&h, V1_DEV), vec!["t1", "t2"]);
}

#[test]
fn an_upgrade_squatting_on_the_real_v2_actor_is_refused() {
    // Creates only a new empty map, so it passes a shape check. But it uses
    // the actor the genuine v2 upgrade will use. Accepted, it would make that
    // upgrade a DuplicateSeqNumber on this doc forever. The canonical rebuild
    // catches it: this actor name only ever means "creates `tags`".
    let (mut h, mut net) = household();
    let real = schema::upgrade_change(MARCH, 2);
    let mut forge = model::genesis_for(MARCH);
    forge.set_actor(real.actor_id().clone());
    forge.put_object(ROOT, "zzz", ObjType::Map).unwrap();
    forge.commit_with(
        CommitOptions::default()
            .with_message("schema v2")
            .with_time(0),
    );
    let c = forge.get_last_local_change().unwrap();
    assert_eq!((c.actor_id(), c.seq()), (real.actor_id(), real.seq()));
    // The danger is real: once a doc holds the squatter, the genuine
    // upgrade can't be applied.
    let mut victim = model::genesis_for(MARCH);
    victim.apply_changes([c.clone()]).unwrap();
    assert!(victim.apply_changes([real.clone()]).is_err());
    let env = net.seal(V2_DEV, B, MARCH, Kind::Changes, &encode_changes(&[c]));
    net.relay.publish(env).unwrap();
    net.pull(&mut h, V1_DEV, B).unwrap();
    assert_eq!(net.client(V1_DEV).rejected, vec![Rejection::BadUpgrade]);

    // The genuine upgrade still goes through afterwards.
    v2_tag(&mut h, V2_DEV, "t1", "work");
    net.sync(&mut h, B);
    assert_eq!(tag(&h, V1_DEV, "t1").as_deref(), Some("work"));
}

#[test]
fn a_v1_compaction_keeps_v2_columns_and_strips_erased_rows_from_them() {
    let (mut h, mut net) = household();
    v2_tag(&mut h, V2_DEV, "t1", "work");
    v2_tag(&mut h, V2_DEV, "t2", "clinic visit");
    net.sync(&mut h, B);
    model::delete_txn(h.device_mut(V1_DEV), B, date(2026, 3, 10), "t2");
    net.compact(&mut h, V1_DEV, B).unwrap(); // the v1 owner compacts
    net.sync(&mut h, B);

    assert_same(&mut h);
    for dev in [V1_DEV, V2_DEV, V2_OTHER] {
        assert_eq!(tag(&h, dev, "t1").as_deref(), Some("work"), "{dev}");
        assert!(!erasure::history_mentions(
            h.device_mut(dev).doc_mut(MARCH),
            "clinic visit"
        ));
    }
    // v2 keeps writing into the same container after compaction.
    v2_tag(&mut h, V2_OTHER, "t1", "work,shared");
    net.sync(&mut h, B);
    assert_eq!(tag(&h, V2_DEV, "t1").as_deref(), Some("work,shared"));
    assert_eq!(
        h.device(V2_DEV)
            .doc(MARCH)
            .get_all(ROOT, "tags")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_rebase_keeps_an_upgrade_the_compactor_never_saw() {
    let (mut h, mut net) = household();
    h.go_offline(V2_OTHER);
    v2_tag(&mut h, V2_OTHER, "t1", "work");
    net.compact(&mut h, V1_DEV, B).unwrap(); // nobody else has upgraded
    h.go_online(V2_OTHER);
    net.sync(&mut h, B);

    assert_same(&mut h);
    assert_eq!(tag(&h, V1_DEV, "t1").as_deref(), Some("work"));
    assert_eq!(net.client(V2_OTHER).dropped_on_rebase, 0);
}

// --- meaning changes -----------------------------------------------------------------

#[test]
fn a_v1_client_refuses_a_budget_that_requires_v2() {
    let (mut h, mut net) = household();
    // An unknown enum value alone is not a meaning change; v1 still computes.
    h.device_mut(V2_DEV)
        .change(&model::budget_doc(B), "set strategy", |d| {
            d.put(model::map(d, "meta"), "strategy", "fifty_thirty_twenty")
                .unwrap();
        });
    net.sync(&mut h, B);
    let budget = h.device(V1_DEV).doc(&model::budget_doc(B)).clone();
    assert_eq!(schema::check_client(&budget, V1), Ok(()));
    let v = BudgetView::load(h.device(V1_DEV), B);
    assert_eq!(read::categories(v.doc).len(), 1);
    assert_eq!(
        read::total_spent(&v, date(2026, 3, 1), date(2026, 3, 31)),
        40_00 + 25_00
    );
    assert!(read::resolve_rollup(std::slice::from_ref(&v))
        .ignored
        .is_empty());

    // v2 starts using a feature v1 would sum wrongly (e.g. split amounts),
    // and marks the budget. Both v2 devices marking it converge to one key.
    schema::require(h.device_mut(V2_DEV), B, 2);
    schema::require(h.device_mut(V2_OTHER), B, 2);
    net.sync(&mut h, B);
    assert_same(&mut h);
    let budget = h.device(V1_DEV).doc(&model::budget_doc(B)).clone();
    assert_eq!(
        schema::check_client(&budget, V1),
        Err(UpgradeRequired {
            required: 2,
            client: 1
        })
    );
    assert_eq!(schema::check_client(&budget, 2), Ok(()));
}

// --- paired fields (disposition follow-up) ----------------------------------------

/// Two devices concurrently edit a (benefit, quoted-at-age) pair, the shape
/// of #466's Social Security fields. X switches to the figure quoted at 62;
/// Y updates only the benefit, which it believes is quoted at 67.
fn merge_pair(x_actor: &[u8], y_actor: &[u8], one_value: bool) -> (i64, i64) {
    let mut base = AutoCommit::new().with_actor(ActorId::from(b"base".to_vec()));
    let p = base.put_object(ROOT, "profile", ObjType::Map).unwrap();
    if one_value {
        base.put(&p, "ss", "2400|67").unwrap();
    } else {
        base.put(&p, "ss_benefit", 2400).unwrap();
        base.put(&p, "ss_at_age", 67).unwrap();
    }
    base.commit();
    let mut x = base.fork().with_actor(ActorId::from(x_actor.to_vec()));
    let mut y = base.fork().with_actor(ActorId::from(y_actor.to_vec()));
    if one_value {
        x.put(&p, "ss", "1700|62").unwrap();
        y.put(&p, "ss", "3500|67").unwrap();
    } else {
        x.put(&p, "ss_benefit", 1700).unwrap();
        x.put(&p, "ss_at_age", 62).unwrap();
        y.put(&p, "ss_benefit", 3500).unwrap();
    }
    x.merge(&mut y).unwrap();
    if one_value {
        let s = model::get_str(&x, &p, "ss").unwrap();
        let (b, a) = s.split_once('|').unwrap();
        (b.parse().unwrap(), a.parse().unwrap())
    } else {
        (
            model::get_i64(&x, &p, "ss_benefit").unwrap(),
            model::get_i64(&x, &p, "ss_at_age").unwrap(),
        )
    }
}

#[test]
fn paired_fields_tear_unless_stored_as_one_value() {
    let written = [(1700, 62), (3500, 67)];
    let separate: Vec<_> = [(b"xx", b"yy"), (b"yy", b"xx")]
        .iter()
        .map(|(x, y)| merge_pair(*x, *y, false))
        .collect();
    assert!(
        separate.iter().any(|pair| !written.contains(pair)),
        "some actor order tears the pair: {separate:?}"
    );
    for (x, y) in [(b"xx", b"yy"), (b"yy", b"xx")] {
        assert!(written.contains(&merge_pair(x, y, true)));
    }
}
