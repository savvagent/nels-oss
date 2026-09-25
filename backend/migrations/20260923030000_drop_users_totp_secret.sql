-- nels#554: drop the inert users.totp_secret column. Since nels#551 (WebAuthn
-- passkeys) nothing reads it; new accounts only wrote a placeholder to satisfy
-- the NOT NULL constraint.
ALTER TABLE users DROP COLUMN IF EXISTS totp_secret;
