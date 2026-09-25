// Stripe Financial Connections bank-account linking (#303). Pure helpers are
// exported separately from the Stripe.js-driving flow so they're unit
// testable without mocking the SDK (mirrors this repo's existing
// pure-logic-alongside-*.test.js convention).

/** Normalize a linked-accounts list response to a plain array. */
export function parseLinkedAccountsResponse(data) {
  return Array.isArray(data?.accounts) ? data.accounts : [];
}

/** Whether an error (shaped like `{status}`) represents the Pro-gate (402). */
export function isProGateError(err) {
  return !!err && err.status === 402;
}

/**
 * Drive the full link flow: start a session (unless the caller already has
 * a `clientSecret` — see below), open Stripe.js's
 * collectFinancialConnectionsAccounts modal, then tell our backend the
 * session completed so it can persist the accounts. `fetchApi` and
 * `loadStripe` are injected so this is testable without a real network call
 * or the real Stripe SDK.
 *
 * `clientSecret` is optional: the REST-button flow (LinkedAccounts.svelte)
 * has no session yet and needs this function to create one via
 * `POST /session`. The chat-triggered flow (App.svelte's LINK_BANK_ACCOUNT
 * handling) already receives a client_secret in the chat response — passing
 * it here skips the redundant `/session` POST that would otherwise open a
 * second, wasted Stripe Financial Connections session. This lets both call
 * sites share this one function (#303 code review).
 */
