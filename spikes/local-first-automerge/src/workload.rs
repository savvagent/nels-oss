//! Week 2b: a realistic household, generated deterministically, so the two
//! transaction layouts can be measured on exactly the same history.
//!
//! Shape (per the plan): 5 years × ~150 transactions a month, 30 categories,
//! 2 members on 3 devices, with edit churn. Each member's bank account is
//! imported by one of their devices once a day, as one batch; about 75% of
//! bank rows are categorized by a rule in a second change and the rest by a
//! person a few days later. On top of that: recategorizations (some of them
//! months later, which touches old month docs), amount corrections, deletes,
//! notes, manual entries from the phones, and monthly limit changes. Devices
//! sync every few days.
//!
//! The generator never makes two devices write the same key concurrently
//! (each transaction's later edits belong to one device, and only once every
//! device has seen the row). Conflicts are Week 1's subject; here they would
//! only make the two layouts resolve LWW differently and spoil the
//! comparison. The one deliberate exception is [`Workload::go_offline`].
//!
//! The random stream does not depend on the layout, so the same `Spec` with a
//! different layout performs the same operations.

// Money literals are cents grouped as dollars_cents: `180_00` is $180.00.
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeSet;

use automerge::ReadDoc;
use chrono::{Duration, NaiveDate, TimeZone, Utc};

use crate::harness::Harness;
use crate::model::{self, BankRow, BudgetMeta, Cents, NewTxn, TxnLayout};
use crate::SimClock;

pub const BUDGET: &str = "household";
pub const DEVICES: [&str; 3] = ["alice-laptop", "alice-phone", "bob-phone"];
/// Which device imports which bank account.
const IMPORTERS: [(&str, &str, f64); 2] = [
    ("alice-laptop", "acct-alice-checking", 0.6),
    ("bob-phone", "acct-bob-card", 0.4),
];
const PHONES: [&str; 2] = ["alice-phone", "bob-phone"];

const CATEGORY_NAMES: [&str; 30] = [
    "Groceries",
    "Dining Out",
    "Coffee",
    "Rent",
    "Electricity",
    "Water",
    "Internet",
    "Phone",
    "Fuel",
    "Car Insurance",
    "Car Maintenance",
    "Transit",
    "Health Insurance",
    "Pharmacy",
    "Doctor",
    "Gym",
    "Streaming",
    "Books",
    "Clothing",
    "Household Supplies",
    "Home Repair",
    "Gifts",
    "Charity",
    "Travel",
    "Kids Activities",
    "Childcare",
    "Pets",
    "Haircuts",
    "Subscriptions",
    "Miscellaneous",
];

/// (merchant, category index, typical amount in cents, has an auto rule)
const MERCHANTS: [(&str, usize, Cents, bool); 40] = [
    ("SAFEWAY #1432", 0, 84_00, true),
    ("TRADER JOE'S #551", 0, 62_00, true),
    ("COSTCO WHSE #0112", 0, 180_00, true),
    ("WHOLE FOODS MKT", 0, 45_00, true),
    ("CHIPOTLE 2291", 1, 14_00, true),
    ("OLIVE GARDEN", 1, 58_00, false),
    ("DOORDASH*THAI BASIL", 1, 32_00, false),
    ("LOCAL TAQUERIA", 1, 21_00, false),
    ("STARBUCKS STORE 8812", 2, 6_50, true),
    ("BLUE BOTTLE COFFEE", 2, 5_75, false),
    ("PARKSIDE APTS RENT", 3, 1_850_00, true),
    ("CITY POWER & LIGHT", 4, 96_00, true),
    ("MUNICIPAL WATER", 5, 41_00, true),
    ("COMCAST XFINITY", 6, 79_99, true),
    ("VERIZON WIRELESS", 7, 110_00, true),
    ("SHELL OIL 5738", 8, 48_00, true),
    ("CHEVRON 0091", 8, 52_00, true),
    ("GEICO AUTO", 9, 132_00, true),
    ("JIFFY LUBE #221", 10, 79_00, false),
    ("CLIPPER TRANSIT", 11, 20_00, true),
    ("BLUE CROSS PREMIUM", 12, 412_00, true),
    ("CVS PHARMACY #2211", 13, 23_00, false),
    ("VALLEY MEDICAL GRP", 14, 40_00, false),
    ("PLANET FITNESS", 15, 24_99, true),
    ("NETFLIX.COM", 16, 15_49, true),
    ("SPOTIFY USA", 16, 10_99, true),
    ("AMAZON KINDLE", 17, 9_99, false),
    ("UNIQLO USA", 18, 64_00, false),
    ("TARGET 00012345", 19, 57_00, false),
    ("HOME DEPOT #6601", 20, 88_00, false),
    ("ETSY.COM", 21, 35_00, false),
    ("RED CROSS DONATION", 22, 50_00, false),
    ("UNITED AIRLINES", 23, 420_00, false),
    ("AIRBNB", 23, 310_00, false),
    ("YMCA YOUTH PROGRAMS", 24, 85_00, true),
    ("BRIGHT HORIZONS", 25, 1_200_00, true),
    ("CHEWY.COM", 26, 48_00, true),
    ("GREAT CLIPS", 27, 22_00, true),
    ("APPLE.COM/BILL", 28, 2_99, true),
    ("VENMO PAYMENT", 29, 40_00, false),
];

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub months: u32,
    pub txns_per_month: u32,
    pub layout: TxnLayout,
    pub sync_every_days: i64,
    pub seed: u64,
}

