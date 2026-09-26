//! Env wiring for the subscription entitlement rules, which live in
//! nels-core (spec savvagent/nels-oss#5). Core cannot read the environment.

// Pure domain rules live in nels-core (core/, spec savvagent/nels-oss#5).
// Re-exported so existing `crate::entitlement::…` paths keep resolving.
// Add NEW pure rules to core/, not here.
pub use nels_core::entitlement::*;

/// Reads the four Stripe price env vars once. Moved verbatim from the former
/// `PriceCatalog::from_env` (an inherent method can't live outside core).
pub fn price_catalog_from_env() -> PriceCatalog {
    let get = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    let basic = ["STRIPE_PRICE_BASIC_MONTHLY", "STRIPE_PRICE_BASIC_ANNUAL"]
        .iter().filter_map(|k| get(k)).collect();
    let pro = ["STRIPE_PRICE_PRO_MONTHLY", "STRIPE_PRICE_PRO_ANNUAL"]
        .iter().filter_map(|k| get(k)).collect();
    PriceCatalog::new(basic, pro)
}
