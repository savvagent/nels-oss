//! Week 2a: an end-to-end encrypted relay with enforced roles.
//!
//! Every test here syncs ONLY through `SecureNet` (never `Harness::sync_all`),
//! so nothing reaches a device except envelopes the relay accepted and the
//! device verified itself.

// Money literals are cents grouped as dollars_cents: `400_00` is $400.00.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use automerge::sync::{State, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ReadDoc, ROOT};
use common::{date, B};
use local_first_automerge_spike::model::{self, BudgetMeta, NewTxn};
use local_first_automerge_spike::read::{self, BudgetView};
use local_first_automerge_spike::secure::{Fetched, Kind, Roster, SealedEnvelope, SecureNet};
use local_first_automerge_spike::snapshot::render;
use local_first_automerge_spike::{Harness, Rejection, Role, SimClock};

const OWNER: &str = "a";
const EDITOR: &str = "b";
const VIEWER: &str = "v";

/// A budget created on `a` and shared with `members`, synced once.
fn shared(members: &[(&str, Role)]) -> (Harness, SecureNet) {
    let mut h = Harness::new();
    let mut net = SecureNet::new();
    for (d, _) in members {
        h.add_device(d, SimClock::ymd(2026, 3, 10));
        net.enroll(d);
    }
    model::create_budget(h.device_mut(OWNER), B, &BudgetMeta::monthly("Home"));
    model::add_category(h.device_mut(OWNER), B, "cat-food", "Groceries", 400_00);
    add(&mut h, OWNER, "t1", 40_00);
    net.create_group(&h, OWNER, B, members).unwrap();
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

fn cat_names(h: &Harness, dev: &str) -> Vec<String> {
    let v = BudgetView::load(h.device(dev), B);
    read::categories(v.doc)
        .into_iter()
        .map(|c| c.name)
        .collect()
}

fn txn_ids(h: &Harness, dev: &str) -> Vec<String> {
    let v = BudgetView::load(h.device(dev), B);
    v.transactions().into_iter().map(|t| t.id).collect()
}

/// Same docs, heads and visible state on each listed device.
fn assert_same(h: &mut Harness, devices: &[&str]) {
    let docs: Vec<String> = h
        .device(devices[0])
        .docs_with_prefix("")
        .map(|(d, _)| d.clone())
        .collect();
    for d in &devices[1..] {
        let other: Vec<String> = h
            .device(d)
            .docs_with_prefix("")
            .map(|(d, _)| d.clone())
            .collect();
        assert_eq!(other, docs, "{d} holds different docs");
    }
    for doc in &docs {
        let heads = h.device_mut(devices[0]).heads(doc);
        let state = render(h.device(devices[0]).doc(doc));
        for d in &devices[1..] {
            assert_eq!(h.device_mut(d).heads(doc), heads, "{d} heads on {doc}");
            assert_eq!(render(h.device(d).doc(doc)), state, "{d} state on {doc}");
        }
    }
}

// --- why Automerge's own sync protocol can't go through a blind relay ------------------------

#[test]
fn a_viewer_must_send_sync_messages_just_to_read() {
    let mut writer = AutoCommit::new();
    writer.put(ROOT, "x", 1).unwrap();
    let mut viewer = AutoCommit::new();
    let (mut ws, mut vs) = (State::new(), State::new());

    // The viewer has made no edits, but the protocol opens with its message
    // (heads + bloom filter). Drop it and the writer never learns what to send.
    let hello = viewer.sync().generate_sync_message(&mut vs);
    assert!(hello.is_some(), "a viewer's first message is mandatory");
    writer
        .sync()
        .receive_sync_message(&mut ws, hello.unwrap())
        .unwrap();
    let reply = writer.sync().generate_sync_message(&mut ws).unwrap();
    viewer.sync().receive_sync_message(&mut vs, reply).unwrap();
    assert!(viewer.get(ROOT, "x").unwrap().is_some());
}

#[test]
fn a_sync_message_carries_changes_its_sender_did_not_author() {
    let mut a = AutoCommit::new().with_actor("aaaa".as_bytes().into());
    a.put(ROOT, "from_a", 1).unwrap();
    let mut b = AutoCommit::new().with_actor("bbbb".as_bytes().into());
    b.merge(&mut a).unwrap();

    // b's message to a fresh peer c contains a's change: whoever signs b's
    // message is not the author of what it carries.
    let mut c = AutoCommit::new();
    let (mut bs, mut cs) = (State::new(), State::new());
    for _ in 0..4 {
        if let Some(m) = c.sync().generate_sync_message(&mut cs) {
            b.sync().receive_sync_message(&mut bs, m).unwrap();
        }
        if let Some(m) = b.sync().generate_sync_message(&mut bs) {
            c.sync().receive_sync_message(&mut cs, m).unwrap();
        }
    }
    let authors: Vec<_> = c
        .get_changes(&[])
        .iter()
        .map(|ch| ch.actor_id().clone())
        .collect();
    assert_eq!(authors, vec!["aaaa".as_bytes().into()]);
}

// --- the secure relay ------------------------------------------------------------------------

#[test]
fn members_converge_through_the_blind_relay() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        (VIEWER, Role::View),
    ]);
    add(&mut h, EDITOR, "t2", 25_00);
    net.sync(&mut h, B);
    for d in [OWNER, EDITOR, VIEWER] {
        assert_eq!(txn_ids(&h, d), vec!["t1", "t2"], "{d}");
    }
    assert_same(&mut h, &[OWNER, EDITOR, VIEWER]);
    assert!(net.relay.rejected.is_empty());
}

