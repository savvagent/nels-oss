//! Deterministic, per-device clock.
//!
//! Every device gets its OWN clock rather than sharing one, so a scenario can
//! skew two devices apart (one still in March, the other already in April)
//! when testing period-boundary rules such as fund advancement (#228) and
//! auto-renew (#51). Nothing in the spike may call `Utc::now()` directly.

use chrono::{DateTime, Duration, TimeZone, Utc};

#[derive(Debug, Clone, Copy)]
pub struct SimClock {
    now: DateTime<Utc>,
}

impl SimClock {
    pub fn at(now: DateTime<Utc>) -> Self {
        Self { now }
    }

    /// Midnight UTC on the given date. Panics on an invalid date; this is test
    /// fixture construction, not input handling.
    pub fn ymd(year: i32, month: u32, day: u32) -> Self {
        Self::at(Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap())
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.now
    }

    pub fn set(&mut self, now: DateTime<Utc>) {
        self.now = now;
    }

    pub fn advance(&mut self, by: Duration) {
        self.now += by;
    }
}
