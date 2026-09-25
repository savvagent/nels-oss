//! Throwaway spike for epic #566. See
//! `docs/superpowers/plans/2026-09-24-local-first-automerge-spike.md`.
//!
//! This crate answers questions; it does not build the product. Nothing here
//! is imported by `backend/` or `frontend/`.

pub mod clock;
pub mod device;
pub mod erasure;
pub mod harness;
pub mod model;
pub mod period;
pub mod read;
pub mod relay;
pub mod schema;
pub mod secure;
pub mod snapshot;
pub mod workload;

pub use clock::SimClock;
pub use device::{Device, DeviceId, DocId};
pub use harness::Harness;
pub use relay::{Envelope, Relay, RelayStats};
pub use secure::{Rejection, Role, SecureNet};