#[test]
fn the_relay_sees_headers_and_ciphertext_only() {
    let (h, net) = shared(&[(OWNER, Role::Owner), (EDITOR, Role::Edit)]);
    let _ = h;
    let leaked = net.relay.leak_all(B);
    assert!(!leaked.is_empty());
    for env in &leaked {
        // Only secrets long enough that random ciphertext can't contain them
        // by chance: a 2-byte id like "t1" occasionally shows up in random
        // bytes (it failed once this way during 2b).
        for secret in [&b"Groceries"[..], b"cat-food", b"2026-03-10"] {
            assert!(
                !env.ciphertext.windows(secret.len()).any(|w| w == secret),
                "plaintext {:?} visible to the relay",
                String::from_utf8_lossy(secret)
            );
        }
    }
}

#[test]
fn s13_a_viewer_edit_is_rejected_at_the_relay_and_never_reaches_anyone() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        (VIEWER, Role::View),
    ]);

    model::rename_category(h.device_mut(VIEWER), B, "cat-food", "Viewer was here");
    assert_eq!(net.push(&mut h, VIEWER, B), Err(Rejection::NotAWriter));
    net.sync(&mut h, B);

    for d in [OWNER, EDITOR] {
        assert_eq!(cat_names(&h, d), vec!["Groceries"], "{d}");
    }
    assert_same(&mut h, &[OWNER, EDITOR]);
    assert!(net.relay.rejected.contains(&Rejection::NotAWriter));

    // The viewer still RECEIVES: the rejection blocks its writes, not its reads.
    add(&mut h, OWNER, "t2", 10_00);
    net.sync(&mut h, B);
    assert_eq!(txn_ids(&h, VIEWER), vec!["t1", "t2"]);
    // Recorded, not a pass: the viewer's device keeps its own rejected edit
    // and is now forked on that doc. A real client must refuse the edit
    // before committing (the UI is read-only) or roll the doc back.
    assert_eq!(cat_names(&h, VIEWER), vec!["Viewer was here"]);
}

#[test]
fn a_non_member_can_neither_write_nor_read() {
    let (mut h, mut net) = shared(&[(OWNER, Role::Owner)]);
    h.add_device("x", SimClock::ymd(2026, 3, 10));
    net.enroll("x");
    // x learned the group id and owner key somehow; it's still not on the roster.
    let owner_pk = net.client(OWNER).identity.public();
    net.clients.get_mut("x").unwrap().accept_invite(B, owner_pk);
    model::open_budget(h.device_mut("x"), B);
    model::add_category(h.device_mut("x"), B, "cat-x", "Mine", 1);
    assert_eq!(net.push(&mut h, "x", B), Err(Rejection::NotAMember));
    // Even a hand-built envelope under a key x made up is refused at the relay.
    let forged = SealedEnvelope::seal(
        &net.client("x").identity,
        B,
        &model::budget_doc(B),
        1,
        1,
        Kind::Changes,
        &[0; 32],
        b"",
    );
    assert_eq!(net.relay.publish(forged), Err(Rejection::NotAMember));
    assert_eq!(net.pull(&mut h, "x", B), Err(Rejection::NotAMember));
}

