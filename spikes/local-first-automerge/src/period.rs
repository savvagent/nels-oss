//! Calendar periods, copied (not imported) from `backend/src/budget.rs`
//! (`next_period_boundary` / `current_period_window`) so the spike uses Nels's
//! real period rules without depending on the backend crate.
//!
//! Periods are identified by their start date. Every function here is a pure
//! function of `(time_frame, instant)`, which is the whole point: anything the
//! server used to *store* on a boundary (auto-renew marker, fund advancement
//! marker) becomes something every device can *compute* identically.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};

/// First instant of the period after the one containing `now`. Same rules as
/// the backend: monthly -> 1st of next month, quarterly -> start of the next
/// fixed quarter, yearly -> Jan 1 next year, anything else -> monthly.
pub fn next_period_boundary(time_frame: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let year = now.year();
    let month = now.month();
    match time_frame {
        "yearly" => Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap(),
        "quarterly" => {
            let q_start_month = ((month - 1) / 3) * 3 + 1;
            if q_start_month == 10 {
                Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap()
            } else {
                Utc.with_ymd_and_hms(year, q_start_month + 3, 1, 0, 0, 0)
                    .unwrap()
            }
        }
        _ => {
            if month == 12 {
                Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap()
            } else {
                Utc.with_ymd_and_hms(year, month + 1, 1, 0, 0, 0).unwrap()
            }
        }
    }
}

/// Start of the period containing `now`.
pub fn period_start(time_frame: &str, now: DateTime<Utc>) -> NaiveDate {
    let (year, month) = (now.year(), now.month());
    let start_month = match time_frame {
        "yearly" => 1,
        "quarterly" => ((month - 1) / 3) * 3 + 1,
        _ => month,
    };
    NaiveDate::from_ymd_opt(year, start_month, 1).unwrap()
}

/// Start of the period containing `date`.
pub fn period_of(time_frame: &str, date: NaiveDate) -> NaiveDate {
    period_start(time_frame, midnight(date))
}

/// Start of the period after the one starting at `start`.
pub fn next_period(time_frame: &str, start: NaiveDate) -> NaiveDate {
    next_period_boundary(time_frame, midnight(start)).date_naive()
}

/// Every period start from `from` (inclusive) up to but excluding the period
/// containing `now`, i.e. every COMPLETED period. This replaces the fund
/// advancement job's marker walk (#228) with a pure enumeration.
pub fn completed_periods(time_frame: &str, from: NaiveDate, now: DateTime<Utc>) -> Vec<NaiveDate> {
    let current = period_start(time_frame, now);
    let mut out = Vec::new();
    let mut p = period_of(time_frame, from);
    while p < current {
        out.push(p);
        p = next_period(time_frame, p);
    }
    out
}

/// Key for a period in document maps: `YYYY-MM-DD` of its start. Sorts
/// chronologically as a string, which the "latest entry at or before p"
/// lookups rely on.
pub fn key(p: NaiveDate) -> String {
    p.format("%Y-%m-%d").to_string()
}

pub fn parse_key(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_else(|e| panic!("bad period key {s:?}: {e}"))
}

pub fn midnight(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}
