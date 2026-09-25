//! N simulated devices in one process, connected through a [`Relay`].
//!
//! Every Week 1 scenario is: set up shared state, take devices offline, apply
//! edits, bring them back, [`Harness::sync_all`], then assert on the merged
//! document and on what the derived read shows the user.

use std::collections::HashMap;

use automerge::sync::{Message, SyncDoc};

use crate::clock::SimClock;
use crate::device::{Device, DeviceId, DocId};
use crate::relay::{Envelope, Relay};

/// A sync that has not gone quiet after this many rounds is a harness or
/// protocol bug, not a slow network. Fail loudly instead of hanging the test.
const MAX_ROUNDS: usize = 1_000;

#[derive(Default)]
pub struct Harness {
    devices: HashMap<DeviceId, Device>,
    /// Insertion order, so `sync_all` visits pairs deterministically.
    order: Vec<DeviceId>,
    pub relay: Relay,
}

impl Harness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_device(&mut self, id: &str, clock: SimClock) -> &mut Device {
        assert!(!self.devices.contains_key(id), "duplicate device {id}");
        self.order.push(id.to_string());
        self.devices.insert(id.to_string(), Device::new(id, clock));
        self.devices.get_mut(id).unwrap()
    }

    pub fn device(&self, id: &str) -> &Device {
        self.devices
            .get(id)
            .unwrap_or_else(|| panic!("no device {id}"))
    }

    pub fn device_mut(&mut self, id: &str) -> &mut Device {
        self.devices
            .get_mut(id)
            .unwrap_or_else(|| panic!("no device {id}"))
    }

    pub fn device_ids(&self) -> Vec<DeviceId> {
        self.order.clone()
    }

    pub fn go_offline(&mut self, id: &str) {
        self.device_mut(id).online = false;
    }

    /// Reconnect a device. Both it and every peer drop in-flight sync state
    /// for each other, as real clients do after a dropped connection.
    pub fn go_online(&mut self, id: &str) {
        for device in self.devices.values_mut() {
            device.reset_peer_states();
        }
        self.device_mut(id).online = true;
    }

    /// Sync every online pair, in insertion order, until a full pass moves no
    /// messages. Returns the number of passes.
    pub fn sync_all(&mut self) -> usize {
        let online: Vec<DeviceId> = self
            .order
            .iter()
            .filter(|id| self.devices[*id].online)
            .cloned()
            .collect();
        let mut pairs = Vec::new();
        for (i, a) in online.iter().enumerate() {
            for b in &online[i + 1..] {
                pairs.push((a.clone(), b.clone()));
            }
        }
        self.sync_in_order(&pairs)
    }

    /// Sync the given pairs in exactly this order, repeating the sequence until
    /// it goes quiet. Used to show convergence does not depend on sync order.
    pub fn sync_in_order(&mut self, pairs: &[(DeviceId, DeviceId)]) -> usize {
        for pass in 1..=MAX_ROUNDS {
            let mut moved = 0;
            for (a, b) in pairs {
                moved += self.sync_pair(a, b);
            }
            if moved == 0 {
                return pass;
            }
        }
        panic!("sync_in_order did not converge within {MAX_ROUNDS} passes");
    }

    /// Exchange sync messages between two devices for every document both have
    /// joined, until neither has anything to send. Returns messages moved.
    /// A no-op if either device is offline.
    pub fn sync_pair(&mut self, a: &str, b: &str) -> usize {
        if !self.device(a).online || !self.device(b).online {
            return 0;
        }
        let shared: Vec<DocId> = self
            .device(a)
            .replicas
            .keys()
            .filter(|doc_id| self.device(b).has_doc(doc_id))
            .cloned()
            .collect();

        let mut moved = 0;
        for doc_id in shared {
            for _ in 0..MAX_ROUNDS {
                let sent = self.send(a, b, &doc_id) + self.send(b, a, &doc_id);
                if sent == 0 {
                    break;
                }
                moved += sent;
            }
        }
        moved
    }

    /// Generate at most one sync message from `from` to `to` for `doc_id`,
    /// route it through the relay, and apply it. Returns 1 if a message moved.
    fn send(&mut self, from: &str, to: &str, doc_id: &str) -> usize {
        let payload = {
            let replica = self.device_mut(from).replicas.get_mut(doc_id).unwrap();
            let state = replica.peers.entry(to.to_string()).or_default();
            match replica.doc.sync().generate_sync_message(state) {
                Some(message) => message.encode(),
                None => return 0,
            }
        };
        let envelope = Envelope {
            from: from.to_string(),
            to: to.to_string(),
            doc_id: doc_id.to_string(),
            payload,
        };
        let delivered = self.relay.forward(envelope);

        let replica = self.device_mut(to).replicas.get_mut(doc_id).unwrap();
        let state = replica.peers.entry(from.to_string()).or_default();
        let message = Message::decode(&delivered.payload).expect("decode sync message");
        replica
            .doc
            .sync()
            .receive_sync_message(state, message)
            .expect("apply sync message");
        1
    }
}