// --- a compromised relay: every check is repeated on receive ---------------------------------

/// Hand `device` whatever a compromised relay chose to append, bypassing the
/// relay's checks entirely.
fn deliver_unchecked(
    net: &mut SecureNet,
    h: &mut Harness,
    device: &str,
    env: SealedEnvelope,
) -> Vec<Rejection> {
    net.relay.inject_unchecked(env);
    let before = net.client(device).rejected.len();
    net.pull(h, device, B).unwrap();
    net.client(device).rejected[before..].to_vec()
}

#[test]
fn receivers_refuse_a_viewer_write_the_relay_let_through() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        (VIEWER, Role::View),
    ]);
    model::rename_category(h.device_mut(VIEWER), B, "cat-food", "Injected");
    let changes = h
        .device_mut(VIEWER)
        .doc_mut(&model::budget_doc(B))
        .get_changes(&[])
        .into_iter()
        .filter(|c| c.actor_id() == h.device(VIEWER).actor())
        .collect::<Vec<_>>();
    // The viewer holds the doc key (it has to, to read), so it CAN produce a
    // perfectly encrypted, correctly signed envelope. Only the role stops it.
    let env = net.seal(
        VIEWER,
        B,
        &model::budget_doc(B),
        Kind::Changes,
        &local_first_automerge_spike::secure::encode_changes(&changes),
    );
    assert_eq!(
        deliver_unchecked(&mut net, &mut h, OWNER, env),
        vec![Rejection::NotAWriter]
    );
    assert_eq!(cat_names(&h, OWNER), vec!["Groceries"]);
}

#[test]
fn receivers_refuse_a_writer_forging_another_members_changes() {
    let (mut h, mut net) = shared(&[(OWNER, Role::Owner), (EDITOR, Role::Edit)]);
    // The editor makes a change AUTHORED AS the owner's actor, and signs it
    // with its own (valid, writer) key.
    let owner_actor = h.device(OWNER).actor().clone();
    let doc = h.device_mut(EDITOR).doc_mut(&model::budget_doc(B));
    let before = doc.get_heads();
    let mut forged = doc.fork().with_actor(owner_actor);
    forged
        .put(model::map(&forged, "meta"), "name", "Owner renamed this")
        .unwrap();
    let changes = forged.get_changes(&before);
    let env = net.seal(
        EDITOR,
        B,
        &model::budget_doc(B),
        Kind::Changes,
        &local_first_automerge_spike::secure::encode_changes(&changes),
    );
    assert_eq!(
        deliver_unchecked(&mut net, &mut h, OWNER, env),
        vec![Rejection::ActorMismatch]
    );
}

#[test]
fn receivers_refuse_tampered_and_misdirected_envelopes() {
    let (mut h, mut net) = shared(&[(OWNER, Role::Owner), (EDITOR, Role::Edit)]);
    add(&mut h, EDITOR, "t2", 1_00);
    let doc_id = model::txns_doc(B, date(2026, 3, 1));
    let changes: Vec<_> = h
        .device_mut(EDITOR)
        .doc_mut(&doc_id)
        .get_changes(&[])
        .into_iter()
        .filter(|c| c.actor_id() == h.device(EDITOR).actor())
        .collect();
    let plain = local_first_automerge_spike::secure::encode_changes(&changes);

    let mut tampered = net.seal(EDITOR, B, &doc_id, Kind::Changes, &plain);
    tampered.ciphertext[0] ^= 1;
    assert_eq!(
        deliver_unchecked(&mut net, &mut h, OWNER, tampered),
        vec![Rejection::BadSignature]
    );

    // A relay can't re-point a valid envelope at another doc: the doc id is
    // signed and is AAD.
    let mut moved = net.seal(EDITOR, B, &doc_id, Kind::Changes, &plain);
    moved.doc_id = model::txns_doc(B, date(2026, 4, 1));
    assert_eq!(
        deliver_unchecked(&mut net, &mut h, OWNER, moved),
        vec![Rejection::BadSignature]
    );

    // A writer in budget B can't write into another budget's doc.
    let foreign = net.seal(EDITOR, B, "budget/someone-else", Kind::Changes, &plain);
    assert_eq!(
        deliver_unchecked(&mut net, &mut h, OWNER, foreign),
        vec![Rejection::DocNotInGroup]
    );

    assert_eq!(txn_ids(&h, OWNER), vec!["t1"]);
}

