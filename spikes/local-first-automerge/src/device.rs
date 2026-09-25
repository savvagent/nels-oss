//! One simulated device: a set of Automerge document replicas plus the
//! per-peer sync state for each.

use std::collections::{BTreeMap, HashMap};

use automerge::sync::State as SyncState;
use automerge::transaction::CommitOptions;
use automerge::{ActorId, AutoCommit, ChangeHash};

use crate::clock::SimClock;

pub type DeviceId = String;
pub type DocId = String;

pub struct Replica {
    pub doc: AutoCommit,
    /// Sync state per peer. Automerge's protocol is pairwise, so each device
    /// tracks what it believes every other device has for this document.
    pub(crate) peers: HashMap<DeviceId, SyncState>,
}

pub struct Device {
    pub id: DeviceId,
    pub clock: SimClock,
    pub(crate) online: bool,
    actor: ActorId,
    pub(crate) replicas: BTreeMap<DocId, Replica>,
}

impl Device {
    pub fn new(id: &str, clock: SimClock) -> Self {
        // A deterministic actor per device keeps runs reproducible: the same
        // scenario resolves conflicts the same way every time it runs.
        let actor = ActorId::from(
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, id.as_bytes())
                .as_bytes()
                .to_vec(),
        );
        Self {
            id: id.to_string(),
            clock,
            online: true,
            actor,
            replicas: BTreeMap::new(),
        }
    }

    /// The Automerge actor this device writes as. The secure relay binds it
    /// to the device's signing key in the roster.
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    pub fn is_online(&self) -> bool {
        self.online
    }

    pub fn has_doc(&self, doc_id: &str) -> bool {
        self.replicas.contains_key(doc_id)
    }

    /// Join a document, starting from an empty replica. The relay only
    /// forwards a document to devices that have joined it, which is what keeps
    /// `private/<user_id>` documents off other members' devices.
    pub fn join(&mut self, doc_id: &str) {
        if !self.replicas.contains_key(doc_id) {
            let mut doc = AutoCommit::new();
            doc.set_actor(self.actor.clone());
            self.replicas.insert(
                doc_id.to_string(),
                Replica {
                    doc,
                    peers: HashMap::new(),
                },
            );
        }
    }

    /// Join a document whose first change is a fixed GENESIS change: same
    /// actor, time and message on every device, so its hash is identical
    /// everywhere and it dedupes on sync. Any container created here is ONE
    /// shared object on every device. Without this, two offline devices that
    /// each create next month's transaction doc would each create their own
    /// `transactions` map and one side's writes would be hidden after merge
    /// (see `tests/automerge_semantics.rs`).
    pub fn join_with_genesis(&mut self, doc_id: &str, genesis: impl FnOnce(&mut AutoCommit)) {
        if !self.replicas.contains_key(doc_id) {
            let mut doc = genesis_doc(genesis);
            doc.set_actor(self.actor.clone());
            self.replicas.insert(
                doc_id.to_string(),
                Replica {
                    doc,
                    peers: HashMap::new(),
                },
            );
        }
    }

    /// Every joined document whose id starts with `prefix`, in id order.
    pub fn docs_with_prefix(&self, prefix: &str) -> impl Iterator<Item = (&DocId, &AutoCommit)> {
        let prefix = prefix.to_string();
        self.replicas
            .range(prefix.clone()..)
            .take_while(move |(id, _)| id.starts_with(&prefix))
            .map(|(id, r)| (id, &r.doc))
    }

    pub fn doc(&self, doc_id: &str) -> &AutoCommit {
        &self
            .replicas
            .get(doc_id)
            .unwrap_or_else(|| panic!("device {} has not joined {doc_id}", self.id))
            .doc
    }

    /// Mutable access for Automerge reads that need `&mut` (heads, change
    /// lookups). Writes should go through [`Device::change`] so they carry the
    /// simulated time.
    pub fn doc_mut(&mut self, doc_id: &str) -> &mut AutoCommit {
        let id = self.id.clone();
        &mut self
            .replicas
            .get_mut(doc_id)
            .unwrap_or_else(|| panic!("device {id} has not joined {doc_id}"))
            .doc
    }

    /// Apply edits to a document and commit them as one change, stamped with
    /// this device's simulated time. Joins the document if needed.
    pub fn change<R>(
        &mut self,
        doc_id: &str,
        message: &str,
        edit: impl FnOnce(&mut AutoCommit) -> R,
    ) -> R {
        self.join(doc_id);
        let time = self.clock.now().timestamp();
        let doc = &mut self.replicas.get_mut(doc_id).unwrap().doc;
        let out = edit(doc);
        doc.commit_with(
            CommitOptions::default()
                .with_message(message)
                .with_time(time),
        );
        out
    }

    pub fn heads(&mut self, doc_id: &str) -> Vec<ChangeHash> {
        let mut heads = self.doc_mut(doc_id).get_heads();
        heads.sort();
        heads
    }

    /// Sorted heads of every joined document whose id starts with `prefix`.
    pub fn heads_with_prefix(&mut self, prefix: &str) -> BTreeMap<DocId, Vec<ChangeHash>> {
        let ids: Vec<DocId> = self
            .docs_with_prefix(prefix)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .map(|id| {
                let heads = self.heads(&id);
                (id, heads)
            })
            .collect()
    }

    /// `save()` bytes of one replica: what a client writes to disk, and what
    /// an owner snapshot carries through the secure relay.
    pub fn save(&mut self, doc_id: &str) -> Vec<u8> {
        self.doc_mut(doc_id).save()
    }

    /// Load a replica from `save()` bytes, as a client does from disk on cold
    /// start. Replaces any replica already held under `doc_id`.
    pub fn load_saved(&mut self, doc_id: &str, bytes: &[u8]) {
        let mut doc = AutoCommit::load(bytes).expect("load saved doc");
        doc.set_actor(self.actor.clone());
        self.replicas.insert(
            doc_id.to_string(),
            Replica {
                doc,
                peers: HashMap::new(),
            },
        );
    }

    /// Swap a replica for a different document (a compacted one), writing as
    /// this device from here on. Sync state is dropped: it describes the old
    /// history.
    pub fn replace_doc(&mut self, doc_id: &str, mut doc: AutoCommit) {
        doc.set_actor(self.actor.clone());
        self.replicas.insert(
            doc_id.to_string(),
            Replica {
                doc,
                peers: HashMap::new(),
            },
        );
    }

    /// Forget in-flight sync bookkeeping for every peer while keeping the
    /// durable part (shared heads). This is what a real client does on
    /// reconnect: messages sent while the link was down were lost, and the
    /// in-memory `State` still believes they are in flight.
    pub(crate) fn reset_peer_states(&mut self) {
        for replica in self.replicas.values_mut() {
            for state in replica.peers.values_mut() {
                *state = SyncState::decode(&state.encode()).expect("round-trip sync state");
            }
        }
    }
}

/// A fresh document holding only the fixed GENESIS change (same actor, time
/// and message everywhere, so its hash is identical on every device). Still
/// writing as the genesis actor: set your own before editing.
pub fn genesis_doc(genesis: impl FnOnce(&mut AutoCommit)) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(b"nels-genesis".to_vec()));
    genesis(&mut doc);
    doc.commit_with(
        CommitOptions::default()
            .with_message("genesis")
            .with_time(0),
    );
    doc
}
