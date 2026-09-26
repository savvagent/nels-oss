//! Nels's pure domain rules (spec savvagent/nels-oss#5).
//!
//! PURITY CONTRACT: nothing in this crate performs I/O, reads the clock, reads
//! the environment, or draws entropy. Callers pass "now"/"today" in, and seeded
//! randomness only. The backend runs these functions; a future local-first
//! client runs the same functions compiled to Wasm, so both compute the same
//! numbers from the same source. `scripts/check-purity.sh` and chrono's missing
//! `clock` feature enforce this; see AGENTS.md §28.
#![forbid(unsafe_code)]

pub mod assets;
pub mod bank_provider;
pub mod budget;
pub mod entitlement;
pub mod error;
pub mod goals;
pub mod notifications;
pub mod retirement;
pub mod retirement_projection;
pub mod social_security;