#[test]
fn only_the_pinned_owner_can_change_the_roster() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        (VIEWER, Role::View),
    ]);
    // The editor signs a roster that promotes the viewer and names itself owner.
    let cards = vec![
        (net.card(&h, EDITOR), Role::Owner),
        (net.card(&h, VIEWER), Role::Edit),
    ];
    let forged = Roster::build(
        B,
        2,
        1,
        net.client(EDITOR).identity.public(),
        &cards,
        &[9; 32],
    )
    .sign(&net.client(EDITOR).identity);
    assert_eq!(
        net.relay.update_roster(forged.clone()),
        Err(Rejection::NotOwner)
    );
    // And a client handed it directly by a compromised relay refuses it too.
    let fetched = Fetched {
        rosters: vec![forged],
        envelopes: vec![],
    };
    assert_eq!(
        net.receive(&mut h, OWNER, B, fetched),
        Err(Rejection::NotOwner)
    );
}

// --- removal and re-keying -------------------------------------------------------------------

#[test]
fn a_removed_member_gets_nothing_after_the_epoch_change() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        ("c", Role::Edit),
    ]);
    add(&mut h, "c", "t-c", 5_00);
    net.sync(&mut h, B);

    let cost = net
        .set_members(
            &mut h,
            OWNER,
            B,
            &[(OWNER, Role::Owner), (EDITOR, Role::Edit)],
        )
        .unwrap()
        .expect("removal rotates the key");
    assert!(cost.docs >= 2 && cost.snapshot_bytes > 0);

    add(&mut h, OWNER, "t-after", 7_00);
    net.sync(&mut h, B);
    assert_eq!(txn_ids(&h, EDITOR), vec!["t-after", "t-c", "t1"]);
    assert_same(&mut h, &[OWNER, EDITOR]);

    // An honest relay refuses c outright.
    assert_eq!(net.pull(&mut h, "c", B), Err(Rejection::NotAMember));
    add(&mut h, "c", "t-c2", 1_00);
    assert_eq!(net.push(&mut h, "c", B), Err(Rejection::NotAMember));

    // A dishonest relay can hand c everything; c can verify the new roster
    // (it's owner-signed) but has no key for the new epoch.
    let leaked = Fetched {
        rosters: net.relay.leak_rosters(B),
        envelopes: net
            .relay
            .leak_all(B)
            .into_iter()
            .enumerate()
            .map(|(i, e)| (1000 + i as u64, e))
            .collect(),
    };
    assert_eq!(net.receive(&mut h, "c", B, leaked), Ok(0));
    assert!(net
        .client("c")
        .rejected
        .iter()
        .all(|r| r == &Rejection::BadCiphertext));
    assert!(!txn_ids(&h, "c").contains(&"t-after".to_string()));

    // Recorded honestly: c keeps everything it synced before removal.
    assert!(txn_ids(&h, "c").contains(&"t1".to_string()));

    // The relay's log holds only new-epoch envelopes (old blobs compacted away).
    assert!(net.relay.leak_all(B).iter().all(|e| e.epoch == 2));
}

