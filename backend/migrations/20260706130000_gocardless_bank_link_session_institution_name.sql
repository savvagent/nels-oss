-- Code-review fix (nels#320 Task 3): `complete_link_session` was persisting
-- `linked_accounts.institution_name` as the machine institution_id (e.g.
-- "MONZO_MONZ_GB") instead of the bank's real display name (e.g. "Monzo").
-- The institution's display name is already fetched once in
-- `start_link_session` via `list_institutions` — carry it on the pending
-- session row so `complete_link_session` can persist the real name without
-- an extra GoCardless API round-trip.
ALTER TABLE bank_link_sessions ADD COLUMN institution_name TEXT NOT NULL DEFAULT '';
ALTER TABLE bank_link_sessions ALTER COLUMN institution_name DROP DEFAULT;
