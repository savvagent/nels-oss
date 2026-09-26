//! Shared bank-provider abstraction (nels#320, extended by nels#322, nels#323,
//! nels#321): a closed, six-member set (Stripe Financial Connections,
//! GoCardless Bank Account Data, Belvo, Basiq, Akahu, Plaid), so enum
//! dispatch is used instead of an `async-trait`+`dyn` object — see the
//! GoCardless spec's Assumption 8 for the original tradeoff rationale,
//! reaffirmed by the Belvo spec's Assumption 14, the Basiq/Akahu spec's
//! Assumption 14, and the Plaid spec's Assumption 1 (each additional
//! provider costs one match arm per site, not a rewrite).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Stripe,
    GoCardless,
    Belvo,
    Basiq,
    Akahu,
    Plaid,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Stripe => "stripe",
            Provider::GoCardless => "gocardless",
            Provider::Belvo => "belvo",
            Provider::Basiq => "basiq",
            Provider::Akahu => "akahu",
            Provider::Plaid => "plaid",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "stripe" => Some(Provider::Stripe),
            "gocardless" => Some(Provider::GoCardless),
            "belvo" => Some(Provider::Belvo),
            "basiq" => Some(Provider::Basiq),
            "akahu" => Some(Provider::Akahu),
            "plaid" => Some(Provider::Plaid),
            _ => None,
        }
    }

    /// Map an ISO 3166-1 alpha-2 country code (case-insensitive) to the
    /// provider that covers it. `US` -> Stripe; the 8 GoCardless launch
    /// countries (nels#320) -> GoCardless; `MX`/`BR` -> Belvo (nels#322);
    /// `AU` -> Basiq; `NZ` -> Akahu (nels#323 — Akahu chosen over Basiq's
    /// partial NZ coverage); `CA` -> Plaid (nels#321); anything else -> None
    /// (caller must ask the user for a supported country, never guess).
    pub fn for_country(cc: &str) -> Option<Provider> {
        match cc.to_uppercase().as_str() {
            "US" => Some(Provider::Stripe),
            "GB" | "FR" | "DE" | "IT" | "ES" | "DK" | "FI" | "NO" => Some(Provider::GoCardless),
            "MX" | "BR" => Some(Provider::Belvo),
            "AU" => Some(Provider::Basiq),
            "NZ" => Some(Provider::Akahu),
            "CA" => Some(Provider::Plaid),
            _ => None,
        }
    }

    /// (code, display name) pairs for every supported country, in the order
    /// the tickets list them — used both by the REST institutions-country
    /// list and chat's clarifying-question copy.
    pub fn supported_countries() -> &'static [(&'static str, &'static str)] {
        &[
            ("US", "United States"),
            ("GB", "United Kingdom"),
            ("FR", "France"),
            ("DE", "Germany"),
            ("IT", "Italy"),
            ("ES", "Spain"),
            ("DK", "Denmark"),
            ("FI", "Finland"),
            ("NO", "Norway"),
            ("MX", "Mexico"),
            ("BR", "Brazil"),
            ("AU", "Australia"),
            ("NZ", "New Zealand"),
            ("CA", "Canada"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_country_maps_us_to_stripe() {
        assert_eq!(Provider::for_country("US"), Some(Provider::Stripe));
    }

    #[test]
    fn for_country_maps_gocardless_countries() {
        for cc in ["GB", "FR", "DE", "IT", "ES", "DK", "FI", "NO"] {
            assert_eq!(Provider::for_country(cc), Some(Provider::GoCardless), "country {cc}");
        }
    }

    #[test]
    fn for_country_maps_au_to_basiq() {
        assert_eq!(Provider::for_country("AU"), Some(Provider::Basiq));
    }

    #[test]
    fn for_country_maps_nz_to_akahu() {
        assert_eq!(Provider::for_country("NZ"), Some(Provider::Akahu));
    }

    #[test]
    fn for_country_au_nz_case_insensitive() {
        assert_eq!(Provider::for_country("au"), Some(Provider::Basiq));
        assert_eq!(Provider::for_country("nz"), Some(Provider::Akahu));
    }

    #[test]
    fn for_country_is_case_insensitive() {
        assert_eq!(Provider::for_country("gb"), Some(Provider::GoCardless));
        assert_eq!(Provider::for_country("us"), Some(Provider::Stripe));
    }

    #[test]
    fn for_country_rejects_unsupported() {
        assert_eq!(Provider::for_country("XX"), None);
        assert_eq!(Provider::for_country(""), None);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        // Covers all six variants, including Belvo/Basiq/Akahu/Plaid — a
        // separate as_str_round_trips_basiq_and_akahu test previously
        // duplicated this exact coverage; removed as redundant (Copilot
        // review finding, #323).
        for p in [
            Provider::Stripe,
            Provider::GoCardless,
            Provider::Belvo,
            Provider::Basiq,
            Provider::Akahu,
            Provider::Plaid,
        ] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn parse_accepts_plaid() {
        assert_eq!(Provider::parse("plaid"), Some(Provider::Plaid));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Provider::parse("venmo"), None);
    }

    #[test]
    fn for_country_maps_belvo_countries() {
        assert_eq!(Provider::for_country("MX"), Some(Provider::Belvo));
        assert_eq!(Provider::for_country("BR"), Some(Provider::Belvo));
    }

    #[test]
    fn for_country_is_case_insensitive_for_belvo() {
        assert_eq!(Provider::for_country("mx"), Some(Provider::Belvo));
        assert_eq!(Provider::for_country("br"), Some(Provider::Belvo));
    }

    #[test]
    fn for_country_maps_ca_to_plaid() {
        assert_eq!(Provider::for_country("CA"), Some(Provider::Plaid));
        assert_eq!(Provider::for_country("ca"), Some(Provider::Plaid));
    }

    #[test]
    fn supported_countries_lists_all_fourteen() {
        assert_eq!(Provider::supported_countries().len(), 14);
    }

    #[test]
    fn belvo_round_trips_through_parse() {
        assert_eq!(Provider::parse(Provider::Belvo.as_str()), Some(Provider::Belvo));
    }
}