impl Spec {
    /// The plan's household: 5 years, ~150 transactions a month.
    pub fn five_years(layout: TxnLayout) -> Self {
        Self {
            months: 60,
            txns_per_month: 150,
            layout,
            sync_every_days: 3,
            seed: 566,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub bank_rows: u64,
    pub manual: u64,
    pub rule_categorized: u64,
    pub user_categorized: u64,
    pub recategorized: u64,
    /// Edits to a row more than 45 days old: they change an old month doc.
    pub late_edits: u64,
    pub amount_edits: u64,
    pub deletes: u64,
    pub notes: u64,
    pub limit_changes: u64,
    pub fund_toggles: u64,
}

impl Counts {
    pub fn txns(&self) -> u64 {
        self.bank_rows + self.manual
    }
}

pub fn category_id(i: usize) -> String {
    format!("cat-{i:02}")
}

/// Category ids made funds at creation (#228).
pub const FUNDS: [usize; 3] = [10, 20, 23];

#[derive(Debug, Clone)]
enum Edit {
    Categorize(String),
    Recategorize(String),
    Amount(Cents),
    Delete,
    Note,
}

#[derive(Debug, Clone)]
struct Pending {
    due: NaiveDate,
    id: String,
    date: NaiveDate,
    edit: Edit,
}

pub struct Workload {
    pub h: Harness,
    pub spec: Spec,
    pub counts: Counts,
    /// The day the next `step` simulates.
    pub today: NaiveDate,
    rng: Rng,
    provider_seq: u64,
    manual_seq: u64,
    pending: Vec<Pending>,
    offline: BTreeSet<String>,
}

pub const START: (i32, u32, u32) = (2021, 1, 1);

pub fn start_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(START.0, START.1, START.2).unwrap()
}

impl Workload {
    /// Create the household (budget, 30 categories, 3 funds) and sync it to
    /// every device. Nothing has happened yet.
    pub fn new(spec: Spec) -> Self {
        let start = start_date();
        let clock = SimClock::ymd(START.0, START.1, START.2);
        let mut h = Harness::new();
        for d in DEVICES {
            h.add_device(d, clock);
        }
        let meta = BudgetMeta {
            layout: spec.layout,
            ..BudgetMeta::monthly("Household")
        };
        let owner = h.device_mut(DEVICES[0]);
        model::create_budget(owner, BUDGET, &meta);
        let mut rng = Rng(spec.seed);
        for (i, name) in CATEGORY_NAMES.iter().enumerate() {
            let limit = 50_00 + rng.below(40) as Cents * 25_00;
            model::add_category(owner, BUDGET, &category_id(i), name, limit);
        }
        for i in FUNDS {
            model::set_fund(owner, BUDGET, &category_id(i), true);
        }
        for d in &DEVICES[1..] {
            model::open_budget(h.device_mut(d), BUDGET);
        }
        let mut w = Self {
            h,
            spec,
            counts: Counts::default(),
            today: start,
            rng,
            provider_seq: 0,
            manual_seq: 0,
            pending: Vec::new(),
            offline: BTreeSet::new(),
        };
        w.sync();
        w
    }

    /// Generate the whole history the spec describes.
    pub fn run(spec: Spec) -> Self {
        let mut w = Self::new(spec);
        let end = add_months(w.today, spec.months);
        w.run_until(end);
        w.sync();
        w
    }

    pub fn run_until(&mut self, end: NaiveDate) {
        while self.today < end {
            self.step();
        }
    }

    pub fn run_days(&mut self, days: i64) {
        let end = self.today + Duration::days(days);
        self.run_until(end);
    }

    /// The device keeps working (manual entries, its share of edits) but
    /// neither syncs nor imports from the bank until [`Self::go_online`].
    pub fn go_offline(&mut self, dev: &str) {
        self.offline.insert(dev.to_string());
        self.h.go_offline(dev);
    }

    pub fn go_online(&mut self, dev: &str) {
        self.offline.remove(dev);
        self.h.go_online(dev);
    }

