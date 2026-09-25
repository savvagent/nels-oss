//! Week 2a: an end-to-end encrypted relay that enforces roles without reading
//! plaintext.
//!
//! **Why this is not the Week 1 transport with crypto bolted on.** Week 1
//! syncs with Automerge's own protocol, pairwise, through [`crate::Relay`].
//! A blind relay can't enforce roles on that protocol, for two reasons
//! pinned in `tests/week2a.rs`:
//! 1. A read-only member still has to SEND sync messages (heads, bloom
//!    filter) to receive anything, so "drop everything a viewer sends" also
//!    stops the viewer from reading.
//! 2. A sync message carries every change the recipient lacks, including
//!    changes authored by third parties. The device that signs the message
//!    is not the author of what's inside it, so a signature on the message
//!    says nothing about who wrote each change.
//!
//! So the relay here is an append-only **log of sealed change batches**. Each
//! batch holds only changes its signer authored (checked on receive against
//! the actor bound to the signer's key in the roster). The relay stores and
//! fans out the original envelopes and never re-signs anything. Every
//! envelope is a write, so "reject writes from viewers" is exact.
//!
//! Trust model:
//! - **Owner key** is the root. It signs the [`Roster`]: members, roles, each
//!   member's Automerge actor, and the current document key wrapped for each
//!   member. Clients pin the owner key at invite time.
//! - **Relay** sees headers (group, doc id, signer, roster version, epoch)
//!   and ciphertext. It checks signatures and roles and drops what fails, as
//!   a bandwidth and first-line guard. It is NOT trusted for integrity or
//!   confidentiality: clients re-check everything on receive.
//! - **Known gap, recorded in findings:** the relay IS trusted for
//!   freshness. Nothing stops a malicious relay from accepting an envelope
//!   that a demoted or removed member back-dates to an old roster version.
//!   Honest relays refuse it (`StaleRoster`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, Change, ChangeHash, ReadDoc};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::device::DeviceId;
use crate::erasure;
use crate::harness::Harness;
use crate::model;
use crate::relay::RelayStats;
use crate::schema;

pub type PubKey = [u8; 32];
pub type DocKey = [u8; 32];

/// The actor every genesis change is authored by (see
/// [`crate::Device::join_with_genesis`]). Genesis is recreated locally by
/// every device and never sent, so it may appear in snapshots only.
const GENESIS_ACTOR: &[u8] = b"nels-genesis";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    UnknownGroup,
    /// Signer (or fetcher) is not in the current roster.
    NotAMember,
    /// Signer is a `view` member and the envelope is a write.
    NotAWriter,
    /// Only the owner may publish a roster or a snapshot.
    NotOwner,
    /// Envelope was sealed under an older roster; refresh and reseal.
    StaleRoster {
        got: u64,
        current: u64,
    },
    /// Envelope's key epoch doesn't match its roster version.
    WrongEpoch,
    BadSignature,
    /// AEAD failed, or no key for that epoch.
    BadCiphertext,
    /// A change inside the batch was authored by an actor other than the
    /// signer's. Blocks one writer forging changes as another.
    ActorMismatch,
    /// Roster update isn't the next version, or changes the pinned owner.
    RosterOutOfOrder,
    /// Envelope names a doc outside its group, e.g. a writer in budget A
    /// trying to write budget B's doc.
    DocNotInGroup,
    /// Plaintext wasn't a well-formed change batch or snapshot.
    Malformed,
    /// Changes (or a snapshot) for a doc that has since been compacted,
    /// sealed before the compaction. They belong to the erased history.
    PreCompaction,
    /// A schema-actor change that does more than create new, empty root
    /// containers (2d). It carries no signer binding, so it is checked
    /// structurally instead.
    BadUpgrade,
    /// A compaction whose base is out of date: something was published for
    /// one of its docs after the owner last fetched. Retrying after a fetch
    /// includes it; accepting would silently drop it.
    CompactionRaced,
}

// --- identities ------------------------------------------------------------

/// One device's keys: ed25519 for signing, X25519 for receiving wrapped
/// document keys. Per DEVICE, not per member, because two devices of one
/// member must be distinct Automerge actors anyway. (Production would have a
/// member key certify device keys; noted in findings.)
pub struct Identity {
    signing: SigningKey,
    x_secret: [u8; 32],
}

impl Identity {
    /// Deterministic from a label so test runs are reproducible. Real keys
    /// come from the OS RNG.
    pub fn from_label(label: &str) -> Self {
        let seed = |purpose: &str| -> [u8; 32] {
            Sha256::digest(format!("nels-spike/{purpose}/{label}")).into()
        };
        Self {
            signing: SigningKey::from_bytes(&seed("ed25519")),
            x_secret: seed("x25519"),
        }
    }

    pub fn public(&self) -> PubKey {
        self.signing.verifying_key().to_bytes()
    }

    pub fn x_public(&self) -> [u8; 32] {
        MontgomeryPoint::mul_base_clamped(self.x_secret).to_bytes()
    }

    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

fn verify(signer: &PubKey, msg: &[u8], sig: &[u8; 64]) -> Result<(), Rejection> {
    let key = VerifyingKey::from_bytes(signer).map_err(|_| Rejection::BadSignature)?;
    key.verify(msg, &Signature::from_bytes(sig))
        .map_err(|_| Rejection::BadSignature)
}

// --- key wrapping (sealed box) ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedKey {
    ephemeral: [u8; 32],
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

fn wrap_kek(shared: [u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> XChaCha20Poly1305 {
    let mut salt = ephemeral.to_vec();
    salt.extend_from_slice(recipient);
    let mut kek = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), &shared)
        .expand(b"nels-spike/wrap-doc-key", &mut kek)
        .unwrap();
    XChaCha20Poly1305::new(&kek.into())
}

/// Encrypt `key` to `recipient_x`. `context` (group + epoch) is bound as AAD
/// so a wrapped key can't be replayed into another group or epoch.
fn seal_key(key: &DocKey, recipient_x: &[u8; 32], context: &[u8]) -> SealedKey {
    let mut eph = [0u8; 32];
    OsRng.fill_bytes(&mut eph);
    let ephemeral = MontgomeryPoint::mul_base_clamped(eph).to_bytes();
    let shared = MontgomeryPoint(*recipient_x).mul_clamped(eph).to_bytes();
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = wrap_kek(shared, &ephemeral, recipient_x)
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: key,
                aad: context,
            },
        )
        .unwrap();
    SealedKey {
        ephemeral,
        nonce,
        ciphertext,
    }
}

