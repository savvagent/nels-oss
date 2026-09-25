//! Week 2c: erasure.
//!
//! An Automerge doc keeps every change forever, and Nels's deletes are
//! tombstones (`model` rule 4), so a deleted transaction is still there
//! twice over: in the current state (the tombstone sits beside the row's
//! facts) and in every earlier state. Nothing short of a new document
//! removes it.
//!
//! **Compaction** is that new document: the doc's genesis plus ONE change
//! that writes the current state, built by the owner.
//! - Deleted rows keep only their `deleted` marker. The marker has to stay,
//!   or a device that still has the row (or a bank re-import) would bring it
//!   back as live.
//! - Conflicts are resolved as they read today: the winning value is kept
//!   and the losers are dropped, and `revision` keeps its highest `seq`. So
//!   Week 1's conflict flags (`amount_edit_conflict`,
//!   `edited_concurrently_with_delete`) don't survive compaction. Compaction
//!   is where a pending conflict gets settled.
//! - Anything that names old change hashes (`close_seen`) is rewritten,
//!   because those hashes no longer exist (see [`encode_close_seen`]).
//!
//! Every device swaps its replica for the compacted doc. Its own edits that
//! it hadn't published yet can't be re-applied as changes: an Automerge
//! change depends on its history by hash. So they are **rebased as values**:
//! [`unpublished_patches`] turns them into state patches and [`replay`]
//! writes them onto the new doc as a fresh change. A rebased write to a row
//! the new doc has erased is dropped: erasure beats an edit it was
//! concurrent with.

use std::collections::BTreeSet;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{
    ActorId, AutoCommit, ChangeHash, ObjId, ObjType, Patch, PatchAction, Prop, ReadDoc,
    ScalarValue, Value, ROOT,
};

use crate::model::{self, map, TXN_COLUMNS};
use crate::schema;

/// Compact `old` into a fresh document: genesis plus one change by `actor`
/// holding the current state, with deleted rows reduced to their tombstone.
/// `extra` runs inside that same change, for values that must be rewritten
/// rather than copied (`close_seen`).
pub fn compact(
    old: &mut AutoCommit,
    doc_id: &str,
    actor: ActorId,
    time: i64,
    extra: impl FnOnce(&mut AutoCommit),
) -> AutoCommit {
    let mut new = model::genesis_for(doc_id);
    // Upgrades first (2d), so containers a newer client added exist under
    // the same ids, and a newer device can keep writing into them.
    new.apply_changes(schema::upgrades_in(old)).unwrap();
    new.set_actor(actor);
    if doc_id.starts_with("txns/") {
        copy_txns(old, &mut new);
    } else {
        copy_map(old, &ROOT, &mut new, &ROOT);
    }
    extra(&mut new);
    new.commit_with(
        CommitOptions::default()
            .with_message("compact")
            .with_time(time),
    );
    new
}

fn copy_txns(old: &AutoCommit, new: &mut AutoCommit) {
    let deleted: BTreeSet<String> = old.keys(map(old, "deleted")).collect();
    // EVERY root container, not just the columns this version knows (2d):
    // a v1 compactor must not erase a v2 column. It can still strip erased
    // rows from it, because every transactions column is keyed by txn id.
    let columns: Vec<String> = old.keys(ROOT).collect();
    debug_assert!(TXN_COLUMNS.iter().all(|c| columns.iter().any(|k| k == c)));
    for col in columns.iter().map(String::as_str) {
        let (from, to) = (map(old, col), map(new, col));
        // One pass per column (`map_range`), not a `get` per key: same
        // reason as `read::ledger`.
        let entries: Vec<(String, ScalarValue, bool)> = old
            .map_range(&from, ..)
            .filter_map(|item| match Value::from(item.value) {
                Value::Scalar(v) => Some((item.key.into_owned(), v.into_owned(), item.conflict)),
                Value::Object(_) => None,
            })
            .collect();
        for (key, value, conflict) in entries {
            if col != "deleted" && deleted.contains(&key) {
                continue;
            }
            let value = if col == "revision" && conflict {
                // Keep the value reads use, not Automerge's LWW winner.
                model::newest_revision(old, &key).unwrap().encode().into()
            } else {
                value
            };
            new.put(&to, key.as_str(), value).unwrap();
        }
    }
}

/// Recursive copy of a map's winning values. Genesis containers already
/// exist in `new` and are reused, so they keep their genesis object ids.
fn copy_map(old: &AutoCommit, from: &ObjId, new: &mut AutoCommit, to: &ObjId) {
    for key in old.keys(from).collect::<Vec<_>>() {
        match old.get(from, &key).unwrap().unwrap() {
            (Value::Object(ObjType::Map), child) => {
                let target = match new.get(to, &key).unwrap() {
                    Some((Value::Object(_), id)) => id,
                    _ => new.put_object(to, key.as_str(), ObjType::Map).unwrap(),
                };
                copy_map(old, &child, new, &target);
            }
            (Value::Scalar(s), _) => new.put(to, key.as_str(), s.into_owned()).unwrap(),
            (Value::Object(t), _) => panic!("compaction only handles maps, found {t:?}"),
        }
    }
}