    /// Sync every online device to quiescence, joining newly listed docs.
    pub fn sync(&mut self) {
        sync_household(&mut self.h, BUDGET);
    }

    fn online(&self, dev: &str) -> bool {
        !self.offline.contains(dev)
    }

    fn step(&mut self) {
        let day = self.today;
        let noon = Utc.from_utc_datetime(&day.and_hms_opt(12, 0, 0).unwrap());
        for d in DEVICES {
            self.h.device_mut(d).clock = SimClock::at(noon);
        }
        let first_day = day == start_date();
        if chrono::Datelike::day(&day) == 1 && !first_day {
            self.month_start();
        }
        for (dev, account, share) in IMPORTERS {
            if self.online(dev) {
                self.import(dev, account, share);
            }
        }
        self.manual_entry();
        self.apply_due_edits();
        self.today = day + Duration::days(1);
        let elapsed = (self.today - start_date()).num_days();
        if elapsed % self.spec.sync_every_days == 0 {
            self.sync();
        }
    }

    /// The owner adjusts two limits every month and flips a fund twice a year.
    fn month_start(&mut self) {
        let owner = DEVICES[0];
        for _ in 0..2 {
            let cat = category_id(self.rng.below(30) as usize);
            let limit = 50_00 + self.rng.below(40) as Cents * 25_00;
            model::set_limit(self.h.device_mut(owner), BUDGET, &cat, limit);
            self.counts.limit_changes += 1;
        }
        if chrono::Datelike::month(&self.today) % 6 == 1 {
            let cat = category_id(FUNDS[self.rng.below(FUNDS.len() as u64) as usize]);
            let on = self.rng.chance(0.5);
            model::set_fund(self.h.device_mut(owner), BUDGET, &cat, on);
            self.counts.fund_toggles += 1;
        }
    }

    fn import(&mut self, dev: &str, account: &str, share: f64) {
        let mean = self.spec.txns_per_month as f64 * 0.85 * share / 30.44;
        let n = self.rng.around(mean);
        if n == 0 {
            return;
        }
        let mut rows = Vec::new();
        for _ in 0..n {
            self.provider_seq += 1;
            let (merchant, cat, typical, rule) =
                MERCHANTS[self.rng.below(MERCHANTS.len() as u64) as usize];
            // Card rows post a day or three after the purchase, so some rows
            // imported on the 1st belong to last month's doc.
            let lag = Duration::days(self.rng.below(3) as i64);
            let date = (self.today - lag).max(start_date());
            let amount = self.rng.amount(typical);
            rows.push((
                format!("ptx_{:08}", self.provider_seq),
                date,
                amount,
                merchant,
                cat,
                rule,
            ));
        }
        let bank: Vec<BankRow> = rows
            .iter()
            .map(|(p, date, amount, merchant, _, _)| BankRow {
                external_account_id: account,
                provider_transaction_id: p,
                date: *date,
                amount: *amount,
                description: merchant,
            })
            .collect();
        let device = self.h.device_mut(dev);
        let ids = model::import_bank_batch(device, BUDGET, &bank);
        self.counts.bank_rows += ids.len() as u64;

        // Rules run right after the import, one change per doc.
        let mut by_doc: std::collections::BTreeMap<String, Vec<(String, String)>> =
            Default::default();
        for (id, (_, date, _, _, cat, rule)) in ids.iter().zip(&rows) {
            if *rule && self.rng.chance(0.95) {
                let doc = model::open_txns(self.h.device_mut(dev), BUDGET, *date);
                by_doc
                    .entry(doc)
                    .or_default()
                    .push((id.clone(), category_id(*cat)));
                self.counts.rule_categorized += 1;
            } else {
                let due = self.today + Duration::days(1 + self.rng.below(6) as i64);
                self.schedule(id, *date, due, Edit::Categorize(category_id(*cat)));
            }
            self.churn(id, *date);
        }
        for (doc, assignments) in by_doc {
            model::set_categories(self.h.device_mut(dev), &doc, &assignments);
        }
    }

    fn manual_entry(&mut self) {
        let mean = self.spec.txns_per_month as f64 * 0.15 / 30.44;
        for _ in 0..self.rng.around(mean) {
            let dev = PHONES[self.rng.below(2) as usize];
            self.manual_seq += 1;
            let id = format!("manual-{:06}", self.manual_seq);
            let (merchant, cat, typical, _) =
                MERCHANTS[self.rng.below(MERCHANTS.len() as u64) as usize];
            let amount = self.rng.amount(typical);
            let cat = category_id(cat);
            model::add_txn(
                self.h.device_mut(dev),
                BUDGET,
                &NewTxn {
                    id: &id,
                    date: self.today,
                    amount,
                    description: merchant,
                    category: Some(&cat),
                },
            )
            .unwrap();
            self.counts.manual += 1;
            self.churn(&id, self.today);
        }
    }