fn open_key(sealed: &SealedKey, me: &Identity, context: &[u8]) -> Option<DocKey> {
    let shared = MontgomeryPoint(sealed.ephemeral)
        .mul_clamped(me.x_secret)
        .to_bytes();
    let plain = wrap_kek(shared, &sealed.ephemeral, &me.x_public())
        .decrypt(
            XNonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.ciphertext,
                aad: context,
            },
        )
        .ok()?;
    plain.try_into().ok()
}

// --- roster ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Edit,
    View,
}

impl Role {
    pub fn can_write(self) -> bool {
        !matches!(self, Role::View)
    }
    fn tag(self) -> u8 {
        self as u8
    }
}

/// What a device hands the owner to be invited: its public keys and actor.
#[derive(Debug, Clone)]
pub struct MemberCard {
    pub public: PubKey,
    pub x_public: [u8; 32],
    pub actor: ActorId,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub role: Role,
    pub actor: ActorId,
    pub x_public: [u8; 32],
    pub wrapped_key: SealedKey,
}

/// Who may do what in a sharing group (one budget: its budget doc plus all of
/// its month docs), and the current document key for each of them.
///
/// This lives OUTSIDE the CRDT on purpose. Week 1 stored `members` as a map in
/// the budget doc, where any writer (or a viewer's patched client) could
/// promote itself; a CRDT has no notion of who may write which key. Authority
/// has to be a separately signed object.
#[derive(Debug, Clone)]
pub struct Roster {
    pub group: String,
    pub version: u64,
    pub epoch: u64,
    pub owner: PubKey,
    pub members: BTreeMap<PubKey, Member>,
    /// Doc id -> the roster version its latest compaction was published
    /// under. Anything for that doc sealed under an earlier version is from
    /// the erased history (2c). Carried forward by every later roster.
    pub compacted: BTreeMap<String, u64>,
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn key_context(group: &str, epoch: u64) -> Vec<u8> {
    let mut c = Vec::new();
    put(&mut c, group.as_bytes());
    c.extend_from_slice(&epoch.to_be_bytes());
    c
}

impl Roster {
    fn encode(&self) -> Vec<u8> {
        let mut out = b"nels-spike/roster/v1".to_vec();
        put(&mut out, self.group.as_bytes());
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.owner);
        for (pk, m) in &self.members {
            out.extend_from_slice(pk);
            out.push(m.role.tag());
            put(&mut out, m.actor.to_bytes());
            out.extend_from_slice(&m.x_public);
            out.extend_from_slice(&m.wrapped_key.ephemeral);
            out.extend_from_slice(&m.wrapped_key.nonce);
            put(&mut out, &m.wrapped_key.ciphertext);
        }
        for (doc, v) in &self.compacted {
            put(&mut out, doc.as_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    pub fn role_of(&self, pk: &PubKey) -> Option<Role> {
        self.members.get(pk).map(|m| m.role)
    }

    /// Build a roster wrapping `key` for every member.
    pub fn build(
        group: &str,
        version: u64,
        epoch: u64,
        owner: PubKey,
        members: &[(MemberCard, Role)],
        key: &DocKey,
    ) -> Self {
        let ctx = key_context(group, epoch);
        Self {
            group: group.to_string(),
            version,
            epoch,
            owner,
            members: members
                .iter()
                .map(|(card, role)| {
                    (
                        card.public,
                        Member {
                            role: *role,
                            actor: card.actor.clone(),
                            x_public: card.x_public,
                            wrapped_key: seal_key(key, &card.x_public, &ctx),
                        },
                    )
                })
                .collect(),
            compacted: BTreeMap::new(),
        }
    }

    pub fn sign(self, owner: &Identity) -> SignedRoster {
        let signature = owner.sign(&self.encode());
        SignedRoster {
            roster: self,
            signature,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SignedRoster {
    pub roster: Roster,
    pub signature: [u8; 64],
}

impl SignedRoster {
    /// Valid iff signed by `pinned_owner`, who must also be the roster's
    /// declared owner and hold the `Owner` role in it.
    pub fn verify(&self, pinned_owner: &PubKey) -> Result<(), Rejection> {
        if &self.roster.owner != pinned_owner
            || self.roster.role_of(pinned_owner) != Some(Role::Owner)
        {
            return Err(Rejection::NotOwner);
        }
        verify(pinned_owner, &self.roster.encode(), &self.signature)
    }
}

// --- envelopes -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Changes authored by the signer, as raw change bytes.
    Changes,
    /// A whole document (`save()`), published by the owner at a key rotation.
    /// Merged into the receiver's copy, so it carries the full history.
    Snapshot,
    /// A compacted document (2c): genesis plus one owner change, no history.
    /// It REPLACES the receiver's copy. Only accepted through
    /// [`SecureRelay::compact`].
    Compacted,
}

/// What travels through the relay. Everything but `ciphertext` is plaintext
/// header the relay may read.
#[derive(Debug, Clone)]
pub struct SealedEnvelope {
    pub group: String,
    pub doc_id: String,
    pub signer: PubKey,
    pub roster_version: u64,
    pub epoch: u64,
    pub kind: Kind,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
    pub signature: [u8; 64],
}

impl SealedEnvelope {
    fn header(&self) -> Vec<u8> {
        let mut out = b"nels-spike/envelope/v1".to_vec();
        put(&mut out, self.group.as_bytes());
        put(&mut out, self.doc_id.as_bytes());
        out.extend_from_slice(&self.signer);
        out.extend_from_slice(&self.roster_version.to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.push(self.kind as u8);
        out
    }

    fn signed_bytes(&self) -> Vec<u8> {
        let mut out = self.header();
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    pub fn wire_len(&self) -> usize {
        self.header().len() + self.nonce.len() + self.ciphertext.len() + self.signature.len()
    }

    /// Encrypt `plaintext` under `key` and sign. The header is AAD, so the
    /// relay can't move a ciphertext to another doc, group or epoch.
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        me: &Identity,
        group: &str,
        doc_id: &str,
        roster_version: u64,
        epoch: u64,
        kind: Kind,
        key: &DocKey,
        plaintext: &[u8],
    ) -> Self {
        let mut env = Self {
            group: group.to_string(),
            doc_id: doc_id.to_string(),
            signer: me.public(),
            roster_version,
            epoch,
            kind,
            nonce: [0; 24],
            ciphertext: Vec::new(),
            signature: [0; 64],
        };
        OsRng.fill_bytes(&mut env.nonce);
        env.ciphertext = XChaCha20Poly1305::new(key.into())
            .encrypt(
                XNonce::from_slice(&env.nonce),
                Payload {
                    msg: plaintext,
                    aad: &env.header(),
                },
            )
            .unwrap();
        env.signature = me.sign(&env.signed_bytes());
        env
    }

    fn decrypt(&self, key: &DocKey) -> Result<Vec<u8>, Rejection> {
        XChaCha20Poly1305::new(key.into())
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.ciphertext,
                    aad: &self.header(),
                },
            )
            .map_err(|_| Rejection::BadCiphertext)
    }
}

/// Role and signature checks shared by the relay and every client. The
/// relay runs them against its current roster; a client runs them against
/// the roster version the envelope claims (verified from the owner's chain).
fn check_envelope(env: &SealedEnvelope, roster: &Roster) -> Result<(), Rejection> {
    if env.group != roster.group || !in_group(&env.doc_id, &env.group) {
        return Err(Rejection::DocNotInGroup);
    }
    let role = roster.role_of(&env.signer).ok_or(Rejection::NotAMember)?;
    match env.kind {
        Kind::Changes if !role.can_write() => return Err(Rejection::NotAWriter),
        Kind::Snapshot | Kind::Compacted if role != Role::Owner => return Err(Rejection::NotOwner),
        _ => {}
    }
    if env.epoch != roster.epoch {
        return Err(Rejection::WrongEpoch);
    }
    verify(&env.signer, &env.signed_bytes(), &env.signature)
}

pub fn encode_changes(changes: &[Change]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in changes {
        put(&mut out, c.raw_bytes());
    }
    out
}

fn decode_changes(mut bytes: &[u8]) -> Result<Vec<Change>, Rejection> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let len = bytes
            .get(..4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize)
            .ok_or(Rejection::Malformed)?;
        let chunk = bytes.get(4..4 + len).ok_or(Rejection::Malformed)?;
        out.push(Change::from_bytes(chunk.to_vec()).map_err(|_| Rejection::Malformed)?);
        bytes = &bytes[4 + len..];
    }
    Ok(out)
}

// --- relay -----------------------------------------------------------------

#[derive(Default)]
struct GroupLog {
    /// Every roster version, oldest first. Kept so a newly joining device can
    /// verify envelopes sealed under earlier versions.
    rosters: Vec<SignedRoster>,
    next_seq: u64,
    log: Vec<(u64, SealedEnvelope)>,
}

impl GroupLog {
    fn current(&self) -> &Roster {
        &self.rosters.last().unwrap().roster
    }
}

pub struct Fetched {
    pub rosters: Vec<SignedRoster>,
    pub envelopes: Vec<(u64, SealedEnvelope)>,
}

/// The blind relay: stores sealed envelopes per group and checks them against
/// the owner-signed roster. Never holds a document key.
#[derive(Default)]
pub struct SecureRelay {
    groups: HashMap<String, GroupLog>,
    pub stats: RelayStats,
    pub rejected: Vec<Rejection>,
}

impl SecureRelay {
    /// Trust on first use: the first roster pins the group's owner.
    pub fn create_group(&mut self, r: SignedRoster) -> Result<(), Rejection> {
        r.verify(&r.roster.owner)?;
        if r.roster.version != 1 || self.groups.contains_key(&r.roster.group) {
            return Err(Rejection::RosterOutOfOrder);
        }
        let log = GroupLog {
            rosters: vec![r.clone()],
            ..Default::default()
        };
        self.groups.insert(r.roster.group.clone(), log);
        Ok(())
    }

