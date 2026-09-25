//! Raw Automerge behaviors the data model depends on, pinned as tests so the
//! findings doc can cite them and a version bump that changes them fails here.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjType, ReadDoc, ROOT};

fn pair() -> (AutoCommit, AutoCommit) {
    let a = AutoCommit::new().with_actor(ActorId::from(b"aaaa".to_vec()));
    let b = AutoCommit::new().with_actor(ActorId::from(b"bbbb".to_vec()));
    (a, b)
}

fn merge_both(a: &mut AutoCommit, b: &mut AutoCommit) {
    a.merge(b).unwrap();
    b.merge(a).unwrap();
}

/// Two devices that each lazily create the same nested container end up with
/// TWO objects under one key. Only the winner is visible, so everything the
/// loser wrote inside its object silently disappears from `get`.
#[test]
fn concurrent_creation_of_the_same_container_hides_one_side() {
    let (mut a, mut b) = pair();
    let ta = a.put_object(ROOT, "transactions", ObjType::Map).unwrap();
    a.put(&ta, "t1", "from a").unwrap();
    let tb = b.put_object(ROOT, "transactions", ObjType::Map).unwrap();
    b.put(&tb, "t2", "from b").unwrap();
    merge_both(&mut a, &mut b);

    let (_, winner) = a.get(ROOT, "transactions").unwrap().unwrap();
    assert_eq!(
        a.keys(&winner).count(),
        1,
        "only one side's transactions are visible"
    );
    assert_eq!(a.get_all(ROOT, "transactions").unwrap().len(), 2);
}

/// The fix for shared containers: every device builds the document from the
/// SAME genesis change (fixed actor, time and message). Identical changes have
/// identical hashes, so the second copy is a no-op on merge and both devices
/// write into one shared object.
#[test]
fn a_deterministic_genesis_change_dedupes_across_devices() {
    let genesis = |actor: &[u8]| {
        let mut d = AutoCommit::new().with_actor(ActorId::from(b"genesis".to_vec()));
        d.put_object(ROOT, "transactions", ObjType::Map).unwrap();
        d.commit_with(
            CommitOptions::default()
                .with_message("genesis")
                .with_time(0),
        );
        d.set_actor(ActorId::from(actor.to_vec()));
        d
    };
    let mut a = genesis(b"aaaa");
    let mut b = genesis(b"bbbb");
    assert_eq!(
        a.get_heads(),
        b.get_heads(),
        "same genesis hash on both devices"
    );

    let (_, ta) = a.get(ROOT, "transactions").unwrap().unwrap();
    a.put(&ta, "t1", "from a").unwrap();
    let (_, tb) = b.get(ROOT, "transactions").unwrap().unwrap();
    b.put(&tb, "t2", "from b").unwrap();
    merge_both(&mut a, &mut b);

    assert_eq!(a.get_all(ROOT, "transactions").unwrap().len(), 1);
    let (_, t) = a.get(ROOT, "transactions").unwrap().unwrap();
    assert_eq!(
        a.keys(&t).count(),
        2,
        "both sides' transactions are visible"
    );
}

/// Delete a map key on one side, edit a field INSIDE that key's object on the
/// other: the delete wins and the edit is gone from the visible state. A
/// concurrent re-`put` of the key itself would survive, but a nested edit does
/// not re-assert the key.
#[test]
fn delete_beats_a_concurrent_nested_edit() {
    let (mut a, mut b) = pair();
    let txns = a.put_object(ROOT, "transactions", ObjType::Map).unwrap();
    let t1 = a.put_object(&txns, "t1", ObjType::Map).unwrap();
    a.put(&t1, "amount", 100).unwrap();
    b.merge(&mut a).unwrap();

    a.delete(&txns, "t1").unwrap();
    b.put(&t1, "amount", 250).unwrap();
    merge_both(&mut a, &mut b);

    assert!(a.get(&txns, "t1").unwrap().is_none());
    assert!(b.get(&txns, "t1").unwrap().is_none());
}

/// Counters merge by SUMMING concurrent increments. That is exactly wrong for
/// a "once per period" operation such as fund advancement: two devices that
/// each advance the same period double-count it.
#[test]
fn counters_sum_concurrent_increments() {
    let (mut a, mut b) = pair();
    a.put(ROOT, "fund_balance", automerge::ScalarValue::counter(0))
        .unwrap();
    b.merge(&mut a).unwrap();
    a.increment(ROOT, "fund_balance", 5_000).unwrap();
    b.increment(ROOT, "fund_balance", 5_000).unwrap();
    merge_both(&mut a, &mut b);
    let (v, _) = a.get(ROOT, "fund_balance").unwrap().unwrap();
    assert_eq!(v.as_i64(), Some(10_000));
}

/// Replicas holding the same change set do NOT necessarily save identical
/// bytes: the encoding depends on the order changes were applied locally.
/// Heads (and every read) are identical, so "byte-identical" convergence is
/// asserted on heads plus canonical rendered state, not on `save()` output.
#[test]
fn replicas_with_the_same_changes_have_equal_heads_but_not_equal_bytes() {
    let (mut a, mut b) = pair();
    a.put(ROOT, "x", 1).unwrap();
    b.put(ROOT, "y", 2).unwrap();
    a.merge(&mut b).unwrap();
    b.merge(&mut a).unwrap();
    let (mut ha, mut hb) = (a.get_heads(), b.get_heads());
    ha.sort();
    hb.sort();
    assert_eq!(ha, hb);
    assert_ne!(
        a.save(),
        b.save(),
        "save() bytes depend on local apply order"
    );
    // Not even a load/save round trip normalizes them.
    let ra = AutoCommit::load(&a.save()).unwrap().save();
    let rb = AutoCommit::load(&b.save()).unwrap().save();
    assert_ne!(ra, rb);
}