#[test]
fn an_offline_editor_reseals_its_pending_writes_after_a_rotation() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        ("c", Role::Edit),
    ]);
    h.go_offline(EDITOR);
    add(&mut h, EDITOR, "t-offline", 3_00);

    net.set_members(
        &mut h,
        OWNER,
        B,
        &[(OWNER, Role::Owner), (EDITOR, Role::Edit)],
    )
    .unwrap();

    h.go_online(EDITOR);
    // First publish is sealed under the old roster, refused as stale; the
    // client refreshes, unwraps the new key, reseals and succeeds.
    assert_eq!(net.push(&mut h, EDITOR, B), Ok(1));
    assert!(net
        .relay
        .rejected
        .iter()
        .any(|r| matches!(r, Rejection::StaleRoster { got: 1, current: 2 })));
    net.sync(&mut h, B);
    assert_eq!(txn_ids(&h, OWNER), vec!["t-offline", "t1"]);
    assert_same(&mut h, &[OWNER, EDITOR]);
}

#[test]
fn demotion_blocks_new_writes_but_old_ones_still_verify_for_a_new_device() {
    let (mut h, mut net) = shared(&[(OWNER, Role::Owner), (EDITOR, Role::Edit)]);
    add(&mut h, EDITOR, "t-before", 2_00);
    net.sync(&mut h, B);

    // Demote to view: same epoch, no rotation (nothing to hide from them).
    assert!(net
        .set_members(
            &mut h,
            OWNER,
            B,
            &[(OWNER, Role::Owner), (EDITOR, Role::View)]
        )
        .unwrap()
        .is_none());
    add(&mut h, EDITOR, "t-after", 9_00);
    assert_eq!(net.push(&mut h, EDITOR, B), Err(Rejection::NotAWriter));

    // A device joining now replays the log: the editor's earlier envelope was
    // sealed under roster v1, where it was a writer, so it verifies.
    h.add_device("d", SimClock::ymd(2026, 3, 10));
    net.enroll("d");
    net.set_members(
        &mut h,
        OWNER,
        B,
        &[
            (OWNER, Role::Owner),
            (EDITOR, Role::View),
            ("d", Role::View),
        ],
    )
    .unwrap();
    net.pull(&mut h, "d", B).unwrap();
    assert!(net.client("d").rejected.is_empty());
    assert_eq!(txn_ids(&h, "d"), vec!["t-before", "t1"]);
    assert_same(&mut h, &[OWNER, "d"]);
}

// --- measurement -----------------------------------------------------------------------------

/// Re-key cost on a 5-year household. Run in release:
/// `cargo test --release --test week2a -- --ignored --nocapture`
#[test]
#[ignore]
fn rotation_cost_on_a_five_year_household() {
    let (mut h, mut net) = shared(&[
        (OWNER, Role::Owner),
        (EDITOR, Role::Edit),
        ("c", Role::Edit),
    ]);
    let started = std::time::Instant::now();
    for m in 0..60u32 {
        let (y, mo) = (2021 + (m / 12) as i32, m % 12 + 1);
        for i in 0..150u32 {
            model::add_txn(
                h.device_mut(OWNER),
                B,
                &NewTxn {
                    id: &format!("t-{m}-{i}"),
                    date: date(y, mo, 1 + i % 28),
                    amount: 1_00 + i as i64,
                    description: "generated",
                    category: Some("cat-food"),
                },
            )
            .unwrap();
        }
    }
    let generated = started.elapsed();
    let started = std::time::Instant::now();
    net.sync(&mut h, B);
    let initial_sync = started.elapsed();

    let cost = net
        .set_members(
            &mut h,
            OWNER,
            B,
            &[(OWNER, Role::Owner), (EDITOR, Role::Edit)],
        )
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    net.pull(&mut h, EDITOR, B).unwrap();
    let member_apply = started.elapsed();

    println!(
        "9000 txns / {} docs: generate {generated:?}, initial secure sync {initial_sync:?}\n\
         rotation (owner: save+seal+publish) {:?}, snapshot {} KiB\n\
         member apply of snapshots {member_apply:?}; relay now stores {} KiB\n\
         wrapping the key for 10 members: {:?}",
        cost.docs,
        cost.elapsed,
        cost.snapshot_bytes / 1024,
        net.relay.stored_bytes(B) / 1024,
        SecureNet::wrap_cost(10),
    );
    assert_same(&mut h, &[OWNER, EDITOR]);
}
