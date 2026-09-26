//! Savings-goal progress math, moved from backend/src/goals.rs
//! (spec savvagent/nels-oss#5).

use chrono::NaiveDate;

/// Percentage of a goal completed, clamped to [0, 100] for display.
pub fn goal_percent(current: f64, target: f64) -> f64 {
    if target <= 0.0 {
        return 0.0;
    }
    (current / target * 100.0).clamp(0.0, 100.0)
}

/// Monthly amount needed to cover `remaining` by `target_date`, given `today`.
/// Returns None when nothing remains, there is no target date, or the date is past.
pub fn monthly_needed(remaining: f64, target_date: Option<NaiveDate>, today: NaiveDate) -> Option<f64> {
    if remaining <= 0.0 {
        return None;
    }
    let target = target_date?;
    if target <= today {
        return None;
    }
    let days = (target - today).num_days();
    let months = ((days as f64) / 30.0).ceil().max(1.0);
    Some(remaining / months)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn percent_basic() {
        assert!((goal_percent(50.0, 200.0) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn percent_clamps_over_100() {
        assert!((goal_percent(300.0, 200.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn percent_zero_target_is_zero() {
        assert_eq!(goal_percent(10.0, 0.0), 0.0);
    }

    #[test]
    fn monthly_none_when_achieved() {
        assert_eq!(monthly_needed(0.0, Some(d(2026, 12, 1)), d(2026, 1, 1)), None);
    }

    #[test]
    fn monthly_none_without_date() {
        assert_eq!(monthly_needed(300.0, None, d(2026, 1, 1)), None);
    }

    #[test]
    fn monthly_none_when_date_past() {
        assert_eq!(monthly_needed(300.0, Some(d(2025, 1, 1)), d(2026, 1, 1)), None);
    }

    #[test]
    fn monthly_splits_over_three_months() {
        // 2026-01-01 -> 2026-04-01 is 90 days -> ceil(90/30)=3 months -> 300/3=100
        let v = monthly_needed(300.0, Some(d(2026, 4, 1)), d(2026, 1, 1)).unwrap();
        assert!((v - 100.0).abs() < 1e-9);
    }
}
