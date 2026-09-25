-- Retirement balance sheet (#464). The foundation of the retirement planner
-- epic (#454): the four tables that record what a person owns, what it is made
-- of, and what it was worth over time.
--
-- STYLE NOTE / deliberate deviation from 20260706000000_financial_connections.sql:
-- that migration adds its CHECK and FOREIGN KEY constraints through separate
-- `ALTER TABLE ... ADD CONSTRAINT` statements so they land even if the table
-- already existed from an earlier run of the same not-yet-deployed migration.
-- We do the opposite here and inline every CHECK and FOREIGN KEY into the
-- `CREATE TABLE`. Reason: PostgreSQL 16 has no `ALTER TABLE ... ADD CONSTRAINT
-- IF NOT EXISTS`, so the separate-ALTER style is only re-runnable by accident
-- (it errors with 42710 "constraint already exists" on a second run). Inlining
-- the constraints into a `CREATE TABLE IF NOT EXISTS` makes the whole file
-- genuinely idempotent: a second run is a no-op for every statement. Every
-- `CREATE TABLE` and `CREATE INDEX` below therefore uses `IF NOT EXISTS`.
--
-- CAVEAT ON THAT IDEMPOTENCE, because it bit us twice while writing this file:
-- "a second run is a no-op" is the whole of what inlining buys. It does NOT
-- retrofit a new constraint onto a table that already exists from an EARLIER
-- DRAFT of this same migration -- `CREATE TABLE IF NOT EXISTS` skips the
-- statement wholesale, so the added CHECK or FK simply never appears and the
-- database sits partially upgraded with nothing complaining. Editing an
-- already-applied migration also changes its checksum, and sqlx compares that
-- against `_sqlx_migrations` (`Migrator::run_direct`), so the symptom you
-- actually see is `MigrateError::VersionMismatch` at boot -- which points at
-- the file, not at the missing constraint. Recovery is three statements that
-- must be run TOGETHER:
--
--   DROP TABLE IF EXISTS asset_balance_history, asset_holdings, assets, securities;
--   DROP INDEX IF EXISTS linked_accounts_user_id_id_idx;
--   DELETE FROM _sqlx_migrations WHERE version = 20260728163000;
--
-- Deleting only the `_sqlx_migrations` row is the trap: the re-apply is then a
-- silent no-op over the stale schema and records a fresh checksum for it, so
-- the database looks migrated and is not. None of this reaches production --
-- this migration has never shipped, and a deployed migration must never be
-- edited at all -- but any dev who amends it locally needs the recipe above.
--
-- Money is `DOUBLE PRECISION` throughout, matching every other financial column
-- in this schema. Taxonomies are TEXT + CHECK rather than PG ENUMs, also
-- matching the rest of the schema: adding a value to an ENUM is a schema
-- migration with awkward transactional semantics, whereas widening a CHECK is a
-- plain constraint swap.
--
-- NON-FINITE FLOATS ARE REJECTED AT THE COLUMN LEVEL -- every money column below
-- carries a `..._finite_check`. This is not theoretical tidiness: PostgreSQL's
-- `double precision` genuinely ACCEPTS the literals 'NaN', 'Infinity' and
-- '-Infinity', so without a CHECK a vendor payload or a hand-written UPDATE can
-- park one in a balance column. From there it is silent all the way out: Rust's
-- `NaN <= 0.0` is FALSE, so a NaN sails past ordinary `<= 0.0` guards, `NaN`
-- propagates through every sum, and `serde_json` serializes a non-finite f64 as
-- JSON `null` rather than raising -- the user sees a missing number and no error
-- is logged anywhere. Rejecting at the boundary is the only place this stays
-- cheap.
--
-- The CHECK shape is `col <> 'NaN'::float8 AND col <> 'Infinity'::float8 AND
-- col <> '-Infinity'::float8`, which works because PostgreSQL deliberately
-- departs from IEEE 754 and defines `NaN = NaN` as TRUE (so `NaN <> 'NaN'` is
-- FALSE and the CHECK fails). An IEEE-semantics `col = col` test would NOT
-- work here for the same reason. Verified empirically on PG16: inserting each
-- of the three literals raises 23514, and ordinary finite values including
-- -1e308 insert fine.
--
-- A NULL money value satisfies every one of these CHECKs, since a CHECK that
-- evaluates to NULL is treated as satisfied. That is intended: see the
-- `current_balance` note below on why NULL is a meaningful state.

-- ---------------------------------------------------------------------------
-- (a) Composite-FK target on linked_accounts.
-- ---------------------------------------------------------------------------
-- PostgreSQL requires a FOREIGN KEY's referenced column list to be backed by a
-- unique constraint or unique index. `linked_accounts.id` is already the
-- primary key, but `assets` needs to reference the *pair* `(user_id, id)` (see
-- the `assets_linked_account_same_user_fkey` note below), so the pair needs its
-- own unique index. It is trivially unique because `id` alone already is; the
-- index exists purely to give the composite FK something to point at.
--
-- LOCKING: this is a plain (non-CONCURRENTLY) `CREATE UNIQUE INDEX`, and sqlx
-- runs each migration inside a transaction, so `CREATE INDEX CONCURRENTLY` is
-- not even an option here. It takes a SHARE lock on `linked_accounts` for the
-- duration of the build, which blocks INSERT/UPDATE/DELETE on that table (reads
-- are unaffected). At current row counts -- linked_accounts holds one row per
-- linked bank account across the whole install, i.e. tens of rows -- the build
-- is sub-millisecond and the write stall is negligible. Revisit if that table
-- ever grows by orders of magnitude.
CREATE UNIQUE INDEX IF NOT EXISTS linked_accounts_user_id_id_idx
    ON linked_accounts (user_id, id);

-- ---------------------------------------------------------------------------
-- (b) securities
-- ---------------------------------------------------------------------------
-- A shared, global cache of instrument metadata keyed by the vendor's own
-- security id. There are no per-user rows here and nothing user-identifying:
-- "VTSAX is a mutual fund" is the same fact for every user, so the row is
-- written once and referenced by every holding of it. `provider` is currently
-- constrained to 'plaid' -- the only investment-holdings vendor wired up -- and
-- the CHECK is what forces a conscious widening when a second vendor arrives.
--
-- `ticker`, `name` and `currency` are all nullable because vendors legitimately
-- omit them (unlisted/private instruments, cash sweep positions, money market
-- funds). `security_type` is NOT NULL with a DEFAULT of 'other' so an
-- unclassifiable instrument is stored as an honest "we do not know" rather than
-- being guessed into a bucket -- see the classification note in
-- backend/src/assets.rs.
CREATE TABLE IF NOT EXISTS securities (
    id                     UUID PRIMARY KEY,
    provider               TEXT NOT NULL CHECK (provider IN ('plaid')),
    provider_security_id   TEXT NOT NULL,
    ticker                 TEXT,
    name                   TEXT,
    security_type          TEXT NOT NULL DEFAULT 'other'
                                CHECK (security_type IN ('equity', 'etf', 'mutual_fund',
                                                         'fixed_income', 'cash', 'other')),
    currency               TEXT,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The idempotency key for the securities cache: a re-sync must upsert the
-- existing row for a vendor security rather than accumulate duplicates of it.
-- `provider` is part of the key because two vendors can hand out colliding
-- opaque ids with no shared meaning.
CREATE UNIQUE INDEX IF NOT EXISTS securities_provider_security_idx
    ON securities (provider, provider_security_id);

-- ---------------------------------------------------------------------------
-- (c) assets
-- ---------------------------------------------------------------------------
-- One row per account/holding-container on a person's balance sheet: a 401(k),
-- an IRA, a taxable brokerage, a pension, a piece of real estate.
--
-- WHY user_id AND NOT budget_id -- READ THIS BEFORE "FIXING" IT.
-- Every other financial table in this schema is budget-scoped:
-- `linked_accounts.budget_id NOT NULL`, `goals.budget_id NOT NULL`, and so on.
-- This table deliberately breaks that pattern, and the break is recorded here
-- so nobody "restores consistency" later.
--
-- Budgets are shareable. A shared budget's collaborators pass the per-module
-- `require_edit_or_owner` guards, and those guards are written to grant a
-- collaborator access to *everything beneath the budget* -- that is exactly
-- what sharing a household budget is supposed to mean for categories,
-- transactions and goals. Retirement balances are not that kind of data. If
-- `assets` were budget-scoped, sharing a household budget with a spouse, a
-- roommate, an adult child or a financial coach would silently hand them the
-- full contents of one person's retirement accounts, and the existing guards
-- would consider that correct and authorize it. Scoping to `user_id` means the
-- only key that can reach an asset row is the individual who owns it; there is
-- no share edge into this table at all.
--
-- See #464 and the access-control decision recorded on the epic #454.
-- Consequence for every future query: filter on `user_id`, never on
-- `budget_id`, and never join through `budgets` to reach these rows.
--
-- `owner_member_id` -- NO FOREIGN KEY YET, ON PURPOSE.
-- It will eventually reference `retirement_profiles(id)` from #465 (which
-- household member an asset belongs to). The FK is intentionally absent from
-- BOTH this migration and #465's, and must be added by a standalone third
-- migration, because two independent conditions have to hold at once:
--   (1) ORDERING ON A FRESH DATABASE. sqlx applies migrations in
--       version-sorted order, not merge order: the sort happens once during
--       source resolution (sqlx-core-0.7.4 src/migrate/source.rs:137,
--       `migrations.sort_by_key(|(m, _)| m.version)`), and `Migrator::iter()`
--       merely walks that already-sorted vector. #465 occupies slot
--       20260728093000, which sorts BEFORE this file's 20260728163000, so on a
--       fresh database #465's migration runs first and `assets` does not exist
--       yet -- an FK declared there would fail outright. Symmetrically, an FK
--       declared here would fail on any database where #465 has not yet run.
--       The FK migration must therefore sort strictly AFTER both
--       20260728093000 and 20260728163000.
--   (2) MERGE ORDER IS NOT RELEASE ORDER. Deploys in this repo are
--       release-gated: merging a feature PR does not deploy it, and the release
--       PR can bundle either or both. So we cannot reason "#465 merged first,
--       therefore its table exists". The FK migration must live in a PR that
--       merges after BOTH #464 and #465 are on main -- a standalone third PR
--       owned by neither issue.
-- Until then `owner_member_id` is a plain UUID column with an index, and
-- referential integrity for it is the application's problem.
--
-- `linked_account_id` is nullable: a manually entered asset (a pension, a house)
-- has no linked account, and a synced asset can also end up NULL via the
-- ON DELETE SET NULL below.
--
-- `balance_as_of` is nullable rather than defaulting to now(): a freshly created
-- manual asset with no stated valuation date should read as "no timestamp",
-- not as "valued this instant".
--
-- `current_balance` is nullable for EXACTLY THE SAME REASON, and the two must
-- stay in step -- a balance and the date it was taken on are one fact, and it
-- would be incoherent for the date to admit "never reported" while the number
-- could not. `NOT NULL DEFAULT 0` would conflate "nobody has reported a balance
-- for this asset yet" with "the custodian reported a balance of zero", which are
-- different facts with different consequences. Concretely: a freshly synced
-- asset that has holdings but whose balance has not arrived would read as 0, and
-- `assets::allocate_reconciled` would compute `residual = 0 - holdings_total`,
-- i.e. a large NEGATIVE residual, which that module's contract defines as "the
-- reported balance looks stale, surface it or trigger a refresh". Every new
-- asset would fire that false signal. With NULL there is nothing to reconcile
-- against and the code says so: `allocate_reconciled(holdings, None)` is plain
-- `allocate(holdings)` with a zero residual.
--
-- `status` is 'active'/'closed' -- closing an account never deletes the row, so
-- history and `asset_balance_history` survive, matching the disconnect-not-
-- delete policy `linked_accounts` already follows.
CREATE TABLE IF NOT EXISTS assets (
    id                  UUID PRIMARY KEY,

    -- ON DELETE CASCADE -- the DELETE POLICY specifically, distinct from the
    -- user-scoping rationale above. Deleting a user must remove that person's
    -- balance sheet in its entirety: there is no legitimate reason to retain
    -- someone's retirement holdings after their account is gone. The cascade
    -- CHAINS -- `asset_holdings.asset_id` and `asset_balance_history.asset_id`
    -- are themselves ON DELETE CASCADE onto `assets` -- so removing the user
    -- removes the assets, which removes the holdings and the balance history.
    -- That chain is what lets `account.rs::delete_user_data` stay complete
    -- WITHOUT naming any of these three new tables: it deletes the `users` row
    -- and the database takes care of the rest. If a future table hangs off
    -- `assets` with anything other than CASCADE, that property quietly breaks
    -- and `delete_user_data` has to be revisited.
    user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    owner_member_id     UUID,
    linked_account_id   UUID,
    name                TEXT NOT NULL,

    -- WHY THESE VALUES AND NOT `401k`/`ira`: this is a COUNTRY-NEUTRAL WRAPPER
    -- taxonomy. It names the KIND of container an asset is, never a national
    -- account product. The geography decision recorded on the epic #454 forbids
    -- baking US constants into the schema, and `401k`, `ira`, `roth_ira`,
    -- `403b`, `457b`, `sep_ira` are US tax-code names that mean nothing to a
    -- UK ISA/SIPP, a Canadian RRSP/TFSA, an Australian super fund or a New
    -- Zealand KiwiSaver -- all of which this product already has bank-linking
    -- connectors for.
    --
    -- The US specifics live in `tax_treatment` instead, and the two axes are
    -- deliberately ORTHOGONAL: a 401(k) is `(retirement_account, pre_tax)`, a
    -- Roth IRA is `(retirement_account, roth)`, a taxable brokerage is
    -- `(brokerage, taxable)`. Collapsing them into one column would require a
    -- roughly twenty-value US-only enum that still could not express a
    -- non-US wrapper. Kept orthogonal, adding a country means widening
    -- `tax_treatment` alone and leaving this list untouched.
    asset_type          TEXT NOT NULL
                             CHECK (asset_type IN ('retirement_account', 'brokerage', 'cash',
                                                   'pension', 'annuity', 'real_estate', 'other')),

    -- WHY THIS IS NOT NULLABLE AND NOT DEFERRED TO A LATER MIGRATION: per
    -- decision 5 on the epic #454, an asset's tax treatment cannot be added
    -- afterwards. It is not derivable from anything else on the row -- not from
    -- the balance, not from the institution, not from the vendor payload -- so
    -- backfilling it later means going back to every user and re-asking them
    -- account by account, which is exactly the interrogation the decision
    -- exists to avoid. It is also the input every downstream calculation in
    -- #467 needs, since pre-tax, Roth and taxable dollars are not worth the
    -- same amount in retirement.
    --
    -- TEXT + CHECK rather than a PG ENUM so the value set can be widened by a
    -- plain `DROP CONSTRAINT` / `ADD CONSTRAINT` pair -- the same swap
    -- `linked_accounts.provider` has already been through four times as each
    -- new bank-linking vendor landed.
    tax_treatment       TEXT NOT NULL
                             CHECK (tax_treatment IN ('pre_tax', 'roth', 'taxable', 'hsa', 'other')),
    institution_name    TEXT,

    -- NULLABLE, NO DEFAULT -- "never reported" is a distinct state from
    -- "reported as zero". See the long note above.
    current_balance     DOUBLE PRECISION,

    currency            TEXT NOT NULL DEFAULT 'USD',
    balance_as_of       TIMESTAMPTZ,
    is_manual           BOOLEAN NOT NULL DEFAULT FALSE,
    status              TEXT NOT NULL DEFAULT 'active'
                             CHECK (status IN ('active', 'closed')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- A manually entered asset must not claim a linked account. Note this is
    -- deliberately ONE-DIRECTIONAL (`NOT is_manual OR linked_account_id IS
    -- NULL`) and NOT the biconditional `is_manual = (linked_account_id IS
    -- NULL)`. The biconditional would be wrong: the ON DELETE SET NULL below
    -- can strip `linked_account_id` from a genuinely *synced* asset whose
    -- linked account row was deleted, leaving `is_manual = FALSE` with a NULL
    -- `linked_account_id` -- a legitimate state the biconditional would reject,
    -- turning an unrelated `DELETE FROM linked_accounts` into a constraint
    -- violation.
    CONSTRAINT assets_manual_has_no_linked_account_check
        CHECK (NOT is_manual OR linked_account_id IS NULL),

    -- `currency` was the one taxonomy column here with no CHECK, which left
    -- 'usd', 'dollars' and '' all storable and indistinguishable from a real
    -- code to any consumer. ISO 4217 alpha-3 is exactly three uppercase
    -- letters, so the shape is checkable even though the full code list is not
    -- worth pinning in a constraint that would need a migration every time ISO
    -- publishes a change. Shape-only is the right strictness: it catches the
    -- casing and free-text mistakes a writer actually makes, without turning a
    -- legitimate rarely-seen code into a failed insert.
    CONSTRAINT assets_currency_format_check
        CHECK (currency ~ '^[A-Z]{3}$'),

    -- See the non-finite note at the top of this file. NULL satisfies this.
    CONSTRAINT assets_current_balance_finite_check
        CHECK (current_balance <> 'NaN'::float8
           AND current_balance <> 'Infinity'::float8
           AND current_balance <> '-Infinity'::float8),

    -- THE SAME-USER INVARIANT. Referencing the *pair* `(user_id,
    -- linked_account_id)` rather than just `linked_account_id` makes "an
    -- asset's linked account belongs to the same user as the asset" an
    -- invariant the DATABASE enforces, not one each writer has to remember.
    --
    -- The failure this prevents is specific and easy to write by accident:
    -- every existing sync path in this codebase derives ownership from the
    -- linked account's *budget*. If #468's holdings sync follows that same
    -- familiar pattern, it would resolve an asset's owner through the budget
    -- and, on a household budget shared by two people, file one member's
    -- retirement holdings under the other member's `user_id`. Nothing
    -- downstream would ever notice: `WHERE user_id = $1` would then serve the
    -- wrong person's retirement account to the wrong person, faithfully and
    -- silently, and the whole point of the user-scoping above would be lost.
    -- With this FK that INSERT simply fails.
    --
    -- MATCH SIMPLE is PostgreSQL's default for multi-column foreign keys, and
    -- it means the constraint is NOT checked at all when any referenced column
    -- is NULL. Here that is exactly the behaviour we want: a manual asset (or
    -- one whose linked account was deleted) has `linked_account_id IS NULL` and
    -- is simply exempt, with no need for a partial constraint or a trigger.
    --
    -- ON DELETE SET NULL (linked_account_id) -- the COLUMN-LIST form, not plain
    -- `ON DELETE SET NULL`. Plain SET NULL nulls *every* column in the FK's
    -- local column list, which here includes `user_id`; since `user_id` is NOT
    -- NULL, deleting a linked_accounts row would abort with a not-null
    -- violation instead of orphaning the asset cleanly. The column-list form
    -- nulls only `linked_account_id` and leaves `user_id` intact. It requires
    -- PostgreSQL 15 or newer; production and local dev are both PG16.
    CONSTRAINT assets_linked_account_same_user_fkey
        FOREIGN KEY (user_id, linked_account_id)
        REFERENCES linked_accounts (user_id, id)
        ON DELETE SET NULL (linked_account_id)
);

-- The hot path is "every asset for this user", so `user_id` gets its own index.
CREATE INDEX IF NOT EXISTS assets_user_id_idx ON assets (user_id);
-- Supports the reverse lookup (which asset does this linked account feed?) and,
-- more importantly, keeps `DELETE FROM linked_accounts` from sequential-scanning
-- `assets` to apply the SET NULL above.
CREATE INDEX IF NOT EXISTS assets_linked_account_id_idx ON assets (linked_account_id);
-- Supports per-household-member breakdowns once #465's profiles exist.
CREATE INDEX IF NOT EXISTS assets_owner_member_id_idx ON assets (owner_member_id);

-- ---------------------------------------------------------------------------
-- (d) asset_holdings
-- ---------------------------------------------------------------------------
-- What an asset is actually made of: one row per security held inside it. This
-- is what feeds the allocation split in backend/src/assets.rs.
--
-- ON DELETE CASCADE on `asset_id`: a holding has no meaning without its asset,
-- and holdings are re-derivable from the vendor on the next sync, so deleting
-- the asset should take them with it.
--
-- ON DELETE RESTRICT on `security_id` -- THE ONLY `ON DELETE RESTRICT` IN THIS
-- SCHEMA, called out explicitly as a deviation. Everywhere else we use CASCADE
-- (child data that is meaningless alone) or SET NULL (history that must
-- survive). Neither fits here: `securities` is a shared, global metadata cache
-- with no per-user rows, and nothing in the product ever deletes from it. If
-- some future code path tries to, that is a bug, and RESTRICT surfaces it
-- immediately as a failed DELETE rather than either silently shredding every
-- user's holdings (CASCADE) or leaving unattributable holdings behind
-- (SET NULL, which would also require making `security_id` nullable).
--
-- `cost_basis` is nullable because vendors very often omit it -- it depends on
-- lot-level history the aggregator may never have seen. A NULL here means
-- "unknown", and callers must not treat it as zero.
CREATE TABLE IF NOT EXISTS asset_holdings (
    id            UUID PRIMARY KEY,
    asset_id      UUID NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    security_id   UUID NOT NULL REFERENCES securities(id) ON DELETE RESTRICT,
    quantity      DOUBLE PRECISION NOT NULL,
    cost_basis    DOUBLE PRECISION,
    market_value  DOUBLE PRECISION NOT NULL,
    as_of         TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- See the non-finite note at the top of this file. `market_value` is the
    -- column that matters most: it is summed directly into every allocation
    -- bucket, so a single NaN here turns an entire portfolio's allocation into
    -- NaN and every fraction derived from it into JSON `null`.
    CONSTRAINT asset_holdings_market_value_finite_check
        CHECK (market_value <> 'NaN'::float8
           AND market_value <> 'Infinity'::float8
           AND market_value <> '-Infinity'::float8),
    CONSTRAINT asset_holdings_quantity_finite_check
        CHECK (quantity <> 'NaN'::float8
           AND quantity <> 'Infinity'::float8
           AND quantity <> '-Infinity'::float8)
);

-- The idempotency key for holdings sync: an asset holds a given security once,
-- so a re-sync UPSERTs quantity/market_value on the existing row instead of
-- appending a second copy and double-counting the position.
CREATE UNIQUE INDEX IF NOT EXISTS asset_holdings_asset_security_idx
    ON asset_holdings (asset_id, security_id);

-- ---------------------------------------------------------------------------
-- (e) asset_balance_history
-- ---------------------------------------------------------------------------
-- One balance snapshot per asset per day, the series behind growth charts and
-- the projection work in #467. `as_of` is a DATE, not a TIMESTAMPTZ: the
-- product question is "what was this worth on that day", and a date makes the
-- one-row-per-day rule below expressible as a plain unique index.
--
-- ON DELETE CASCADE: unlike transaction history, these snapshots are pure
-- derived measurements of one asset and are meaningless once that asset is
-- gone, so they follow it. Note that *closing* an asset sets `status =
-- 'closed'` and deletes nothing, so the ordinary end-of-life path preserves the
-- entire series.
CREATE TABLE IF NOT EXISTS asset_balance_history (
    id          UUID PRIMARY KEY,
    asset_id    UUID NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    as_of       DATE NOT NULL,
    balance     DOUBLE PRECISION NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- See the non-finite note at the top of this file. A NaN anywhere in this
    -- series would poison every chart and every growth calculation drawn from
    -- it, and would do so silently.
    CONSTRAINT asset_balance_history_balance_finite_check
        CHECK (balance <> 'NaN'::float8
           AND balance <> 'Infinity'::float8
           AND balance <> '-Infinity'::float8)
);

-- One snapshot per asset per day. Re-running a sync on the same day must
-- overwrite that day's balance (ON CONFLICT DO UPDATE), not append a second
-- point that would put two values on the same x-coordinate of the chart.
CREATE UNIQUE INDEX IF NOT EXISTS asset_balance_history_asset_as_of_idx
    ON asset_balance_history (asset_id, as_of);