export async function startLinkFlow({
  budgetId, fetchApi, loadStripe, publishableKey, clientSecret,
}) {
  let secret = clientSecret;
  if (!secret) {
    const session = await fetchApi(`/budgets/${budgetId}/linked-accounts/session`, {
      method: "POST",
    });
    secret = session.client_secret;
  }
  const stripe = await loadStripe(publishableKey);
  const result = await stripe.collectFinancialConnectionsAccounts({ clientSecret: secret });
  // Stripe.js resolves this call to a discriminated union: `{error}` on a
  // REAL failure (expired/invalid client_secret, network issue, etc.), with
  // `financialConnectionsSession` left undefined. That must be surfaced
  // distinctly from a plain user-cancel (which also leaves
  // financialConnectionsSession.accounts empty but has no `error`) — code
  // review on #303 caught both being silently swallowed as "no accounts
  // linked" with zero user feedback.
  if (result?.error) {
    // Deliberately no English fallback baked in here — the caller
    // (LinkedAccounts.svelte's `link()`, mirroring App.svelte's handler)
    // owns what to show when Stripe supplies no message, via its own
    // `e.message || $_("linkedAccounts.linkError")` localized fallback.
    throw new Error(result.error.message || "");
  }
  if (!result?.financialConnectionsSession?.accounts?.length) {
    return { linked: [] };
  }
  const response = await fetchApi(`/budgets/${budgetId}/linked-accounts/complete`, {
    method: "POST",
    body: JSON.stringify({ session_id: result.financialConnectionsSession.id }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

export async function fetchLinkedAccounts({ budgetId, fetchApi }) {
  const data = await fetchApi(`/budgets/${budgetId}/linked-accounts`);
  return parseLinkedAccountsResponse(data);
}

export async function refreshLinkedAccount({ budgetId, accountId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/linked-accounts/${accountId}/refresh`, { method: "POST" });
}

export async function disconnectLinkedAccount({ budgetId, accountId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/linked-accounts/${accountId}`, { method: "DELETE" });
}

// --- GoCardless Bank Account Data (#320) ---
// GoCardless's consent flow is redirect-based (no client-side SDK modal like
// Stripe's), so these functions only own the two REST round-trips; the
// actual `window.location.href` navigation and the post-redirect `gc_ref`
// pickup live in the caller (LinkedAccounts.svelte / App.svelte) so this
// stays unit-testable without a real browser navigation.

/** List institutions for a country (GoCardless, #320). */
export async function fetchGcInstitutions({ country, fetchApi }) {
  const data = await fetchApi(`/gocardless/institutions?country=${encodeURIComponent(country)}`);
  return Array.isArray(data) ? data : [];
}

/**
 * Start a GoCardless consent flow: create the session server-side and return
 * its redirect_url — the CALLER does the actual `window.location.href`
 * navigation (kept out of this function so it stays unit-testable without a
 * real browser navigation).
 */
export async function startGcLinkFlow({ budgetId, country, institutionId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/gocardless/session`, {
    method: "POST",
    body: JSON.stringify({ country, institution_id: institutionId }),
  });
}

/** Complete a GoCardless consent flow after the user returns from their bank. */
export async function completeGcLinkFlow({ budgetId, gcRef, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/gocardless/complete`, {
    method: "POST",
    body: JSON.stringify({ gc_ref: gcRef }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

/** Whether a linked account needs the user to reconnect (PSD2 consent expired, #320). */
export function isConsentExpired(account) {
  return account?.status === "consent_expired";
}

// --- Belvo (Mexico, Brazil) (#322) ---
// Belvo's consent flow is a client-side embeddable widget (unlike
// GoCardless's full-page redirect and unlike Stripe's SDK-driven modal) —
// these functions own the two REST round-trips; the widget script
// loading/init is `loadBelvoWidget` below so callers can swap in a fake for
// tests without loading a real external script.

/** Start a Belvo consent flow: mint a widget access token server-side. */
export async function startBelvoLinkFlow({ budgetId, country, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/belvo/session`, {
    method: "POST",
    body: JSON.stringify({ country }),
  });
}

/** Complete a Belvo consent flow after the widget's onSuccess callback fires. */
export async function completeBelvoLinkFlow({ budgetId, sessionId, belvoLinkId, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/belvo/complete`, {
    method: "POST",
    body: JSON.stringify({ session_id: sessionId, belvo_link_id: belvoLinkId }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

// --- Basiq (Australia, #323) ---
// Basiq's consent flow is fully hosted ("Basiq Connect") — Nels never picks
// an institution on its own side (spec Assumption 2), so there is no
// institutions-fetch helper here, unlike GoCardless's fetchGcInstitutions.

/** Start a Basiq consent flow: create the session server-side and return its redirect_url. */
export async function startBasiqLinkFlow({ budgetId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/basiq/session`, { method: "POST" });
}

/** Complete a Basiq consent flow after the user returns from Basiq's hosted page. */
export async function completeBasiqLinkFlow({ budgetId, basiqRef, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/basiq/complete`, {
    method: "POST",
    body: JSON.stringify({ basiq_ref: basiqRef }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

const BELVO_WIDGET_SRC = "https://cdn.belvo.io/belvo-widget-1-stable.js";
// Silent-failure-hunter finding: a browser extension or proxy that silently
// DROPS the script request (neither `onload` nor `onerror` ever fires,
// unlike a normal network failure) would otherwise leave the caller's
// promise — and the "Connecting…" modal gating on it — hanging forever,
// with no way out short of a page reload. This bounds that wait.
const BELVO_SCRIPT_LOAD_TIMEOUT_MS = 15000;

// Module-scoped singleton: a second call before the first resolves reuses
// the same in-flight promise; a call after it resolves is a cheap no-op
// re-injection guard.
let belvoScriptPromise = null;

/**
 * Lazily inject Belvo's widget script once, then initialize the widget with
 * the given access token. `onSuccess(linkId)`/`onExit()` are wired to the
 * widget's own callback names. Exported as a standalone function (rather
 * than inlined in the Svelte component) so it's swappable with a fake in
 * tests without loading the real external script.
 */
export function loadBelvoWidget(accessToken, { onSuccess, onExit } = {}) {
  if (!belvoScriptPromise) {
    belvoScriptPromise = new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = BELVO_WIDGET_SRC;
      const timeoutId = setTimeout(() => {
        reject(new Error("Timed out loading the Belvo widget script"));
      }, BELVO_SCRIPT_LOAD_TIMEOUT_MS);
      script.onload = () => {
        clearTimeout(timeoutId);
        resolve();
      };
      script.onerror = () => {
        clearTimeout(timeoutId);
        reject(new Error("Failed to load Belvo widget script"));
      };
      document.head.appendChild(script);
    }).catch((e) => {
      // Code review finding: a rejected promise is still truthy, so without
      // this the `if (!belvoScriptPromise)` guard above would never retry —
      // one transient CDN failure would permanently break the Belvo flow for
      // the rest of the page session. Clear the cache on failure so the
      // NEXT call gets a fresh attempt, then re-throw so THIS call's caller
      // still sees the failure.
      belvoScriptPromise = null;
      throw e;
    });
  }
  return belvoScriptPromise.then(() => {
    // `belvoSDK` is a global injected by the CDN script above; not
    // available until that script has loaded, hence the promise chain.
    // ASSUMED, not fully verified against a live account (see the spec's §8
    // "Widget DOM-mounting mode is unverified" risk): `createWidget(...).build()`
    // config docs show no container/selector/element option, so this call
    // assumes the widget self-manages its own overlay rather than mounting
    // into a caller-owned DOM node — some of Belvo's OTHER documented
    // integration examples pair the same CDN script with a conventional
    // `<div id="belvo">`, which this programmatic `.build()` call may or may
    // not still require. If the widget never visibly renders in a live
    // sandbox, check that first. (Code review finding on an earlier version
    // of this feature: two Svelte components used to render an inert
    // placeholder `<div>` with mismatched ids that no JS here ever read —
    // removed as dead markup either way, since neither id matched Belvo's
    // own conventional `#belvo` id regardless of which integration mode
    // turns out to be correct.)
    // eslint-disable-next-line no-undef
    belvoSDK.createWidget(accessToken, {
      callback: (link) => onSuccess?.(link),
      onExit: () => onExit?.(),
    }).build();
  });
}

// --- Akahu (New Zealand, #323) ---
// Also fully hosted ("Akahu Connect", OAuth2) — no institution picker needed.

/** Start an Akahu consent flow: return its hosted authorize redirect_url. */
export async function startAkahuLinkFlow({ budgetId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/akahu/session`, { method: "POST" });
}

/** Complete an Akahu consent flow: exchange the returned OAuth `code` for accounts. */
export async function completeAkahuLinkFlow({ budgetId, akahuRef, code, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/akahu/complete`, {
    method: "POST",
    body: JSON.stringify({ akahu_ref: akahuRef, code }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

// --- Plaid Link, Canada (#321) ---
// Plaid ships a CDN script defining a global `Plaid.create(...)` — there is
// no ESM npm package for vanilla (non-React) Plaid Link the way
// @stripe/stripe-js exists for Stripe. loadPlaidLink injects that script
// idempotently and resolves to window.Plaid; startPlaidLinkFlow takes it as
// an INJECTED parameter (default loadPlaidLink itself), mirroring
// startLinkFlow's injected loadStripe parameter, so tests supply a fake
// {create: () => ({open, exit})} without a real script tag or network call.
const PLAID_LINK_SCRIPT_SRC = "https://cdn.plaid.com/link/v2/stable/link-initialize.js";
// Silent-failure-hunter finding (#321): mirrors BELVO_SCRIPT_LOAD_TIMEOUT_MS —
// a browser extension or proxy that silently DROPS the script request (neither
// `onload` nor `onerror` ever fires, unlike a normal network failure) would
// otherwise leave the caller's promise, and any "Connecting…" UI gating on
// it, hanging forever with no way out short of a page reload.
const PLAID_LINK_SCRIPT_LOAD_TIMEOUT_MS = 15000;

// Single shared localStorage key for persisting {linkToken, sessionId,
// budgetId} across a Big-5 institution's OAuth redirect (see
// isPlaidOAuthReturn/startPlaidLinkFlow below). Exported so every
// reader/writer (LinkedAccounts.svelte, App.svelte) imports the same
// constant instead of hand-typing the string — mirrors this codebase's
// other localStorage consumers (theme.js, textsize.js,
// notificationPrefs.js's STORAGE_KEY), which do the same to prevent
// typo/rename drift (code review, #321).
export const PLAID_LINK_RESUME_KEY = "plaidLinkResume";

// Tracks an in-flight script load so overlapping calls (e.g. two components
// mounting at once, or a caller retrying before the first attempt settled)
// reuse the SAME promise instead of injecting a second <script> tag. Reset to
// null on failure so a later call after an error starts a fresh attempt
// rather than being permanently stuck behind a broken script tag.
let plaidLinkLoadPromise = null;

export function loadPlaidLink() {
  if (typeof window !== "undefined" && window.Plaid) {
    return Promise.resolve(window.Plaid);
  }
  if (plaidLinkLoadPromise) {
    return plaidLinkLoadPromise;
  }
  plaidLinkLoadPromise = new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = PLAID_LINK_SCRIPT_SRC;
    const timeoutId = setTimeout(() => {
      script.remove();
      plaidLinkLoadPromise = null;
      reject(new Error("Timed out loading Plaid Link"));
    }, PLAID_LINK_SCRIPT_LOAD_TIMEOUT_MS);
    script.onload = () => {
      clearTimeout(timeoutId);
      // Copilot review finding (#321): `onload` firing only proves the
      // network request succeeded, not that the script actually defined the
      // `Plaid` global (e.g. a CDN serving a truncated/wrong response body
      // with a 200 status). Without this check, the in-flight promise cache
      // would resolve to `undefined` and stay that way (the short-circuit
      // above only re-checks `window.Plaid`, never the cached promise's
      // value), so every subsequent call would keep resolving to
      // `undefined` too, and callers would only find out later with a
      // confusing "Cannot read properties of undefined" crash on
      // `Plaid.create(...)`.
      if (!window.Plaid) {
        script.remove();
        plaidLinkLoadPromise = null;
        reject(new Error("Plaid Link script loaded but did not define window.Plaid"));
        return;
      }
      resolve(window.Plaid);
    };
    script.onerror = () => {
      clearTimeout(timeoutId);
      script.remove();
      plaidLinkLoadPromise = null;
      reject(new Error("Failed to load Plaid Link"));
    };
    document.head.appendChild(script);
  });
  return plaidLinkLoadPromise;
}

/**
 * Drive the full Plaid Link flow: create a link token (unless the caller
 * already has one — the chat-triggered flow receives linkToken/sessionId
 * from the chat response, mirroring startLinkFlow's clientSecret-skip
 * behavior), open Plaid Link's modal, and on success tell our backend to
 * exchange the public_token and persist the accounts.
 *
 * `receivedRedirectUri` is OPTIONAL and must only be supplied when RESUMING
 * a Big-5 institution's OAuth Link session after the browser navigates back
 * (App.svelte's handlePlaidOAuthReturn, passing `window.location.href`).
 * Plaid Link's own docs require this field to be entirely omitted for a
 * normal, non-resuming Link initialization (the REST-button and
 * chat-triggered call sites) — passing it there would confuse Link into
 * thinking a redirect happened, so it's only spread into `Plaid.create`'s
 * argument object when the caller actually supplies it (code review, #321).
 */
export async function startPlaidLinkFlow({
  budgetId, fetchApi, loadPlaidLink: injectedLoadPlaidLink = loadPlaidLink, linkToken, sessionId,
  receivedRedirectUri,
}) {
  let token = linkToken;
  let session = sessionId;
  if (!token) {
    const created = await fetchApi(`/budgets/${budgetId}/plaid/link-token`, { method: "POST" });
    token = created?.link_token;
    session = created?.session_id;
    // Fail fast with a clear message here rather than letting `undefined`
    // silently flow into Plaid.create({token: undefined, ...}) and then into
    // the /plaid/complete body, where it would only surface later as a
    // confusing backend error (code review).
    if (!token) {
      throw new Error("Failed to create Plaid link token");
    }
  }
  // Copilot review finding (#321): the ORIGINAL check above only guarded the
  // freshly-created-token path's `token`, never `session` — so a malformed
  // /plaid/link-token response missing session_id (or a caller passing
  // linkToken without sessionId) would silently omit `session_id` from the
  // /plaid/complete request body below, surfacing only as a confusing
  // backend validation error. Checked here, after both the
  // caller-supplied-token and freshly-created-token paths converge, so it
  // covers both.
  if (!session) {
    throw new Error("Failed to create Plaid link session");
  }

  const Plaid = await injectedLoadPlaidLink();

  return new Promise((resolve, reject) => {
    const handler = Plaid.create({
      token,
      ...(receivedRedirectUri ? { receivedRedirectUri } : {}),
      onSuccess: async (publicToken) => {
        try {
          const response = await fetchApi(`/budgets/${budgetId}/plaid/complete`, {
            method: "POST",
            body: JSON.stringify({ session_id: session, public_token: publicToken }),
          });
          resolve({ linked: parseLinkedAccountsResponse(response) });
        } catch (e) {
          reject(e);
        }
      },
      // Plaid Link's onExit fires on both a plain user-cancel (error === null)
      // and a real failure (error is populated) — these must be distinguished
      // the same way #303's Stripe.js result.error handling is (code review
      // precedent there): a cancel resolves with an empty linked list, a real
      // error rejects so the caller can surface it.
      onExit: (error) => {
        if (error) {
          reject(new Error(error.error_message || ""));
        } else {
          resolve({ linked: [] });
        }
      },
    });
    handler.open();
  });
}

/** Whether an oauth_state_id query param is present — the browser has
 * returned from a Big-5 institution's OAuth redirect (#321) and Plaid Link
 * must be resumed with the SAME link_token used to open the original
 * session (Plaid does not support a new token for this resumption). */
export function isPlaidOAuthReturn(search) {
  return new URLSearchParams(search).has("oauth_state_id");
}

/**
 * Parse a PLAID_LINK_RESUME_KEY localStorage payload. Returns null (rather
 * than throwing) for a missing/malformed value — mirrors
 * notificationPrefs.js's "non-fatal, fall back" convention. The caller
 * (App.svelte's handlePlaidOAuthReturn) is invoked fire-and-forget from
 * onMount, so a raw JSON.parse there would surface as an unhandled promise
 * rejection with zero user feedback if the stored value were ever corrupted
 * or tampered with (code review, #321). The "safe default" here is simply
 * "no resume data" — there is nothing sensible to resume with, so callers
 * should just abandon the resume attempt when this returns null.
 */
export function parsePlaidLinkResume(stored) {
  if (!stored) return null;
  try {
    const parsed = JSON.parse(stored);
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

// ---- Presentation helpers for the Accounts UI (#365) ----
// Pure, framework-free, unit-tested. Icons are referenced by a stable string
// id the component maps to a Lucide component, keeping this file SDK-free.

/** Short relative-time label from two epoch-ms timestamps (e.g. "3h", "2d"). */
export function formatRelativeTime(fromMs, nowMs) {
  if (typeof fromMs !== "number" || Number.isNaN(fromMs)) return "";
  const diff = Math.max(0, nowMs - fromMs);
  const min = Math.floor(diff / 60_000);
  if (min < 60) return `${Math.max(1, min)}m`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h`;
  const day = Math.floor(hr / 24);
  if (day < 7) return `${day}d`;
  if (day < 30) return `${Math.floor(day / 7)}w`;
  if (day < 365) return `${Math.floor(day / 30)}mo`;
  return `${Math.floor(day / 365)}y`;
}

/**
 * Derive an account's sync-freshness state. Only the reachable states are
 * covered: "synced" (last_synced_at present + parseable) and "never".
 * A "failed" kind exists in the UI's vocabulary but the backend exposes no
 * per-account sync-error signal today (status enum is active|disconnected|
 * consent_expired), so it is intentionally never returned here (#365 spec §6).
 */
export function syncStatusFor(account, nowMs) {
  const raw = account?.last_synced_at;
  if (raw) {
    const ms = Date.parse(raw);
    if (!Number.isNaN(ms)) return { kind: "synced", relative: formatRelativeTime(ms, nowMs) };
  }
  return { kind: "never" };
}

/** Badge variant + icon id + i18n label key for an account's status. */
export function statusBadgeFor(account) {
  const status = account?.status;
  if (status === "consent_expired") {
    return { variant: "badge-warning", icon: "alert", labelKey: "linkedAccounts.consentExpiredBadge" };
  }
  if (status === "disconnected") {
    return { variant: "badge-ghost", icon: "x", labelKey: "linkedAccounts.disconnectedBadge" };
  }
  return { variant: "badge-success", icon: "check", labelKey: "linkedAccounts.activeBadge" };
}

/** Up to two uppercase initials from institution_name (fallback display_name). */
export function initialsFor(account) {
  const src = (account?.institution_name || account?.display_name || "").trim();
  if (!src) return "?";
  const words = src.split(/\s+/).filter(Boolean);
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[words.length - 1][0]).toUpperCase();
}

/** "•••• 1234" when last4 is present, else "". */
export function maskedLast4(account) {
  return account?.last4 ? `•••• ${account.last4}` : "";
}

/**
 * Group accounts by institution_name, preserving first-seen order. Each group
 * carries its account list, count, and the OLDEST last_synced_at (ms) among
 * its accounts (null if none have synced) — used for the collapse header.
 */
export function groupAccountsByInstitution(accounts) {
  const list = Array.isArray(accounts) ? accounts : [];
  const groups = [];
  const byKey = new Map();
  for (const acct of list) {
    const key = acct?.institution_name || "";
    let g = byKey.get(key);
    if (!g) {
      g = { key, institutionName: acct?.institution_name ?? null, accounts: [], count: 0, oldestSyncedAtMs: null };
      byKey.set(key, g);
      groups.push(g);
    }
    g.accounts.push(acct);
    g.count += 1;
    const ms = acct?.last_synced_at ? Date.parse(acct.last_synced_at) : NaN;
    if (!Number.isNaN(ms)) {
      g.oldestSyncedAtMs = g.oldestSyncedAtMs == null ? ms : Math.min(g.oldestSyncedAtMs, ms);
    }
  }
  return groups;
}

/** Region grouping order for the add-account country picker. */
export const REGION_ORDER = ["northAmerica", "europe", "latinAmerica", "oceania"];

/**
 * The 14 supported link countries (same set as the legacy <select>), each with
 * a flag emoji and a region. Display names stay English, matching the legacy
 * <select> (they were never i18n-keyed); region headers ARE i18n-keyed.
 */
export const COUNTRY_OPTIONS = [
  { id: "US", name: "United States", flag: "🇺🇸", region: "northAmerica" },
  { id: "CA", name: "Canada", flag: "🇨🇦", region: "northAmerica" },
  { id: "GB", name: "United Kingdom", flag: "🇬🇧", region: "europe" },
  { id: "FR", name: "France", flag: "🇫🇷", region: "europe" },
  { id: "DE", name: "Germany", flag: "🇩🇪", region: "europe" },
  { id: "IT", name: "Italy", flag: "🇮🇹", region: "europe" },
  { id: "ES", name: "Spain", flag: "🇪🇸", region: "europe" },
  { id: "DK", name: "Denmark", flag: "🇩🇰", region: "europe" },
  { id: "FI", name: "Finland", flag: "🇫🇮", region: "europe" },
  { id: "NO", name: "Norway", flag: "🇳🇴", region: "europe" },
  { id: "MX", name: "Mexico", flag: "🇲🇽", region: "latinAmerica" },
  { id: "BR", name: "Brazil", flag: "🇧🇷", region: "latinAmerica" },
  { id: "AU", name: "Australia", flag: "🇦🇺", region: "oceania" },
  { id: "NZ", name: "New Zealand", flag: "🇳🇿", region: "oceania" },
];

/** Case-insensitive filter on country name or ISO code. Empty query → all. */
export function filterCountries(options, query) {
  const q = (query || "").trim().toLowerCase();
  if (!q) return options;
  return options.filter(
    (o) => o.name.toLowerCase().includes(q) || o.id.toLowerCase().includes(q),
  );
}

/**
 * Case-insensitive filter on institution name (mirrors filterCountries for the
 * add-account modal's bank-search box). Empty query → all. Tolerates missing
 * `name` on an institution.
 */
export function filterInstitutions(institutions, query) {
  const list = Array.isArray(institutions) ? institutions : [];
  const q = (query || "").trim().toLowerCase();
  if (!q) return list;
  return list.filter((i) => (i?.name || "").toLowerCase().includes(q));
}

/** Group country options by region in REGION_ORDER, skipping empty regions. */
export function groupCountriesByRegion(options) {
  const byRegion = new Map();
  for (const o of options) {
    if (!byRegion.has(o.region)) byRegion.set(o.region, []);
    byRegion.get(o.region).push(o);
  }
  return REGION_ORDER
    .filter((r) => byRegion.has(r))
    .map((r) => ({ region: r, countries: byRegion.get(r) }));
}

const GC_PICKER_COUNTRIES = new Set(["GB", "FR", "DE", "IT", "ES", "DK", "FI", "NO"]);
const REDIRECT_COUNTRIES = new Set([...GC_PICKER_COUNTRIES, "AU", "NZ"]);
const WIDGET_COUNTRIES = new Set(["MX", "BR"]);

/**
 * CTA + helper copy metadata stating what happens when the user connects,
 * so the button never reads a generic "Connect": redirect (GoCardless/Basiq/
 * Akahu), modal (Stripe US / Plaid CA), or embedded widget (Belvo MX/BR).
 * `bankName` is only available for GoCardless (the one flow with an in-app
 * institution picker); Basiq/Akahu are fully hosted, so they use the generic
 * "Continue to your bank" copy rather than an empty "{bank}".
 */
export function linkCtaMeta(country, { bankName } = {}) {
  if (WIDGET_COUNTRIES.has(country)) {
    return { kind: "widget", ctaKey: "linkedAccounts.ctaConnect", helperKey: "linkedAccounts.widgetHelper", ctaValues: {} };
  }
  if (REDIRECT_COUNTRIES.has(country)) {
    if (GC_PICKER_COUNTRIES.has(country) && bankName) {
      return { kind: "redirect", ctaKey: "linkedAccounts.ctaContinueToBank", helperKey: "linkedAccounts.redirectHelper", ctaValues: { bank: bankName } };
    }
    return { kind: "redirect", ctaKey: "linkedAccounts.ctaContinueGeneric", helperKey: "linkedAccounts.redirectHelper", ctaValues: {} };
  }
  return { kind: "modal", ctaKey: "linkedAccounts.ctaConnect", helperKey: "linkedAccounts.modalHelper", ctaValues: {} };
}
