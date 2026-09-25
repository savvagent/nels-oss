// Parse the marketing → app checkout deep-link (the prerendered marketing site
// can't know auth state, so it links back with these params and the app starts
// checkout after login). Two-tier: `?plan=basic|pro&cadence=monthly|annual`
// selects the tier. Legacy (pre-two-tier): `?upgrade=monthly|annual` — cadence
// only, tier left null so the backend defaults it to Pro.
//
// Returns `{ tier, cadence }` when the query carries checkout intent, else null.
// `tier` is "basic" | "pro" | null; `cadence` is always "monthly" | "annual".
export function parseCheckoutDeepLink(search) {
  const p = new URLSearchParams(search);
  const validCadence = (c) => (c === "monthly" || c === "annual" ? c : null);

  const plan = p.get("plan");
  if (plan === "basic" || plan === "pro") {
    return { tier: plan, cadence: validCadence(p.get("cadence")) ?? "monthly" };
  }

  const legacy = validCadence(p.get("upgrade"));
  if (legacy) return { tier: null, cadence: legacy };

  return null;
}
