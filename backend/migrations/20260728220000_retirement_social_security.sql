-- Social Security input for the retirement planner (nels#466, epic #454).
--
-- Extends the `retirement_profiles` table #465 created. Deliberately an ALTER
-- rather than a second table: these are six more attributes of the same
-- household member, keyed by exactly the same `(user_id, member_ordinal)`, and a
-- side table would buy nothing but a join.
--
-- NO `IF NOT EXISTS` AND NO `DO $$ ... IF NOT EXISTS` GUARDS ANYWHERE IN THIS
-- FILE, matching #464's and #500's headers. sqlx's `_sqlx_migrations` ledger is
-- what makes a migration run exactly once; a conditional guard adds nothing on
-- top of that and takes something away, because a guard that silently skips
-- leaves a constraint ABSENT while the migration reports success. A migration
-- that cannot fail loudly cannot tell you it did not do its job.
--
-- `retirement_profiles_member_unique`, the UNIQUE (user_id, member_ordinal)
-- constraint, is deliberately LEFT ALONE. The thing that depends on it is
-- `upsert_profile`'s `ON CONFLICT (user_id, member_ordinal)` — that clause needs
-- a unique index on exactly those columns and silently has no arbiter without
-- one. (#500's composite FK on `assets.owner_member_id` does NOT depend on it:
-- that change creates its own `(user_id, id)` index and points the FK there.
-- Do not cite #500 as the reason this constraint must stay.)
--
-- WHAT IS STORED, AND WHAT IS NOT
--
-- Per #454 decision 1 — binding — the user ENTERS the figure printed on their
-- own Social Security statement. Nels does not estimate a benefit from earnings
-- history: AIME/PIA needs 35 years of indexed earnings, Plaid and Stripe FC
-- return bank transactions rather than W-2 wage records, and SSA publishes no
-- consumer data API. The statement already prints the answer at 62, at full
-- retirement age, and at 70. What Nels computes is the DERIVATIVE — what that
-- figure becomes at a different claiming age — which lives in
-- `backend/src/social_security.rs` as pure functions.
--
-- ABSENT IS NOT ZERO. Every column below is NULLABLE and carries NO SQL DEFAULT.
-- A user who has not entered a Social Security figure has NULL here, not 0, and
-- that distinction is carried all the way out through `Option<f64>` in Rust to a
-- JSON `null` on the API and an explicit `benefit_entered: false` flag. #469
-- renders a missing figure as an explicitly labelled ABSENT segment of the
-- stacked income bar; a zero segment would read as "you get nothing", which is
-- both alarming and wrong.
--
-- NO SQL DEFAULT is also the #465 pattern for the assumption columns, and for
-- the same reason: the named Rust constants are the sole source of truth, and a
-- DEFAULT here would be a second one that drifts.
--
-- UNITS
--   ss_monthly_benefit        -- DOLLARS PER MONTH, as printed on the statement.
--                                Not annual. Not cents.
--   ss_benefit_at_age_months  -- MONTHS from birth (744 = 62y, 840 = 70y): the
--                                age the entered figure is quoted at.
--   ss_claiming_age_months    -- MONTHS from birth: the age they intend to claim.
--
-- WHY MONTHS AND NOT WHOLE YEARS. Full retirement age is 66y2m through 66y10m
-- for the 1955-1959 cohorts and 65y2m through 65y10m for 1938-1942 — ten birth
-- years whose FRA is not a whole number. The FRA figure is the most prominent
-- number on an SSA statement, so a whole-year column would have no correct
-- representation for it: a 1957 user storing "67" understates their implied PIA
-- by about 3% and storing "66" overstates it by about 3%, silently, forever. The
-- API still accepts a whole age (or the literal "fra") and converts.
--
-- WHY AN ANCHOR COLUMN SITS BESIDE EACH MONTHS COLUMN. Storing only the resolved
-- month count loses WHY it has that value, and the two anchors age differently.
-- A user born 1957 who says "claim at my FRA" resolves to 798 months. If a later
-- turn corrects their `birth_date` to 1960 — a routine fix, which is exactly why
-- the birth-date input is lenient — their FRA becomes 804 and the stored 798
-- silently stops meaning "at FRA" and starts meaning "six months early", quietly
-- applying a 3.33% reduction to a benefit they asked to receive in full. So the
-- anchor is persisted, and `resolve_input` RE-DERIVES the month count from the
-- merged birth date on every write where the anchor is 'fra'. An 'explicit'
-- anchor is a number the user actually said and is carried forward untouched.
--
-- ss_source EXISTS SO A LATER ESTIMATOR IS A CHECK WIDENING, NOT A MIGRATION.
-- v1 has exactly one legal value, 'user_entered', and no caller can set it — the
-- Rust write path derives it. If Nels ever does produce its own figure, adding
-- 'estimated' costs one line in this CHECK rather than a new column on a table
-- that by then has rows. Same TEXT+CHECK style as `linked_accounts.provider` and
-- `assets.tax_treatment`.

ALTER TABLE retirement_profiles
    ADD COLUMN ss_monthly_benefit       DOUBLE PRECISION,
    ADD COLUMN ss_benefit_at_age_months INT,
    ADD COLUMN ss_benefit_at_age_anchor TEXT,
    ADD COLUMN ss_claiming_age_months   INT,
    ADD COLUMN ss_claiming_age_anchor   TEXT,
    ADD COLUMN ss_source                TEXT;

-- Data-layer invariants. Each is `IS NULL OR ...` so that ABSENT stays legal —
-- these constrain the shape of a value that IS present and never force one to
-- exist. `validate_profile` enforces the same bounds in Rust with a message
-- naming the field and its scale; these exist so a hand-written INSERT or a
-- future writer cannot bypass them.
--
-- 744..840 is 62..70 years. Below 62 Social Security cannot be claimed at all,
-- and above 70 no further credit accrues, so a value outside the range is a
-- misunderstanding rather than an unusual choice.
ALTER TABLE retirement_profiles
    ADD CONSTRAINT retirement_profiles_ss_benefit_at_age_check
        CHECK (ss_benefit_at_age_months IS NULL
               OR (ss_benefit_at_age_months >= 744 AND ss_benefit_at_age_months <= 840)),

    ADD CONSTRAINT retirement_profiles_ss_claiming_age_check
        CHECK (ss_claiming_age_months IS NULL
               OR (ss_claiming_age_months >= 744 AND ss_claiming_age_months <= 840)),

    ADD CONSTRAINT retirement_profiles_ss_benefit_at_age_anchor_check
        CHECK (ss_benefit_at_age_anchor IS NULL
               OR ss_benefit_at_age_anchor IN ('fra', 'explicit')),

    ADD CONSTRAINT retirement_profiles_ss_claiming_age_anchor_check
        CHECK (ss_claiming_age_anchor IS NULL
               OR ss_claiming_age_anchor IN ('fra', 'explicit')),

    -- An anchor and its resolved month count are one fact in two columns; either
    -- both are present or neither is. Without this a row could carry an 'fra'
    -- anchor with no months (unreadable) or months with no anchor (unrefreshable
    -- when the birth date changes, which is the defect the anchor exists to fix).
    ADD CONSTRAINT retirement_profiles_ss_benefit_anchor_pairing_check
        CHECK ((ss_benefit_at_age_months IS NULL) = (ss_benefit_at_age_anchor IS NULL)),

    ADD CONSTRAINT retirement_profiles_ss_claiming_anchor_pairing_check
        CHECK ((ss_claiming_age_months IS NULL) = (ss_claiming_age_anchor IS NULL)),

    ADD CONSTRAINT retirement_profiles_ss_source_check
        CHECK (ss_source IS NULL OR ss_source IN ('user_entered')),

    -- A dollar figure with no age attached cannot be renormalized to any other
    -- claiming age, so it is not a usable input — it is a half-entered one. The
    -- reverse is fine: an age with no figure yet is a user part-way through.
    ADD CONSTRAINT retirement_profiles_ss_benefit_needs_age_check
        CHECK (ss_monthly_benefit IS NULL OR ss_benefit_at_age_months IS NOT NULL),

    -- `ss_source` records the PROVENANCE of a benefit figure, so it exists
    -- exactly when the figure does.
    ADD CONSTRAINT retirement_profiles_ss_source_pairing_check
        CHECK ((ss_monthly_benefit IS NULL) = (ss_source IS NULL)),

    -- Non-negative. There is no upper bound in SQL: the plausibility ceiling
    -- lives in Rust as a named constant with a message that names the scale,
    -- following #465's MAX_PLAUSIBLE_* pattern, so there is one source of truth
    -- for it rather than a constraint and a constant that drift.
    ADD CONSTRAINT retirement_profiles_ss_benefit_sign_check
        CHECK (ss_monthly_benefit IS NULL OR ss_monthly_benefit >= 0),

    -- FINITE, and this is NOT implied by the sign check above.
    --
    -- `double precision` genuinely accepts the literals 'NaN', 'Infinity' and
    -- '-Infinity', and PostgreSQL sorts NaN as GREATER THAN every number — so
    -- `'NaN'::float8 >= 0` is TRUE and sails straight through the sign check.
    -- The consequence here is a silent one: a NaN benefit is rejected by
    -- `social_security::claiming_adjustment`, whose error `social_security_view`
    -- discards with `.ok()`, so `adjusted_monthly_benefit` quietly becomes null
    -- and the user is told nothing rather than shown a wrong number. Refusing
    -- the value at the data layer is the honest place to stop it.
    --
    -- The CHECK shape mirrors #464's `assets.current_balance` exactly, including
    -- the `<> 'NaN'` spelling: PostgreSQL departs from IEEE 754 and defines
    -- `NaN = NaN` as TRUE, so `col <> 'NaN'::float8` really does exclude NaN.
    ADD CONSTRAINT retirement_profiles_ss_benefit_finite_check
        CHECK (ss_monthly_benefit IS NULL
               OR (ss_monthly_benefit <> 'NaN'::float8
                   AND ss_monthly_benefit <> 'Infinity'::float8
                   AND ss_monthly_benefit <> '-Infinity'::float8));