    pub fn update_roster(&mut self, r: SignedRoster) -> Result<(), Rejection> {
        let g = self
            .groups
            .get_mut(&r.roster.group)
            .ok_or(Rejection::UnknownGroup)?;
        let cur = g.current();
        r.verify(&cur.owner)?;
        if r.roster.version != cur.version + 1 || r.roster.epoch < cur.epoch {
            return Err(Rejection::RosterOutOfOrder);
        }
        g.rosters.push(r);
        Ok(())
    }

    pub fn publish(&mut self, env: SealedEnvelope) -> Result<u64, Rejection> {
        let out = self.try_publish(env);
        if let Err(e) = &out {
            self.rejected.push(e.clone());
        }
        out
    }

    fn try_publish(&mut self, env: SealedEnvelope) -> Result<u64, Rejection> {
        let g = self
            .groups
            .get_mut(&env.group)
            .ok_or(Rejection::UnknownGroup)?;
        let cur = g.current();
        // Freshness first: an honest relay only accepts envelopes sealed under
        // the CURRENT roster, which is what stops a removed or demoted member
        // from back-dating a write to a version where they still had rights.
        if cur.role_of(&env.signer).is_none() {
            return Err(Rejection::NotAMember);
        }
        if env.roster_version != cur.version {
            return Err(Rejection::StaleRoster {
                got: env.roster_version,
                current: cur.version,
            });
        }
        check_envelope(&env, cur)?;
        if env.kind == Kind::Compacted {
            return Err(Rejection::Malformed);
        }
        let seq = g.next_seq;
        g.next_seq += 1;
        self.stats.messages += 1;
        self.stats.bytes += env.wire_len() as u64;
        if env.kind == Kind::Snapshot {
            // Compaction: a snapshot supersedes everything earlier for its
            // doc, which is how the relay stops holding old-epoch blobs.
            g.log.retain(|(_, e)| e.doc_id != env.doc_id);
        }
        g.log.push((seq, env));
        Ok(seq)
    }

