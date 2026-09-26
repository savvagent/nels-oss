//! Budget-limit alert levels and reminder cadence, moved from
//! backend/src/notifications.rs (spec savvagent/nels-oss#5).

use chrono::{DateTime, Duration, Utc};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LimitLevel {
    Approaching,
    Exceeded,
}

/// Which limit threshold `spent` has reached against an optional `limit`.
/// None when there is no positive limit or spending is below 80%.
pub fn limit_level(spent: f64, limit: Option<f64>) -> Option<LimitLevel> {
    let lim = limit?;
    if lim <= 0.0 {
        return None;
    }
    if spent > lim {
        Some(LimitLevel::Exceeded)
    } else if spent >= 0.8 * lim {
        Some(LimitLevel::Approaching)
    } else {
        None
    }
}

/// One of a limit scope's two dedup keys (a scope = one category, or the whole
/// budget). Each scope has exactly one `exceeded` and one `approaching` alert key.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LimitKey {
    Exceeded,
    Approaching,
}

/// DB-free core of limit reconciliation: given the current level (None when the
/// scope is back under threshold), return which of the scope's dedup keys are now
/// STALE and must be deleted. The active level's own key is never returned here —
/// it is upserted (refreshed) separately.
///   None        -> both keys stale (clear the scope entirely)
///   Exceeded    -> the approaching key is stale
///   Approaching -> the exceeded key is stale (a downgrade)
pub fn stale_limit_keys(level: Option<LimitLevel>) -> Vec<LimitKey> {
    match level {
        None => vec![LimitKey::Exceeded, LimitKey::Approaching],
        Some(LimitLevel::Exceeded) => vec![LimitKey::Approaching],
        Some(LimitLevel::Approaching) => vec![LimitKey::Exceeded],
    }
}

/// Advance a fire time by one cadence step (daily/weekly/monthly; default daily).
pub fn advance(from: DateTime<Utc>, cadence: &str) -> DateTime<Utc> {
    let days = match cadence {
        "weekly" => 7,
        "monthly" => 30,
        _ => 1,
    };
    from + Duration::days(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn level_none_without_limit() {
        assert_eq!(limit_level(500.0, None), None);
    }

    #[test]
    fn level_none_for_nonpositive_limit() {
        assert_eq!(limit_level(50.0, Some(0.0)), None);
    }

    #[test]
    fn level_none_below_80pct() {
        assert_eq!(limit_level(50.0, Some(100.0)), None);
    }

    #[test]
    fn level_approaching_at_80pct() {
        assert_eq!(limit_level(80.0, Some(100.0)), Some(LimitLevel::Approaching));
    }

    #[test]
    fn level_exceeded_only_above_limit() {
        // spend strictly OVER the limit is Exceeded
        assert_eq!(limit_level(120.0, Some(100.0)), Some(LimitLevel::Exceeded));
        assert_eq!(limit_level(100.01, Some(100.0)), Some(LimitLevel::Exceeded));
    }

    #[test]
    fn level_at_limit_not_exceeded() {
        // spend EQUAL to the limit is at-limit/approaching, never Exceeded (#193)
        assert_eq!(limit_level(100.0, Some(100.0)), Some(LimitLevel::Approaching));
    }

    #[test]
    fn stale_keys_none_clears_both() {
        let mut got = stale_limit_keys(None);
        got.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(got, vec![LimitKey::Approaching, LimitKey::Exceeded]);
    }

    #[test]
    fn stale_keys_exceeded_clears_approaching() {
        assert_eq!(stale_limit_keys(Some(LimitLevel::Exceeded)), vec![LimitKey::Approaching]);
    }

    #[test]
    fn stale_keys_approaching_clears_exceeded() {
        // Approaching clears the (now higher-severity) exceeded key — this removal is
        // what the async layer reads as a "downgrade" to suppress re-surfacing.
        assert_eq!(stale_limit_keys(Some(LimitLevel::Approaching)), vec![LimitKey::Exceeded]);
    }

    #[test]
    fn advance_steps() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(advance(t, "daily"), t + Duration::days(1));
        assert_eq!(advance(t, "weekly"), t + Duration::days(7));
        assert_eq!(advance(t, "monthly"), t + Duration::days(30));
        assert_eq!(advance(t, "bogus"), t + Duration::days(1));
    }
}
