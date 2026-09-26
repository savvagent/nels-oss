//! Period windows, renewal boundaries and project spans (#47, #48, #51).

use chrono::{DateTime, Datelike, TimeZone, Utc};

/// The full lifetime span of a project budget — from creation until it is
/// closed (or now if still open). Returns `(created_at, closed_at.unwrap_or(now))`.
pub fn project_span(
    created_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    (created_at, closed_at.unwrap_or(now))
}

/// The half-open `[start, end)` UTC window of the period IMMEDIATELY BEFORE the
/// period containing `now`, used to source the previous period's spend for
/// per-budget rollover (issue #47). Periods are anchored to the calendar:
/// `monthly` -> the previous calendar month, `yearly` -> the previous calendar
/// year, `quarterly` -> the previous fixed quarter (Q1=Jan-Mar, Q2=Apr-Jun,
/// Q3=Jul-Sep, Q4=Oct-Dec). Any unrecognized `time_frame` falls back to
/// `monthly`.
pub fn previous_period_window(
    time_frame: &str,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let year = now.year();
    let month = now.month();

    match time_frame {
        "yearly" => {
            let end = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();
            let start = Utc.with_ymd_and_hms(year - 1, 1, 1, 0, 0, 0).unwrap();
            (start, end)
        }
        "quarterly" => {
            // First calendar month of the quarter containing `now` (1, 4, 7, 10).
            let q_start_month = ((month - 1) / 3) * 3 + 1;
            // The current quarter starts here; that's also where the previous
            // quarter ends (half-open).
            let end = Utc.with_ymd_and_hms(year, q_start_month, 1, 0, 0, 0).unwrap();
            // Previous quarter begins three months earlier; underflow from Q1
            // (Jan) rolls back to Q4 (Oct) of the prior year.
            let (start_year, start_month) = if q_start_month == 1 {
                (year - 1, 10)
            } else {
                (year, q_start_month - 3)
            };
            let start = Utc
                .with_ymd_and_hms(start_year, start_month, 1, 0, 0, 0)
                .unwrap();
            (start, end)
        }
        // "monthly" and any unrecognized value.
        _ => {
            // End of the previous month is the first of the current month.
            let end = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).unwrap();
            // Previous month start; underflow from January rolls to December.
            let (start_year, start_month) = if month == 1 {
                (year - 1, 12)
            } else {
                (year, month - 1)
            };
            let start = Utc
                .with_ymd_and_hms(start_year, start_month, 1, 0, 0, 0)
                .unwrap();
            (start, end)
        }
    }
}

/// The UTC start of the period IMMEDIATELY AFTER the period containing `now` —
/// i.e. the next period boundary, used by auto-renew (#51) to schedule a budget's
/// next renewal and to advance the marker once a period has elapsed. Anchored to
/// the calendar exactly like `previous_period_window`: `monthly` -> first of next
/// month, `quarterly` -> first month of the next fixed quarter (Q1=Jan, Q2=Apr,
/// Q3=Jul, Q4=Oct), `yearly` -> Jan 1 of next year. Any unrecognized `time_frame`
/// falls back to `monthly`.
///
/// Because the boundary is computed relative to `now` (not a stale stored
/// marker), renewing after the server was down across several boundaries jumps
/// straight to the next FUTURE boundary in a single advance — no catch-up loop.
pub fn next_period_boundary(time_frame: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let year = now.year();
    let month = now.month();

    match time_frame {
        "yearly" => Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap(),
        "quarterly" => {
            // First calendar month of the quarter containing `now` (1, 4, 7, 10).
            let q_start_month = ((month - 1) / 3) * 3 + 1;
            // The next quarter begins three months later; overflow from Q4 (Oct)
            // rolls forward to Q1 (Jan) of the next year.
            if q_start_month == 10 {
                Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap()
            } else {
                Utc.with_ymd_and_hms(year, q_start_month + 3, 1, 0, 0, 0)
                    .unwrap()
            }
        }
        // "monthly" and any unrecognized value.
        _ => {
            // First of next month; overflow from December rolls to January.
            if month == 12 {
                Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap()
            } else {
                Utc.with_ymd_and_hms(year, month + 1, 1, 0, 0, 0).unwrap()
            }
        }
    }
}

/// The `next_renewal_at` marker to persist for a budget given its auto-renew
/// state: `Some(next boundary)` when auto-renew is on, `None` when off. Keeping
/// this in one place ensures the REST and chat paths compute the marker
/// identically (enable -> schedule next boundary; disable -> clear). (#51)
pub fn renewal_marker(
    auto_renew: bool,
    time_frame: &str,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if auto_renew {
        Some(next_period_boundary(time_frame, now))
    } else {
        None
    }
}