    /// Schedule the later edits a row attracts.
    fn churn(&mut self, id: &str, date: NaiveDate) {
        let r = &mut self.rng;
        let mut plan = Vec::new();
        if r.chance(0.10) {
            // Most recategorizations happen within weeks; one in five is a
            // clean-up months (up to a year) later, in an old month doc.
            let delay = if r.chance(0.2) {
                45 + r.below(320)
            } else {
                2 + r.below(18)
            };
            let cat = category_id(r.below(30) as usize);
            plan.push((delay, Edit::Recategorize(cat)));
        }
        if r.chance(0.03) {
            let fix = r.amount(40_00);
            plan.push((1 + r.below(5), Edit::Amount(fix)));
        }
        if r.chance(0.01) {
            plan.push((1 + r.below(10), Edit::Delete));
        }
        if r.chance(0.02) {
            plan.push((1 + r.below(10), Edit::Note));
        }
        for (delay, edit) in plan {
            let due = self.today + Duration::days(delay as i64);
            self.schedule(id, date, due, edit);
        }
    }

    fn schedule(&mut self, id: &str, date: NaiveDate, due: NaiveDate, edit: Edit) {
        self.pending.push(Pending {
            due,
            id: id.to_string(),
            date,
            edit,
        });
    }

    /// Apply every due edit on the one device that owns that row's edits.
    /// An edit whose row hasn't reached that device yet waits, so the edit is
    /// always causally after the row's creation (and its rule category).
    fn apply_due_edits(&mut self) {
        let today = self.today;
        let (due, later): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| p.due <= today);
        self.pending = later;
        for p in due {
            let dev = editor_of(&p.id);
            if !self.holds_row(dev, &p) {
                self.pending.push(p);
                continue;
            }
            if (today - p.date).num_days() > 45 {
                self.counts.late_edits += 1;
            }
            let device = self.h.device_mut(dev);
            match &p.edit {
                Edit::Categorize(c) => {
                    model::set_txn_category(device, BUDGET, p.date, &p.id, c);
                    self.counts.user_categorized += 1;
                }
                Edit::Recategorize(c) => {
                    model::set_txn_category(device, BUDGET, p.date, &p.id, c);
                    self.counts.recategorized += 1;
                }
                Edit::Amount(a) => {
                    model::edit_amount(device, BUDGET, p.date, &p.id, *a);
                    self.counts.amount_edits += 1;
                }
                Edit::Delete => {
                    model::delete_txn(device, BUDGET, p.date, &p.id);
                    self.counts.deletes += 1;
                }
                Edit::Note => {
                    model::set_note(device, BUDGET, p.date, &p.id, "split with roommate");
                    self.counts.notes += 1;
                }
            }
        }
    }

    fn holds_row(&self, dev: &str, p: &Pending) -> bool {
        let device = self.h.device(dev);
        let doc_id = format!(
            "{}{}",
            model::txns_prefix(BUDGET),
            self.spec.layout.doc_key(p.date)
        );
        device.has_doc(&doc_id) && {
            let doc = device.doc(&doc_id);
            doc.get(model::map(doc, "date"), p.id.as_str())
                .unwrap()
                .is_some()
        }
    }
}

/// Every transaction's later edits are made on ONE device, chosen from its id,
/// so no two devices ever edit a row concurrently.
fn editor_of(id: &str) -> &'static str {
    let h = id
        .bytes()
        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
    DEVICES[(h % DEVICES.len() as u64) as usize]
}

/// Sync every online device to quiescence, joining any transactions doc the
/// budget's `periods` index lists (the same loop as `tests/common::sync`).
pub fn sync_household(h: &mut Harness, budget_id: &str) {
    loop {
        h.sync_all();
        let mut joined = 0;
        for id in h.device_ids() {
            if h.device(&id).is_online() {
                joined += model::join_listed_periods(h.device_mut(&id), budget_id);
            }
        }
        if joined == 0 {
            return;
        }
    }
}

pub fn add_months(d: NaiveDate, months: u32) -> NaiveDate {
    d.checked_add_months(chrono::Months::new(months)).unwrap()
}

/// SplitMix64: tiny, deterministic, and good enough for workload shape.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// A whole number between 0 and `2 × mean`, averaging `mean`.
    fn around(&mut self, mean: f64) -> u64 {
        (self.unit() * 2.0 * mean).round() as u64
    }

    /// 50%–150% of a typical amount, to the cent.
    fn amount(&mut self, typical: Cents) -> Cents {
        (typical as f64 * (0.5 + self.unit())).round() as Cents
    }
}