    /// Compaction (2c), atomically: adopt the owner's new roster (which
    /// lists the compacted docs), drop every stored envelope for those docs,
    /// and store the compacted ones. `base` is the log cursor the owner had
    /// fetched up to. If anything for one of these docs arrived since, the
    /// compaction doesn't include it and is refused, so the owner fetches
    /// and retries rather than the relay deleting someone's accepted write.
    pub fn compact(
        &mut self,
        r: SignedRoster,
        envs: Vec<SealedEnvelope>,
        base: u64,
    ) -> Result<(), Rejection> {
        let out = self.try_compact(r, envs, base);
        if let Err(e) = &out {
            self.rejected.push(e.clone());
        }
        out
    }

    fn try_compact(
        &mut self,
        r: SignedRoster,
        envs: Vec<SealedEnvelope>,
        base: u64,
    ) -> Result<(), Rejection> {
        let g = self
            .groups
            .get_mut(&r.roster.group)
            .ok_or(Rejection::UnknownGroup)?;
        let cur = g.current();
        r.verify(&cur.owner)?;
        if r.roster.version != cur.version + 1 || r.roster.epoch < cur.epoch {
            return Err(Rejection::RosterOutOfOrder);
        }
        let docs: BTreeSet<&str> = envs.iter().map(|e| e.doc_id.as_str()).collect();
        for env in &envs {
            if env.kind != Kind::Compacted
                || env.roster_version != r.roster.version
                || r.roster.compacted.get(&env.doc_id) != Some(&r.roster.version)
            {
                return Err(Rejection::Malformed);
            }
            check_envelope(env, &r.roster)?;
        }
        if g.log
            .iter()
            .any(|(seq, e)| *seq >= base && docs.contains(e.doc_id.as_str()))
        {
            return Err(Rejection::CompactionRaced);
        }
        g.log.retain(|(_, e)| !docs.contains(e.doc_id.as_str()));
        g.rosters.push(r);
        for env in envs {
            let seq = g.next_seq;
            g.next_seq += 1;
            self.stats.messages += 1;
            self.stats.bytes += env.wire_len() as u64;
            g.log.push((seq, env));
        }
        Ok(())
    }

    /// Envelopes with `seq >= since`, for a requester who proves possession of
    /// a current member's key. A removed member gets nothing new from an
    /// honest relay (and couldn't decrypt it from a dishonest one).
    pub fn fetch(
        &self,
        group: &str,
        requester: &PubKey,
        since: u64,
        proof: &[u8; 64],
    ) -> Result<Fetched, Rejection> {
        let g = self.groups.get(group).ok_or(Rejection::UnknownGroup)?;
        if g.current().role_of(requester).is_none() {
            return Err(Rejection::NotAMember);
        }
        verify(requester, &fetch_request(group, since), proof)?;
        Ok(Fetched {
            rosters: g.rosters.clone(),
            envelopes: g.log.iter().filter(|(s, _)| *s >= since).cloned().collect(),
        })
    }

    /// What a compromised relay could hand to anyone: every stored envelope.
    pub fn leak_all(&self, group: &str) -> Vec<SealedEnvelope> {
        self.groups[group]
            .log
            .iter()
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// Rosters are signed but not secret; a compromised relay can hand them out.
    pub fn leak_rosters(&self, group: &str) -> Vec<SignedRoster> {
        self.groups[group].rosters.clone()
    }

    /// Bytes currently stored for a group (after compaction).
    pub fn stored_bytes(&self, group: &str) -> usize {
        self.groups[group]
            .log
            .iter()
            .map(|(_, e)| e.wire_len())
            .sum()
    }

    /// Test hook for a compromised relay: append without any checks.
    pub fn inject_unchecked(&mut self, env: SealedEnvelope) {
        let g = self.groups.get_mut(&env.group).unwrap();
        let seq = g.next_seq;
        g.next_seq += 1;
        g.log.push((seq, env));
    }
}

fn fetch_request(group: &str, since: u64) -> Vec<u8> {
    let mut m = b"nels-spike/fetch/v1".to_vec();
    put(&mut m, group.as_bytes());
    m.extend_from_slice(&since.to_be_bytes());
    m
}

// --- client ----------------------------------------------------------------

struct ClientGroup {
    owner: PubKey,
    rosters: BTreeMap<u64, Roster>,
    keys: BTreeMap<u64, DocKey>,
    cursor: u64,
    /// Heads at which this device last published each doc.
    pushed: HashMap<String, Vec<ChangeHash>>,
}

impl ClientGroup {
    fn current(&self) -> &Roster {
        self.rosters.values().next_back().unwrap()
    }
}

/// One device's view of the secure network: its identity and, per group, the
/// verified roster chain, the keys it could unwrap, and its log cursor.
pub struct SecureClient {
    pub identity: Identity,
    groups: HashMap<String, ClientGroup>,
    /// Envelopes this client refused on receive, with the reason.
    pub rejected: Vec<Rejection>,
    /// Own unpublished writes dropped while rebasing onto a compaction
    /// (they touched an erased row, or were superseded).
    pub dropped_on_rebase: usize,
}

fn in_group(doc_id: &str, group: &str) -> bool {
    doc_id == model::budget_doc(group) || doc_id.starts_with(&model::txns_prefix(group))
}

impl SecureClient {
    fn new(label: &str) -> Self {
        Self {
            identity: Identity::from_label(label),
            groups: HashMap::new(),
            rejected: Vec::new(),
            dropped_on_rebase: 0,
        }
    }

