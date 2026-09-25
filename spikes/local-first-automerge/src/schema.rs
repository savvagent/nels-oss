//! Week 2d: version skew. Two household members on different app versions
//! share one budget.
//!
//! What's safe without any machinery (`tests/week2d.rs`):
//! - A NEW FIELD inside an existing object (a category's `color`): Automerge
//!   stores it, the old client never reads it, and the old client's
//!   field-level writes leave it alone.
//! - A NEW FLAT KEY in an existing map, or a new value in an existing column.
//!
//! What needs machinery:
//! 1. **A new container.** A doc type's genesis is frozen: two devices with
//!    different genesis versions each create every container, and one side's
//!    writes vanish (Automerge behavior 1, for the whole doc). A new
//!    container is instead created by a **schema upgrade change**: time 0,
//!    depending only on genesis, authored by an actor named after its own
//!    content (`nels-schema-v2-<hash of doc type, version, containers>`).
//!    Like genesis it is byte-identical wherever it's built, so devices
//!    that upgrade independently create ONE container. The content-derived
//!    actor matters: Automerge refuses to sync a doc holding two DIFFERENT
//!    changes with the same actor and seq (`DuplicateSeqNumber`, permanent),
//!    so a name that could carry two contents is a way to break a doc for
//!    good. Unlike genesis it has to TRAVEL: a v2 device's later changes
//!    depend on it, and a v1 device that lacked it would queue every one of
//!    them forever. So it's published like a member's change, and receivers
//!    accept it on a structural check instead of the actor binding
//!    ([`check_upgrade`]).
//! 2. **A field that changes what existing numbers mean** (e.g. a split
//!    across categories: an old client summing `amount` would be wrong).
//!    The budget doc carries add-only `requires/<n>` markers in `meta`; a
//!    client older than the highest marker must not compute or write
//!    ([`check_client`]). Add-only keys, so two devices can't race it
//!    downward the way an LWW `min_version` value could.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, Change, ObjType, ReadDoc, Value, ROOT};
use sha2::{Digest, Sha256};

use crate::device::Device;
use crate::model::{self, map};

pub const SCHEMA_ACTOR_PREFIX: &str = "nels-schema-v";
const GENESIS_ACTOR: &[u8] = b"nels-genesis";

/// The version this spike's `model`/`read` code implements.
pub const V1: u32 = 1;

/// Containers each schema version adds, per doc type. v1 is genesis.
pub fn containers(doc_id: &str, version: u32) -> &'static [&'static str] {
    match (doc_id.split('/').next(), version) {
        // v2: per-transaction tags, `txn_id -> "a,b"`.
        (Some("txns"), 2) => &["tags"],
        // v2: saved views, a map of view id -> object (only its creator makes it).
        (Some("budget"), 2) => &["views"],
        _ => &[],
    }
}

/// `nels-schema-v<version>-<first 8 bytes of sha256(doc type, version, keys)>`.
pub fn schema_actor(doc_id: &str, version: u32, keys: &[&str]) -> ActorId {
    let mut h = Sha256::new();
    h.update(doc_type(doc_id).as_bytes());
    h.update(version.to_be_bytes());
    for k in keys {
        h.update((k.len() as u32).to_be_bytes());
        h.update(k.as_bytes());
    }
    let digest: String = h.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    ActorId::from(format!("{SCHEMA_ACTOR_PREFIX}{version}-{digest}").into_bytes())
}

fn doc_type(doc_id: &str) -> &str {
    doc_id.split('/').next().unwrap_or("")
}

fn version_of(actor: &ActorId) -> Option<u32> {
    let s = std::str::from_utf8(actor.to_bytes()).ok()?;
    s.strip_prefix(SCHEMA_ACTOR_PREFIX)?
        .split('-')
        .next()?
        .parse()
        .ok()
}

pub fn is_schema_actor(actor: &ActorId) -> bool {
    actor.to_bytes().starts_with(SCHEMA_ACTOR_PREFIX.as_bytes())
}

/// The upgrade change for `version` of `doc_id`'s doc type.
pub fn upgrade_change(doc_id: &str, version: u32) -> Change {
    canonical_upgrade(doc_id, version, containers(doc_id, version))
}