/// The half-open `[start, end)` UTC window of the period CONTAINING `now` — i.e.
/// the CURRENT period — used to source the current period's spend (#52 rollup
/// aggregation surfaces a parent's combined current-period spend). It is exactly
/// the gap between the previous-period window's end and the next period boundary,
/// so it is anchored to the calendar identically to `previous_period_window` /
/// `next_period_boundary` (monthly/quarterly/yearly; unrecognized -> monthly).
/// Defined in terms of those two helpers so the three windows can never drift.
pub fn current_period_window(
    time_frame: &str,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    // previous_period_window's `end` is the start of the current period; the next
    // boundary is the start of the following period (the current period's end).
    let (_, current_start) = previous_period_window(time_frame, now);
    let current_end = next_period_boundary(time_frame, now);
    (current_start, current_end)
}

/// The `[start, end)` spend-window a rollup SOURCE budget contributes through,
/// keyed off the source's OWN `budget_type` (never the parent's) — a project
/// source's full lifetime span, or a time_based source's current calendar
/// period. Shared by `linked_budgets_spent` (the aggregate combined-spend
/// figure) and `category_table_rows`'s mirror-row resolution (nels#298) so the
/// two "what window does this source's mirror represent" answers cannot drift
/// apart.
pub fn source_spend_window(
    budget_type: &str,
    time_frame: &str,
    created_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    if budget_type == "project" {
        project_span(created_at, closed_at, now)
    } else {
        current_period_window(time_frame, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_period_window_is_between_prev_end_and_next_boundary() {
        // The current period starts where the previous period ends and ends at the
        // next boundary — anchored identically to the other two window helpers (#52).
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();
        for tf in ["monthly", "quarterly", "yearly", "weekly"] {
            let (_, prev_end) = previous_period_window(tf, now);
            let next = next_period_boundary(tf, now);
            let (cur_start, cur_end) = current_period_window(tf, now);
            assert_eq!(cur_start, prev_end, "current start == previous end ({tf})");
            assert_eq!(cur_end, next, "current end == next boundary ({tf})");
            assert!(cur_start <= now && now < cur_end, "now is inside the current window ({tf})");
        }
    }

    #[test]
    fn prev_window_monthly() {
        let now = Utc.with_ymd_and_hms(2026, 3, 14, 12, 0, 0).unwrap();
        let (s, e) = previous_period_window("monthly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_monthly_january_crosses_year() {
        let now = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let (s, e) = previous_period_window("monthly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_yearly() {
        let now = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let (s, e) = previous_period_window("yearly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_quarterly_q1_goes_to_prev_q4() {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap(); // Q1 2026
        let (s, e) = previous_period_window("quarterly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2025, 10, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_quarterly_q2_goes_to_q1() {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 0, 0, 0).unwrap(); // Q2 2026
        let (s, e) = previous_period_window("quarterly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_unknown_timeframe_falls_back_to_monthly() {
        let now = Utc.with_ymd_and_hms(2026, 3, 14, 0, 0, 0).unwrap();
        assert_eq!(
            previous_period_window("weekly", now),
            previous_period_window("monthly", now)
        );
    }

    #[test]
    fn next_period_boundary_monthly() {
        // Mid-month -> first of next month.
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 10, 30, 0).unwrap();
        assert_eq!(
            next_period_boundary("monthly", now),
            Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap()
        );
        // December rolls over to January of the next year.
        let dec = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).unwrap();
        assert_eq!(
            next_period_boundary("monthly", dec),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_period_boundary_quarterly() {
        // Q2 (Apr-Jun) -> start of Q3 (Jul 1).
        let q2 = Utc.with_ymd_and_hms(2026, 5, 15, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("quarterly", q2),
            Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap()
        );
        // Q1 (Jan-Mar) -> start of Q2 (Apr 1).
        let q1 = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("quarterly", q1),
            Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()
        );
        // Q4 (Oct-Dec) -> start of Q1 next year (Jan 1).
        let q4 = Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("quarterly", q4),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_period_boundary_yearly() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("yearly", now),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_period_boundary_unknown_falls_back_to_monthly() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("weekly", now),
            next_period_boundary("monthly", now)
        );
    }

    #[test]
    fn renewal_marker_set_when_enabled_cleared_when_disabled() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        // Disabled -> no marker.
        assert_eq!(renewal_marker(false, "monthly", now), None);
        // Enabled -> the next boundary for the timeframe.
        assert_eq!(
            renewal_marker(true, "monthly", now),
            Some(next_period_boundary("monthly", now))
        );
        assert_eq!(
            renewal_marker(true, "yearly", now),
            Some(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn project_span_open_ends_at_now() {
        let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
        let (start, end) = project_span(created, None, now);
        assert_eq!(start, created);
        assert_eq!(end, now);
    }

    #[test]
    fn project_span_closed_ends_at_closed_at() {
        let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let closed = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
        let (start, end) = project_span(created, Some(closed), now);
        assert_eq!(start, created);
        assert_eq!(end, closed);
    }
}
