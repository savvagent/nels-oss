//! The relay: every sync message between two devices passes through here.
//!
//! This is the TRUSTED plaintext transport Week 1 runs on: it forwards
//! Automerge sync messages and counts bytes. It cannot enforce roles, and
//! Week 2a found it can't be made to (see the module docs of
//! [`crate::secure`]): the enforcing, blind relay is
//! [`crate::secure::SecureRelay`], which carries per-author change batches
//! instead of sync messages.

use crate::device::{DeviceId, DocId};

#[derive(Debug, Clone)]
pub struct Envelope {
    pub from: DeviceId,
    pub to: DeviceId,
    pub doc_id: DocId,
    /// An encoded `automerge::sync::Message`. Opaque to the relay.
    pub payload: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayStats {
    pub messages: u64,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct Relay {
    pub stats: RelayStats,
}

impl Relay {
    pub fn forward(&mut self, envelope: Envelope) -> Envelope {
        self.stats.messages += 1;
        self.stats.bytes += envelope.payload.len() as u64;
        envelope
    }
}