/// The ONE change that may create `keys` for `version`: deps are genesis
/// only (upgrades are siblings, not a chain, so nothing else can vary), keys
/// are created in sorted order, and actor, seq, time, message and ops all
/// follow from the arguments. Same arguments, same hash, on every device.
pub fn canonical_upgrade(doc_id: &str, version: u32, keys: &[&str]) -> Change {
    let mut keys = keys.to_vec();
    keys.sort_unstable();
    let keys = &keys[..];
    let mut d = model::genesis_for(doc_id);
    d.set_actor(schema_actor(doc_id, version, keys));
    for name in keys {
        d.put_object(ROOT, *name, ObjType::Map).unwrap();
    }
    d.commit_with(
        CommitOptions::default()
            .with_message(format!("schema v{version}"))
            .with_time(0),
    );
    d.get_last_local_change().unwrap()
}

/// Bring `doc_id` on `dev` up to `version`. Idempotent, and safe to run
/// concurrently on devices that can't see each other.
pub fn upgrade(dev: &mut Device, doc_id: &str, version: u32) {
    let changes: Vec<Change> = (2..=version)
        .filter(|v| !containers(doc_id, *v).is_empty())
        .map(|v| upgrade_change(doc_id, v))
        .collect();
    dev.doc_mut(doc_id).apply_changes(changes).unwrap();
}

/// Receiver-side check for a change by a schema actor, which carries no
/// signer binding. A v1 receiver can't know what v2 is supposed to add, but
/// it can check the change is the CANONICAL one for what it does add: only
/// new, empty root maps, rebuilt byte for byte by [`canonical_upgrade`].
/// That refuses a "schema" change re-creating `date` (behavior 1: every row
/// hidden), and one squatting on a schema actor with other content (which
/// would make the genuine upgrade a `DuplicateSeqNumber` forever).
pub fn check_upgrade(doc: &mut AutoCommit, doc_id: &str, c: &Change) -> Result<(), &'static str> {
    if doc.get_change_by_hash(&c.hash()).is_some() {
        return Ok(()); // already have it: identical, nothing new
    }
    let version = version_of(c.actor_id()).ok_or("not a schema actor")?;
    let before: Vec<String> = doc.keys(ROOT).collect();
    let mut probe = doc.fork();
    probe.apply_changes([c.clone()]).map_err(|_| "malformed")?;
    let added: Vec<String> = probe.keys(ROOT).filter(|k| !before.contains(k)).collect();
    // Every op must be one of the new, empty root maps.
    if added.len() != c.len() {
        return Err("upgrade touches existing containers");
    }
    for k in &added {
        match probe.get(ROOT, k.as_str()).unwrap() {
            Some((Value::Object(ObjType::Map), id)) if probe.length(&id) == 0 => {}
            _ => return Err("upgrade writes something other than an empty map"),
        }
    }
    let keys: Vec<&str> = added.iter().map(String::as_str).collect();
    if canonical_upgrade(doc_id, version, &keys).hash() != c.hash() {
        return Err("not the canonical upgrade for what it creates");
    }
    Ok(())
}

/// Schema-actor changes in `doc` (upgrades it holds), oldest first.
pub fn upgrades_in(doc: &mut AutoCommit) -> Vec<Change> {
    doc.get_changes(&[])
        .into_iter()
        .filter(|c| is_schema_actor(c.actor_id()))
        .collect()
}

pub fn is_structural(actor: &ActorId) -> bool {
    actor.to_bytes() == GENESIS_ACTOR || is_schema_actor(actor)
}

// --- the minimum-client gate ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeRequired {
    pub required: u32,
    pub client: u32,
}

/// The highest `requires/<n>` marker in the budget's `meta` (1 if none).
pub fn required_version(budget: &AutoCommit) -> u32 {
    budget
        .keys(map(budget, "meta"))
        .filter_map(|k| k.strip_prefix("requires/")?.parse().ok())
        .max()
        .unwrap_or(1)
}

/// Called by a v`n` client the first time it writes something older clients
/// would misread. Add-only: a lower marker can't hide a higher one.
pub fn require(dev: &mut Device, budget_id: &str, version: u32) {
    let doc_id = model::budget_doc(budget_id);
    dev.change(&doc_id, "require client version", |d| {
        d.put(map(d, "meta"), format!("requires/{version}"), true)
            .unwrap();
    });
}

/// Gate every read that computes money and every write. An older client
/// shows the budget as "update Nels to see this budget" instead.
pub fn check_client(budget: &AutoCommit, client: u32) -> Result<(), UpgradeRequired> {
    let required = required_version(budget);
    if client >= required {
        Ok(())
    } else {
        Err(UpgradeRequired { required, client })
    }
}