    /// Accept an invitation: pin the owner key received out of band (QR code,
    /// link). Every roster is verified against it from here on.
    pub fn accept_invite(&mut self, group: &str, owner: PubKey) {
        self.groups.entry(group.to_string()).or_insert(ClientGroup {
            owner,
            rosters: BTreeMap::new(),
            keys: BTreeMap::new(),
            cursor: 0,
            pushed: HashMap::new(),
        });
    }

    /// Verify and adopt new roster versions; unwrap this device's key for
    /// each. A roster not signed by the pinned owner is refused.
    fn adopt_rosters(&mut self, group: &str, rosters: &[SignedRoster]) -> Result<(), Rejection> {
        let me = self.identity.public();
        let g = self.groups.get_mut(group).ok_or(Rejection::UnknownGroup)?;
        for r in rosters {
            if g.rosters.contains_key(&r.roster.version) {
                continue;
            }
            r.verify(&g.owner)?;
            if let Some(m) = r.roster.members.get(&me) {
                let ctx = key_context(group, r.roster.epoch);
                if let Some(k) = open_key(&m.wrapped_key, &self.identity, &ctx) {
                    g.keys.insert(r.roster.epoch, k);
                }
            }
            g.rosters.insert(r.roster.version, r.roster.clone());
        }
        Ok(())
    }

    /// Everything a receiver checks, independently of the relay.
    fn open(&self, env: &SealedEnvelope) -> Result<Opened, Rejection> {
        let g = self.groups.get(&env.group).ok_or(Rejection::UnknownGroup)?;
        let roster = g
            .rosters
            .get(&env.roster_version)
            .ok_or(Rejection::StaleRoster {
                got: env.roster_version,
                current: g.current().version,
            })?;
        check_envelope(env, roster)?;
        // Erased history: sealed before this doc's latest compaction. An
        // honest relay has dropped these; a compromised one could replay them.
        if g.current()
            .compacted
            .get(&env.doc_id)
            .is_some_and(|v| env.roster_version < *v)
        {
            return Err(Rejection::PreCompaction);
        }
        let key = g.keys.get(&env.epoch).ok_or(Rejection::BadCiphertext)?;
        let plain = env.decrypt(key)?;
        match env.kind {
            Kind::Changes => {
                let changes = decode_changes(&plain)?;
                let actor = &roster.members[&env.signer].actor;
                // Schema upgrade changes (2d) are relayed by whoever upgraded
                // first; they're checked structurally in `receive`.
                if changes
                    .iter()
                    .any(|c| c.actor_id() != actor && !schema::is_schema_actor(c.actor_id()))
                {
                    return Err(Rejection::ActorMismatch);
                }
                Ok(Opened::Changes(changes))
            }
            Kind::Snapshot => {
                let mut doc = AutoCommit::load(&plain).map_err(|_| Rejection::Malformed)?;
                // The owner vouches for a snapshot, but it still may only
                // contain changes by actors that were members at some point.
                let known: BTreeSet<&ActorId> = g
                    .rosters
                    .values()
                    .flat_map(|r| r.members.values().map(|m| &m.actor))
                    .collect();
                let genesis = ActorId::from(GENESIS_ACTOR);
                if doc
                    .get_changes(&[])
                    .iter()
                    .any(|c| c.actor_id() != &genesis && !known.contains(c.actor_id()))
                {
                    return Err(Rejection::ActorMismatch);
                }
                Ok(Opened::Snapshot(Box::new(doc)))
            }
            Kind::Compacted => {
                let mut doc = AutoCommit::load(&plain).map_err(|_| Rejection::Malformed)?;
                // Exactly genesis plus the owner's one compaction change:
                // anything else would be history the compaction claims to
                // have removed.
                // Schema upgrades (2d) are allowed too, each checked the same
                // way as a relayed one, replayed onto a fresh genesis.
                let owner_actor = &roster.members[&env.signer].actor;
                let changes = doc.get_changes(&[]);
                let genesis = ActorId::from(GENESIS_ACTOR);
                let shape_ok = changes.iter().filter(|c| c.actor_id() == &genesis).count() == 1
                    && changes
                        .iter()
                        .filter(|c| c.actor_id() == owner_actor)
                        .count()
                        == 1
                    && changes.iter().all(|c| {
                        c.actor_id() == owner_actor || schema::is_structural(c.actor_id())
                    });
                if !shape_ok {
                    return Err(Rejection::Malformed);
                }
                let mut fresh = model::genesis_for(&env.doc_id);
                for c in changes
                    .iter()
                    .filter(|c| schema::is_schema_actor(c.actor_id()))
                {
                    schema::check_upgrade(&mut fresh, &env.doc_id, c)
                        .map_err(|_| Rejection::BadUpgrade)?;
                    fresh.apply_changes([c.clone()]).unwrap();
                }
                Ok(Opened::Compacted(Box::new(doc)))
            }
        }
    }
}

/// Swap `device`'s copy of `doc_id` for a compacted doc, re-applying its own
/// unpublished edits on top as one new change. Returns the compacted doc's
/// heads (everything up to there counts as published) and how many rebased
/// writes were dropped (see [`erasure::replay`]).
fn rebase_onto(
    dev: &mut crate::Device,
    doc_id: &str,
    snap: AutoCommit,
    published: &[ChangeHash],
) -> (Vec<ChangeHash>, usize) {
    let mut snap = snap;
    // The compacted doc's own heads are what counts as published. Taken
    // BEFORE grafting local upgrades on, so those get republished: nobody
    // can apply our later changes without them.
    let heads = snap.get_heads();
    let patches = if dev.has_doc(doc_id) {
        let actor = dev.actor().clone();
        let old = dev.doc_mut(doc_id);
        // Keep upgrades this device has that the compactor didn't (2d), or
        // rebased writes into the new containers would find nothing there.
        snap.apply_changes(schema::upgrades_in(old)).unwrap();
        erasure::unpublished_patches(old, published, &actor)
    } else {
        Vec::new()
    };
    dev.replace_doc(doc_id, snap);
    let dropped = if patches.is_empty() {
        0
    } else {
        dev.change(doc_id, "rebase after compaction", |d| {
            erasure::replay(d, doc_id, &patches)
        })
    };
    (heads, dropped)
}

enum Opened {
    Changes(Vec<Change>),
    Snapshot(Box<AutoCommit>),
    Compacted(Box<AutoCommit>),
}

/// What a compaction cost the owner's device, and what it removed.
#[derive(Debug, Clone, Copy)]
pub struct CompactionCost {
    pub docs: usize,
    /// `save()` size of the docs before and after.
    pub bytes_before: usize,
    pub bytes_after: usize,
    /// Build + save + encrypt + sign, all docs (no relay round trip).
    pub elapsed: Duration,
}

/// What a key rotation cost the owner's device.
#[derive(Debug, Clone, Copy)]
pub struct RotationCost {
    pub docs: usize,
    pub snapshot_bytes: usize,
    /// save + encrypt + sign + publish, all docs.
    pub elapsed: Duration,
}

/// The secure relay plus each device's client state, driven against the
/// devices of a [`Harness`].
#[derive(Default)]
pub struct SecureNet {
    pub relay: SecureRelay,
    pub clients: HashMap<DeviceId, SecureClient>,
}

impl SecureNet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enroll(&mut self, device: &str) {
        self.clients
            .entry(device.to_string())
            .or_insert_with(|| SecureClient::new(device));
    }