/// This device's own edits in `old` that it has not published, as state
/// patches. `published` is the heads it last published at. Only its own
/// changes count: everything else it holds came through the relay, so the
/// owner had it when compacting.
pub fn unpublished_patches(
    old: &mut AutoCommit,
    published: &[ChangeHash],
    actor: &ActorId,
) -> Vec<Patch> {
    let mine: BTreeSet<ChangeHash> = old
        .get_changes(published)
        .iter()
        .filter(|c| c.actor_id() == actor)
        .map(|c| c.hash())
        .collect();
    if mine.is_empty() {
        return Vec::new();
    }
    // Heads of everything BUT those changes. That set is causally closed:
    // nobody else can have built on a change that was never published.
    let rest: Vec<_> = old
        .get_changes(&[])
        .into_iter()
        .filter(|c| !mine.contains(&c.hash()))
        .collect();
    let depended: BTreeSet<ChangeHash> = rest.iter().flat_map(|c| c.deps().to_vec()).collect();
    assert!(
        depended.is_disjoint(&mine),
        "a change by another actor depends on an unpublished change"
    );
    let base: Vec<ChangeHash> = rest
        .iter()
        .map(|c| c.hash())
        .filter(|h| !depended.contains(h))
        .collect();
    let now = old.get_heads();
    old.diff(&base, &now)
}

/// Write rebased patches onto a compacted doc (inside the caller's change).
/// Objects are found by key path, not by id: a compacted doc re-creates
/// every non-genesis object under a new id. Returns how many patches were
/// DROPPED: writes to an erased row, stale provider revisions, and writes
/// into objects that no longer exist.
pub fn replay(doc: &mut AutoCommit, doc_id: &str, patches: &[Patch]) -> usize {
    let is_txns = doc_id.starts_with("txns/");
    let mut dropped = 0;
    for p in patches {
        let Some(obj) = resolve(doc, &p.path) else {
            dropped += 1;
            continue;
        };
        let column = p.path.last().map(|(_, prop)| prop_str(prop));
        match &p.action {
            PatchAction::PutMap { key, value, .. } => {
                if is_txns {
                    let col = column.as_deref().unwrap_or("");
                    let erased = doc.get(map(doc, "deleted"), key).unwrap().is_some();
                    if col != "deleted" && erased {
                        dropped += 1;
                        continue;
                    }
                    if col == "revision" {
                        let incoming = value.0.as_str().and_then(model::Revision::decode);
                        let held = model::newest_revision(doc, key);
                        if let (Some(i), Some(h)) = (incoming, held) {
                            if h.seq >= i.seq {
                                dropped += 1;
                                continue;
                            }
                        }
                    }
                }
                match &value.0 {
                    Value::Object(t) => {
                        if doc.get(&obj, key).unwrap().is_none() {
                            doc.put_object(&obj, key.as_str(), *t).unwrap();
                        }
                    }
                    Value::Scalar(s) => doc.put(&obj, key.as_str(), s.as_ref().clone()).unwrap(),
                }
            }
            PatchAction::DeleteMap { key } => {
                if doc.get(&obj, key).unwrap().is_some() {
                    doc.delete(&obj, key.as_str()).unwrap();
                }
            }
            other => panic!("unexpected patch in a map-only doc: {other:?}"),
        }
    }
    dropped
}

fn prop_str(p: &Prop) -> String {
    match p {
        Prop::Map(k) => k.clone(),
        Prop::Seq(i) => i.to_string(),
    }
}

fn resolve(doc: &AutoCommit, path: &[(ObjId, Prop)]) -> Option<ObjId> {
    let mut obj = ROOT;
    for (_, prop) in path {
        obj = match doc.get(&obj, prop.clone()).unwrap()? {
            (Value::Object(_), id) => id,
            _ => return None,
        };
    }
    Some(obj)
}

/// Does any change in `doc`'s history, not just its current state, contain
/// `needle`? This is the erasure check: what a device could recover with
/// `fork_at` on the doc it holds.
pub fn history_mentions(doc: &mut AutoCommit, needle: &str) -> bool {
    doc.get_changes(&[])
        .iter()
        .any(|c| format!("{:?}", c.decode()).contains(needle))
}

/// `close_seen` value for a compacted transactions doc: the new doc's heads,
/// plus the ids the closer's heads classified as changed after the close.
/// Those old heads don't exist in the new doc, so the answer they gave has to
/// be written down: `"<heads>;late=<id>,<id>"`.
pub fn encode_close_seen(heads: &[ChangeHash], late: &[String]) -> String {
    let heads = heads
        .iter()
        .map(ChangeHash::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if late.is_empty() {
        heads
    } else {
        format!("{heads};late={}", late.join(","))
    }
}

/// Inverse of [`encode_close_seen`]; a value with no `;late=` is the
/// uncompacted form `model::close_budget` writes.
pub fn decode_close_seen(value: &str) -> (Vec<ChangeHash>, Vec<String>) {
    let (heads, late) = value.split_once(";late=").unwrap_or((value, ""));
    (
        heads
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|h| h.parse().unwrap())
            .collect(),
        late.split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}
