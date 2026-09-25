// Pure, framework-free helpers for the Settings subscription section (two-tier,
// Plan C). The backend /billing/subscription response includes:
//   { status, tier, payment_warning, is_pro, price_id, cancel_at_period_end, ... }
// where `tier` is "none" | "basic" | "pro".
//
// Why not use `is_pro`? `is_pro` is the legacy status-only flag (active|trialing)
// that predates the tier split. It (a) can't tell Basic from Pro, and (b) is FALSE
// for `past_due` — so the old UI hid the "Manage" button from exactly the users in
// payment grace who need it to update their card. These helpers key off `tier` and
// `payment_warning` instead.

/**
 * Whether the user holds an entitled plan (Basic or Pro), so the plan label and
 * the "Manage" (Stripe billing portal) button should render. True during the
 * `past_due` grace period too (tier stays Basic/Pro there), unlike `is_pro`.
 */
export function hasEntitledPlan(subscription) {
  const tier = subscription?.tier;
  return tier === "basic" || tier === "pro";
}

/**
 * i18n key for the plan label. A trial is always Pro; otherwise the label follows
 * the resolved tier. Falls back to the free-plan key when there's no entitled plan.
 */
export function planLabelKey(subscription) {
  if (subscription?.status === "trialing") return "settings.planProTrial";
  if (subscription?.tier === "pro") return "settings.planPro";
  if (subscription?.tier === "basic") return "settings.planBasic";
  return "settings.planFree";
}

/** Whether to surface the "update your payment method" nudge (past_due grace). */
export function needsPaymentUpdate(subscription) {
  return subscription?.payment_warning === true;
}