    pub fn client(&self, device: &str) -> &SecureClient {
        &self.clients[device]
    }

    pub fn card(&self, h: &Harness, device: &str) -> MemberCard {
        let id = &self.clients[device].identity;
        MemberCard {
            public: id.public(),
            x_public: id.x_public(),
            actor: h.device(device).actor().clone(),
        }
    }

    fn cards(&self, h: &Harness, members: &[(&str, Role)]) -> Vec<(MemberCard, Role)> {
        members.iter().map(|(d, r)| (self.card(h, d), *r)).collect()
    }

    /// The owner creates a sharing group for budget `group` and invites
    /// `members` (which must include the owner as `Owner`).
    pub fn create_group(
        &mut self,
        h: &Harness,
        owner: &str,
        group: &str,
        members: &[(&str, Role)],
    ) -> Result<(), Rejection> {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let owner_pk = self.clients[owner].identity.public();
        let roster = Roster::build(group, 1, 1, owner_pk, &self.cards(h, members), &key)
            .sign(&self.clients[owner].identity);
        self.relay.create_group(roster)?;
        for (d, _) in members {
            self.clients
                .get_mut(*d)
                .unwrap()
                .accept_invite(group, owner_pk);
        }
        Ok(())
    }

    /// Publish this device's own unpublished changes, one envelope per doc.
    /// A stale roster (the owner rotated while we were offline) triggers one
    /// refresh-and-reseal. Returns envelopes accepted.
    pub fn push(&mut self, h: &mut Harness, device: &str, group: &str) -> Result<usize, Rejection> {
        if !h.device(device).is_online() {
            return Ok(0);
        }
        if self.clients[device].groups[group].rosters.is_empty() {
            // Never fetched a roster yet, so no key to seal with.
            self.pull(h, device, group)?;
        }
        let actor = h.device(device).actor().clone();
        let doc_ids: Vec<String> = h
            .device(device)
            .docs_with_prefix("")
            .map(|(d, _)| d.clone())
            .filter(|d| in_group(d, group))
            .collect();
        let mut sent = 0;
        for doc_id in doc_ids {
            let mut attempt = 0;
            loop {
                // Recomputed on a retry: the refresh may have delivered a
                // compaction, which replaces the doc and rebases our changes.
                let since = self.clients[device].groups[group]
                    .pushed
                    .get(&doc_id)
                    .cloned()
                    .unwrap_or_default();
                let doc = h.device_mut(device).doc_mut(&doc_id);
                let mine: Vec<Change> = doc
                    .get_changes(&since)
                    .into_iter()
                    // Own changes, plus schema upgrades this device holds
                    // (2d): other devices can't apply our later changes
                    // without them. Re-sending one someone else already
                    // published is a harmless duplicate.
                    .filter(|c| c.actor_id() == &actor || schema::is_schema_actor(c.actor_id()))
                    .collect();
                if mine.is_empty() {
                    break;
                }
                let heads = doc.get_heads();
                let env = self.seal(
                    device,
                    group,
                    &doc_id,
                    Kind::Changes,
                    &encode_changes(&mine),
                );
                match self.relay.publish(env) {
                    Ok(_) => {
                        self.clients
                            .get_mut(device)
                            .unwrap()
                            .groups
                            .get_mut(group)
                            .unwrap()
                            .pushed
                            .insert(doc_id.clone(), heads);
                        sent += 1;
                        break;
                    }
                    Err(Rejection::StaleRoster { .. }) if attempt == 0 => {
                        attempt += 1;
                        self.pull(h, device, group)?;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(sent)
    }

    /// Seal as `device` under its current roster and key. Public so a test
    /// can play a malicious member who seals whatever it likes.
    pub fn seal(
        &self,
        device: &str,
        group: &str,
        doc_id: &str,
        kind: Kind,
        plaintext: &[u8],
    ) -> SealedEnvelope {
        let c = &self.clients[device];
        let g = &c.groups[group];
        let r = g.current();
        SealedEnvelope::seal(
            &c.identity,
            group,
            doc_id,
            r.version,
            r.epoch,
            kind,
            &g.keys[&r.epoch],
            plaintext,
        )
    }

    /// Fetch new rosters and envelopes, verify each envelope independently of
    /// the relay, and apply what passes. Returns envelopes applied.
    pub fn pull(&mut self, h: &mut Harness, device: &str, group: &str) -> Result<usize, Rejection> {
        if !h.device(device).is_online() {
            return Ok(0);
        }
        let (me, since) = {
            let c = &self.clients[device];
            (c.identity.public(), c.groups[group].cursor)
        };
        let proof = self.clients[device]
            .identity
            .sign(&fetch_request(group, since));
        let fetched = self.relay.fetch(group, &me, since, &proof)?;
        self.receive(h, device, group, fetched)
    }

    /// Apply a fetch result. Public so a test can hand a device envelopes
    /// straight from a compromised relay.
    pub fn receive(
        &mut self,
        h: &mut Harness,
        device: &str,
        group: &str,
        fetched: Fetched,
    ) -> Result<usize, Rejection> {
        let client = self.clients.get_mut(device).unwrap();
        client.adopt_rosters(group, &fetched.rosters)?;
        let mut applied = 0;
        for (seq, env) in fetched.envelopes {
            let g = client.groups.get_mut(group).unwrap();
            g.cursor = g.cursor.max(seq + 1);
            match client.open(&env) {
                Ok(Opened::Compacted(snap)) => {
                    let g = client.groups.get_mut(group).unwrap();
                    let published = g.pushed.get(&env.doc_id).cloned().unwrap_or_default();
                    let (new_heads, dropped) =
                        rebase_onto(h.device_mut(device), &env.doc_id, *snap, &published);
                    g.pushed.insert(env.doc_id.clone(), new_heads);
                    client.dropped_on_rebase += dropped;
                    applied += 1;
                }
                Ok(opened) => {
                    let dev = h.device_mut(device);
                    model::open_by_id(dev, &env.doc_id);
                    let doc = dev.doc_mut(&env.doc_id);
                    match opened {
                        Opened::Changes(changes) => {
                            let bad = changes.iter().any(|c| {
                                schema::is_schema_actor(c.actor_id())
                                    && schema::check_upgrade(doc, &env.doc_id, c).is_err()
                            });
                            if bad {
                                client.rejected.push(Rejection::BadUpgrade);
                                continue;
                            }
                            doc.apply_changes(changes).unwrap()
                        }
                        Opened::Snapshot(mut snap) => {
                            doc.merge(&mut snap).unwrap();
                        }
                        Opened::Compacted(_) => unreachable!(),
                    }
                    applied += 1;
                }
                Err(e) => client.rejected.push(e),
            }
        }
        Ok(applied)
    }

    /// Push and pull every online enrolled device until nothing moves.
    pub fn sync(&mut self, h: &mut Harness, group: &str) {
        let devices: Vec<DeviceId> = h
            .device_ids()
            .into_iter()
            .filter(|d| {
                self.clients
                    .get(d)
                    .is_some_and(|c| c.groups.contains_key(group))
            })
            .collect();
        for _ in 0..100 {
            let mut moved = 0;
            for d in &devices {
                // A viewer's rejected write is reported by `push`; sync keeps
                // going so the viewer still receives everyone else's changes.
                moved += self.push(h, d, group).unwrap_or(0);
                if let Ok(n) = self.pull(h, d, group) {
                    moved += n;
                }
            }
            if moved == 0 {
                return;
            }
        }
        panic!("secure sync did not go quiet");
    }

    /// Owner changes the member list. If anyone was removed, rotate: new key
    /// epoch, and a fresh snapshot of every doc sealed under it (which also
    /// compacts the relay's log). Adding members or changing roles keeps the
    /// epoch and just wraps the current key for the new list.
    pub fn set_members(
        &mut self,
        h: &mut Harness,
        owner: &str,
        group: &str,
        members: &[(&str, Role)],
    ) -> Result<Option<RotationCost>, Rejection> {
        // Owner must be current, so the snapshot includes everything published.
        self.push(h, owner, group)?;
        self.pull(h, owner, group)?;

        let cards = self.cards(h, members);
        let (next, removed, old_epoch, old_key) = {
            let g = &self.clients[owner].groups[group];
            let cur = g.current();
            let kept: BTreeSet<PubKey> = cards.iter().map(|(c, _)| c.public).collect();
            let removed = cur.members.keys().any(|pk| !kept.contains(pk));
            (cur.version + 1, removed, cur.epoch, g.keys[&cur.epoch])
        };
        let (epoch, key) = if removed {
            let mut k = [0u8; 32];
            OsRng.fill_bytes(&mut k);
            (old_epoch + 1, k)
        } else {
            (old_epoch, old_key)
        };
        let owner_pk = self.clients[owner].identity.public();
        let mut roster = Roster::build(group, next, epoch, owner_pk, &cards, &key);
        roster.compacted = self.clients[owner].groups[group]
            .current()
            .compacted
            .clone();
        let roster = roster.sign(&self.clients[owner].identity);
        self.relay.update_roster(roster.clone())?;
        self.clients
            .get_mut(owner)
            .unwrap()
            .adopt_rosters(group, &[roster])?;
        for (d, _) in members {
            self.clients
                .get_mut(*d)
                .unwrap()
                .accept_invite(group, owner_pk);
        }
        if !removed {
            return Ok(None);
        }

        let started = Instant::now();
        let doc_ids: Vec<String> = h
            .device(owner)
            .docs_with_prefix("")
            .map(|(d, _)| d.clone())
            .filter(|d| in_group(d, group))
            .collect();
        let mut bytes = 0;
        for doc_id in &doc_ids {
            let snapshot = h.device_mut(owner).doc_mut(doc_id).save();
            bytes += snapshot.len();
            let env = self.seal(owner, group, doc_id, Kind::Snapshot, &snapshot);
            self.relay.publish(env)?;
        }
        // Everything the owner holds is now in the snapshots.
        let owner_client = self.clients.get_mut(owner).unwrap();
        for doc_id in &doc_ids {
            let heads = h.device_mut(owner).doc_mut(doc_id).get_heads();
            owner_client
                .groups
                .get_mut(group)
                .unwrap()
                .pushed
                .insert(doc_id.clone(), heads);
        }
        Ok(Some(RotationCost {
            docs: doc_ids.len(),
            snapshot_bytes: bytes,
            elapsed: started.elapsed(),
        }))
    }

    /// Compact every doc of the group (2c): the owner catches up, builds
    /// each doc afresh with no history and deleted rows reduced to their
    /// tombstone, and publishes them with a new roster version that marks
    /// the old history dead. The relay drops every earlier envelope for
    /// those docs. Members replace their copies on their next pull and
    /// rebase anything they hadn't published.
    ///
    /// Same members and same key epoch: compaction hides nothing from
    /// current members (they had it all), so there is nothing to re-key for.
    /// Removing a member is what needs a new epoch ([`Self::set_members`]).
    pub fn compact(
        &mut self,
        h: &mut Harness,
        owner: &str,
        group: &str,
    ) -> Result<CompactionCost, Rejection> {
        self.push(h, owner, group)?;
        self.pull(h, owner, group)?;
        self.compact_as_of_last_fetch(h, owner, group)
    }

    /// [`Self::compact`] without catching up first, so a test can race a
    /// member's publish against it.
    pub fn compact_as_of_last_fetch(
        &mut self,
        h: &mut Harness,
        owner: &str,
        group: &str,
    ) -> Result<CompactionCost, Rejection> {
        let started = Instant::now();
        let dev = h.device_mut(owner);
        let actor = dev.actor().clone();
        let time = dev.clock.now().timestamp();
        let budget_id = group;
        let budget = model::budget_doc(budget_id);
        let txn_ids: Vec<String> = dev
            .docs_with_prefix(&model::txns_prefix(budget_id))
            .map(|(d, _)| d.clone())
            .collect();

        // A closed budget's `close_seen` holds change hashes that are about
        // to stop existing. Settle "what changed after the close" now, while
        // they still resolve, and write the answer into the compacted docs.
        let late: Vec<String> = {
            let view = crate::read::BudgetView::load(dev, budget_id);
            crate::read::changed_after_close(&view).map_err(|_| Rejection::CompactionRaced)?
        };

        let mut bytes_before = 0;
        let mut compacted: Vec<(String, AutoCommit)> = Vec::new();
        for id in &txn_ids {
            let old = dev.doc_mut(id);
            bytes_before += old.save().len();
            compacted.push((
                id.clone(),
                erasure::compact(old, id, actor.clone(), time, |_| {}),
            ));
        }
        let old_budget = dev.doc_mut(&budget);
        bytes_before += old_budget.save().len();
        let closed_docs: Vec<String> = old_budget
            .keys(model::map(old_budget, "close_seen"))
            .collect();
        let rewritten: Vec<(String, String)> = closed_docs
            .iter()
            .filter_map(|doc_id| {
                let (_, new) = compacted.iter_mut().find(|(id, _)| id == doc_id)?;
                let rows: BTreeSet<String> = new.keys(model::map(new, "date")).collect();
                let mine: Vec<String> =
                    late.iter().filter(|t| rows.contains(*t)).cloned().collect();
                Some((
                    doc_id.clone(),
                    erasure::encode_close_seen(&new.get_heads(), &mine),
                ))
            })
            .collect();
        let new_budget = erasure::compact(old_budget, &budget, actor, time, |d| {
            let cs = model::map(d, "close_seen");
            for (doc_id, value) in &rewritten {
                d.put(&cs, doc_id.as_str(), value.as_str()).unwrap();
            }
        });
        compacted.push((budget, new_budget));

        // New roster version: same members, same epoch, every doc marked.
        let (base, mut roster) = {
            let g = &self.clients[owner].groups[group];
            (g.cursor, g.current().clone())
        };
        roster.version += 1;
        for (id, _) in &compacted {
            roster.compacted.insert(id.clone(), roster.version);
        }
        let signed = roster.sign(&self.clients[owner].identity);
        self.clients
            .get_mut(owner)
            .unwrap()
            .adopt_rosters(group, std::slice::from_ref(&signed))?;

        let mut bytes_after = 0;
        let mut envs = Vec::new();
        for (id, doc) in &mut compacted {
            let bytes = doc.save();
            bytes_after += bytes.len();
            envs.push(self.seal(owner, group, id, Kind::Compacted, &bytes));
        }
        let elapsed = started.elapsed();
        if let Err(e) = self.relay.compact(signed, envs, base) {
            // Roll the owner's own view back so it can fetch and retry.
            let g = self
                .clients
                .get_mut(owner)
                .unwrap()
                .groups
                .get_mut(group)
                .unwrap();
            let v = *g.rosters.keys().next_back().unwrap();
            g.rosters.remove(&v);
            return Err(e);
        }
        let docs = compacted.len();
        let owner_client = self.clients.get_mut(owner).unwrap();
        for (id, mut doc) in compacted {
            let heads = doc.get_heads();
            h.device_mut(owner).replace_doc(&id, doc);
            owner_client
                .groups
                .get_mut(group)
                .unwrap()
                .pushed
                .insert(id, heads);
        }
        Ok(CompactionCost {
            docs,
            bytes_before,
            bytes_after,
            elapsed,
        })
    }

    /// The owner-side cost of wrapping one key for N members, for the
    /// findings: rotation cost is dominated by snapshots, not by wrapping.
    pub fn wrap_cost(members: usize) -> Duration {
        let key = [7u8; 32];
        let x = Identity::from_label("probe").x_public();
        let started = Instant::now();
        for _ in 0..members {
            seal_key(&key, &x, b"probe");
        }
        started.elapsed()
    }
}
