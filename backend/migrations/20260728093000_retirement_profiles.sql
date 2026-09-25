-- Retirement profile & assumptions (nels#465, part of the #454 epic).
--
-- Deliberately a NEW TABLE, not columns on `users`. This is a correction to
-- #454's own decomposition text, which said "needs a users/profile migration".
-- Two reasons:
--   1. `users` has already accreted single-purpose columns through four separate
--      column-adding `ALTER TABLE users` migrations; adding eleven more
--      planner-only columns to the row every authenticated request loads makes
--      that worse.
--   2. Columns on `users` cannot express the per-household-member shape #454
--      decision 6 requires — one user may eventually have two retirement
--      members, and a row-per-member is the only shape that does not become a
--      table restructure later.
--
-- UNIQUE (user_id, member_ordinal) exists precisely so that a second household
-- member is a NEW ROW rather than a schema change. v1 only ever writes
-- member_ordinal = 1, and no surface (REST or chat) can create a second; the
-- constraint is here now so the future addition is additive. Joint-household
-- behaviour is deliberately deferred.
--
-- `country` is EXPLICIT and USER-SET, never inferred from linked accounts
-- (#454 decision 2). A user can legitimately link accounts in several countries,
-- and the frontend's `selectedCountry` is transient UI state that is never
-- persisted. This column is the only place in the database that records where a
-- USER lives, and the US-only region gate for the whole planner reads from it.
-- A non-US profile still SAVES; the gate is a rendering gate, not a write gate.
--
-- The assumption columns carry NO SQL DEFAULT on purpose. The named Rust
-- constants in `backend/src/retirement.rs` are the single source of truth for
-- 65 / 5.0 / 2.5 / 90 / 0.75, and duplicating those numbers here would invite
-- silent drift between the two. The Rust write path always supplies a value.
-- `member_ordinal` keeps its DEFAULT 1 because it is structural, not an
-- assumption.
--
-- UNITS -- load-bearing, because #467's projection math is wrong by a factor of
-- 100 if any of these is misread:
--   contribution_rate_pre_tax  PERCENT of gross      (6.0 = 6%)
--   contribution_rate_roth     PERCENT of gross      (3.0 = 3%)
--   expected_real_return       PERCENT per year, REAL not nominal (5.0 = 5%)
--   inflation_rate             PERCENT per year      (2.5 = 2.5%)
--   target_replacement_ratio   FRACTION of gross     (0.75 = 75%)
--   target_retirement_age      whole years
--   life_expectancy_age        whole years
--   current_gross_income       absolute annual dollars
--   employer_match_formula     JSONB {"tiers":[{"employee_pct_up_to":PERCENT,
--                              "match_pct":PERCENT}],"annual_dollar_cap":DOLLARS|null}
--
-- `id UUID PRIMARY KEY` has no DB default: UUIDs are generated in Rust, matching
-- the goals/goal_contributions precedent.
-- ON DELETE CASCADE so deleting an account removes the profile; account.rs also
-- issues an explicit DELETE for symmetry with the `subscriptions` precedent.
-- #464 will add `assets.owner_member_id` referencing retirement_profiles(id).

CREATE TABLE IF NOT EXISTS retirement_profiles (
    id                        UUID PRIMARY KEY,
    user_id                   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    member_ordinal            INT NOT NULL DEFAULT 1,
    country                   TEXT NOT NULL,
    birth_date                DATE NOT NULL,
    target_retirement_age     INT NOT NULL,
    current_gross_income      DOUBLE PRECISION NOT NULL,
    contribution_rate_pre_tax DOUBLE PRECISION NOT NULL,
    contribution_rate_roth    DOUBLE PRECISION NOT NULL,
    employer_match_formula    JSONB NOT NULL,
    expected_real_return      DOUBLE PRECISION NOT NULL,
    inflation_rate            DOUBLE PRECISION NOT NULL,
    life_expectancy_age       INT NOT NULL,
    target_replacement_ratio  DOUBLE PRECISION NOT NULL,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT retirement_profiles_member_unique UNIQUE (user_id, member_ordinal),
    CONSTRAINT retirement_profiles_member_ordinal_check CHECK (member_ordinal >= 1),
    -- ISO 3166-1 alpha-2, uppercase, as a data-layer invariant. The Rust write
    -- path uppercases and trims before binding.
    CONSTRAINT retirement_profiles_country_check CHECK (country ~ '^[A-Z]{2}$')
);

CREATE INDEX IF NOT EXISTS retirement_profiles_user_id_idx ON retirement_profiles (user_id);
