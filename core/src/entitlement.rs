//! Entitlement derivation (two-tier billing). Pure logic: `resolve()` maps a
//! Stripe (status, price_id) pair to an `Entitlement`. The backend's
//! `price_catalog_from_env()` builds the catalog from env vars (core cannot
//! read the environment).

/// Feature tier granted by a subscription. Ordered `None < Basic < Pro`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    None,
    Basic,
    Pro,
}

/// Known Stripe price ids grouped by the tier they grant. Built from env once
/// (`from_env`) or explicitly in tests (`new`).
pub struct PriceCatalog {
    basic: Vec<String>,
    pro: Vec<String>,
}

impl PriceCatalog {
    pub fn new(basic: Vec<String>, pro: Vec<String>) -> Self {
        Self { basic, pro }
    }

    /// `Some(tier)` for a known price id; `None` for unknown/absent (the
    /// "unrecognized price" signal `resolve()` handles via the fail-safe rule).
    pub fn tier_for(&self, price_id: Option<&str>) -> Option<Tier> {
        let id = price_id?;
        if self.pro.iter().any(|p| p == id) {
            Some(Tier::Pro)
        } else if self.basic.iter().any(|b| b == id) {
            Some(Tier::Basic)
        } else {
            None
        }
    }
}

/// Resolved entitlement for a subscription row (or absence of one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entitlement {
    pub tier: Tier,
    /// True only during `past_due` grace — the UI shows "update your card".
    pub payment_warning: bool,
}

const NONE: Entitlement = Entitlement { tier: Tier::None, payment_warning: false };

/// Map a persisted Stripe (status, price_id) pair to an `Entitlement`.
/// Pure — all environment/config arrives via `catalog`. See the module docs and
/// the spec's Section 1 status table.
pub fn resolve(status: Option<&str>, price_id: Option<&str>, catalog: &PriceCatalog) -> Entitlement {
    let (entitled, warning) = match status {
        Some("trialing") => return Entitlement { tier: Tier::Pro, payment_warning: false },
        Some("active") => (true, false),
        Some("past_due") => (true, true),
        _ => (false, false), // canceled/unpaid/incomplete/incomplete_expired/paused/unknown/none
    };
    if !entitled {
        return NONE;
    }
    // Entitled: derive tier from price, failing safe to Basic on an unknown/absent id.
    let tier = match catalog.tier_for(price_id) {
        Some(t) => t,
        None => {
            tracing::error!(
                ?price_id,
                ?status,
                "entitled subscription with unrecognized price_id; failing safe to Basic"
            );
            Tier::Basic
        }
    };
    Entitlement { tier, payment_warning: warning }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> PriceCatalog {
        PriceCatalog::new(
            vec!["price_basic_m".into(), "price_basic_a".into()],
            vec!["price_pro_m".into(), "price_pro_a".into()],
        )
    }

    #[test]
    fn tier_ordering_none_lt_basic_lt_pro() {
        assert!(Tier::None < Tier::Basic);
        assert!(Tier::Basic < Tier::Pro);
    }

    #[test]
    fn tier_for_known_pro_price() {
        assert_eq!(catalog().tier_for(Some("price_pro_a")), Some(Tier::Pro));
    }

    #[test]
    fn tier_for_known_basic_price() {
        assert_eq!(catalog().tier_for(Some("price_basic_m")), Some(Tier::Basic));
    }

    #[test]
    fn tier_for_unknown_price_is_none() {
        assert_eq!(catalog().tier_for(Some("price_wat")), None);
    }

    #[test]
    fn tier_for_absent_price_is_none() {
        assert_eq!(catalog().tier_for(None), None);
    }

    #[test]
    fn resolve_no_subscription_is_none() {
        let e = resolve(None, None, &catalog());
        assert_eq!(e, Entitlement { tier: Tier::None, payment_warning: false });
    }

    #[test]
    fn resolve_trialing_is_pro_regardless_of_price() {
        // Trial grants Pro even if the price is a Basic id or absent.
        assert_eq!(resolve(Some("trialing"), None, &catalog()).tier, Tier::Pro);
        assert_eq!(resolve(Some("trialing"), Some("price_basic_m"), &catalog()).tier, Tier::Pro);
        assert!(!resolve(Some("trialing"), None, &catalog()).payment_warning);
    }

    #[test]
    fn resolve_active_uses_price_tier() {
        assert_eq!(resolve(Some("active"), Some("price_pro_a"), &catalog()).tier, Tier::Pro);
        assert_eq!(resolve(Some("active"), Some("price_basic_a"), &catalog()).tier, Tier::Basic);
    }

    #[test]
    fn resolve_active_unknown_price_fails_safe_to_basic() {
        let e = resolve(Some("active"), Some("price_unknown"), &catalog());
        assert_eq!(e.tier, Tier::Basic);
        assert!(!e.payment_warning);
    }

    #[test]
    fn resolve_active_missing_price_fails_safe_to_basic() {
        // Entitled but no price id at all → Basic (never locked out, never Pro).
        assert_eq!(resolve(Some("active"), None, &catalog()).tier, Tier::Basic);
    }

    #[test]
    fn resolve_past_due_keeps_tier_and_warns() {
        let e = resolve(Some("past_due"), Some("price_pro_m"), &catalog());
        assert_eq!(e.tier, Tier::Pro);
        assert!(e.payment_warning, "past_due must set the payment warning");
    }

    #[test]
    fn resolve_past_due_unknown_price_is_basic_and_warns() {
        let e = resolve(Some("past_due"), Some("price_unknown"), &catalog());
        assert_eq!(e.tier, Tier::Basic);
        assert!(e.payment_warning);
    }

    #[test]
    fn resolve_terminal_statuses_are_none() {
        for s in ["canceled", "unpaid", "incomplete", "incomplete_expired", "paused", "bogus"] {
            let e = resolve(Some(s), Some("price_pro_a"), &catalog());
            assert_eq!(e, Entitlement { tier: Tier::None, payment_warning: false }, "{s} must be None");
        }
    }
}
