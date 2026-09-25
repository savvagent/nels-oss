-- Stripe subscription billing (#25). One row per user (UNIQUE user_id via PK).
-- The Stripe Customer is created lazily at first checkout and persisted
-- immediately; stripe_subscription_id/status stay NULL until the first
-- subscription webhook event. Entitlement state is synced ONLY by webhooks.
CREATE TABLE IF NOT EXISTS subscriptions (
    user_id                UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    stripe_customer_id     TEXT NOT NULL,
    stripe_subscription_id TEXT UNIQUE,
    status                 TEXT,
    price_id               TEXT,
    current_period_end     TIMESTAMPTZ,
    cancel_at_period_end   BOOLEAN NOT NULL DEFAULT FALSE,
    -- event.created of the last applied Stripe event; used to drop out-of-order
    -- / duplicate webhook deliveries.
    last_stripe_event_at   TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The webhook resolves a user from the Stripe customer id on subscription events.
-- UNIQUE enforces one Stripe customer per user at the data layer (persist_upsert
-- UPDATEs by stripe_customer_id), making the invariant structural rather than only
-- code-enforced by ensure_customer's ON CONFLICT (user_id).
CREATE UNIQUE INDEX IF NOT EXISTS subscriptions_stripe_customer_id_idx
    ON subscriptions (stripe_customer_id);
