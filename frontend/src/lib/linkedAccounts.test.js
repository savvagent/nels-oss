import {
  describe, it, expect, vi, beforeEach, afterEach,
} from "vitest";
import {
  parseLinkedAccountsResponse,
  isProGateError,
  startLinkFlow,
  fetchGcInstitutions,
  startGcLinkFlow,
  completeGcLinkFlow,
  isConsentExpired,
  startBelvoLinkFlow,
  completeBelvoLinkFlow,
  loadBelvoWidget,
  startBasiqLinkFlow,
  completeBasiqLinkFlow,
  startAkahuLinkFlow,
  completeAkahuLinkFlow,
  isPlaidOAuthReturn,
  startPlaidLinkFlow,
  parsePlaidLinkResume,
  PLAID_LINK_RESUME_KEY,
  formatRelativeTime,
  syncStatusFor,
  statusBadgeFor,
  initialsFor,
  maskedLast4,
  groupAccountsByInstitution,
  COUNTRY_OPTIONS,
  REGION_ORDER,
  filterCountries,
  filterInstitutions,
  groupCountriesByRegion,
  linkCtaMeta,
} from "./linkedAccounts.js";

const flushPendingPromises = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("parseLinkedAccountsResponse", () => {
  it("returns the accounts array unchanged on a normal response", () => {
    const input = { accounts: [{ id: "1", display_name: "Checking", status: "active" }] };
    expect(parseLinkedAccountsResponse(input)).toEqual(input.accounts);
  });

  it("returns an empty array when accounts is missing", () => {
    expect(parseLinkedAccountsResponse({})).toEqual([]);
  });
});

describe("isProGateError", () => {
  it("recognizes a 402 as a Pro-gate error", () => {
    expect(isProGateError({ status: 402 })).toBe(true);
  });

  it("does not treat other statuses as a Pro-gate error", () => {
    expect(isProGateError({ status: 403 })).toBe(false);
    expect(isProGateError({ status: 500 })).toBe(false);
    expect(isProGateError(null)).toBe(false);
  });
});

describe("startLinkFlow", () => {
  // stripe.collectFinancialConnectionsAccounts resolves to a discriminated
  // union: `{error}` on a REAL failure (expired/invalid client_secret,
  // network issue, etc.) with financialConnectionsSession left undefined.
  // Before this fix, that was indistinguishable from a plain user-cancel
  // (also empty accounts, but no `error`) — both silently fell through to
  // "no accounts linked" with zero user feedback (#303 code review).
  it("throws the Stripe error's message when collectFinancialConnectionsAccounts resolves with an error", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ client_secret: "secret_123" });
    const collectFinancialConnectionsAccounts = vi
      .fn()
      .mockResolvedValue({ error: { message: "Your session has expired." } });
    const loadStripe = vi.fn().mockResolvedValue({ collectFinancialConnectionsAccounts });

    await expect(
      startLinkFlow({ budgetId: "b1", fetchApi, loadStripe, publishableKey: "pk_test" }),
    ).rejects.toThrow("Your session has expired.");

    // Must not proceed to tell the backend the session "completed".
    expect(fetchApi).toHaveBeenCalledTimes(1);
  });

  it("throws with an empty message (no baked-in English) when the Stripe error has none, leaving the localized fallback to the caller", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ client_secret: "secret_123" });
    const collectFinancialConnectionsAccounts = vi.fn().mockResolvedValue({ error: {} });
    const loadStripe = vi.fn().mockResolvedValue({ collectFinancialConnectionsAccounts });

    const thrown = await startLinkFlow({
      budgetId: "b1", fetchApi, loadStripe, publishableKey: "pk_test",
    }).catch((e) => e);

    expect(thrown).toBeInstanceOf(Error);
    expect(thrown.message).toBe("");
  });

  it("returns no linked accounts (without throwing) on a plain user-cancel", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ client_secret: "secret_123" });
    const collectFinancialConnectionsAccounts = vi
      .fn()
      .mockResolvedValue({ financialConnectionsSession: { accounts: [] } });
    const loadStripe = vi.fn().mockResolvedValue({ collectFinancialConnectionsAccounts });

    const result = await startLinkFlow({ budgetId: "b1", fetchApi, loadStripe, publishableKey: "pk_test" });
    expect(result).toEqual({ linked: [] });
    expect(fetchApi).toHaveBeenCalledTimes(1);
  });

  // `/complete` returns `{accounts: [...]}` (matching GET /linked-accounts's
  // shape), not a bare array — a backend fix landed after this was first
  // written. startLinkFlow must parse it the same way parseLinkedAccountsResponse
  // does, not assume the raw response IS the array.
  it("parses the {accounts: [...]} response shape from /complete into a plain array", async () => {
    const completedAccounts = [
      { id: "acc_1", institution_name: "Test Bank", display_name: "Checking", last4: "1234", status: "active", last_synced_at: null },
    ];
    const fetchApi = vi
      .fn()
      .mockResolvedValueOnce({ client_secret: "secret_123" })
      .mockResolvedValueOnce({ accounts: completedAccounts });
    const collectFinancialConnectionsAccounts = vi
      .fn()
      .mockResolvedValue({ financialConnectionsSession: { id: "fcsess_1", accounts: [{ id: "acc_1" }] } });
    const loadStripe = vi.fn().mockResolvedValue({ collectFinancialConnectionsAccounts });

    const result = await startLinkFlow({ budgetId: "b1", fetchApi, loadStripe, publishableKey: "pk_test" });

    expect(result).toEqual({ linked: completedAccounts });
  });

  it("calls POST /session when no clientSecret is provided (REST-button flow)", async () => {
    const fetchApi = vi
      .fn()
      .mockResolvedValueOnce({ client_secret: "secret_123" })
      .mockResolvedValueOnce({ accounts: [] });
    const collectFinancialConnectionsAccounts = vi
      .fn()
      .mockResolvedValue({ financialConnectionsSession: { accounts: [] } });
    const loadStripe = vi.fn().mockResolvedValue({ collectFinancialConnectionsAccounts });

    await startLinkFlow({ budgetId: "b1", fetchApi, loadStripe, publishableKey: "pk_test" });

    expect(fetchApi).toHaveBeenCalledTimes(1);
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/linked-accounts/session", { method: "POST" });
    expect(collectFinancialConnectionsAccounts).toHaveBeenCalledWith({ clientSecret: "secret_123" });
  });

  it("skips POST /session and calls /complete directly when a clientSecret is already provided (chat-triggered flow)", async () => {
    const completedAccounts = [{ id: "acc_1", status: "active" }];
    const fetchApi = vi.fn().mockResolvedValue({ accounts: completedAccounts });
    const collectFinancialConnectionsAccounts = vi
      .fn()
      .mockResolvedValue({ financialConnectionsSession: { id: "fcsess_1", accounts: [{ id: "acc_1" }] } });
    const loadStripe = vi.fn().mockResolvedValue({ collectFinancialConnectionsAccounts });

    const result = await startLinkFlow({
      budgetId: "b1", fetchApi, loadStripe, publishableKey: "pk_test", clientSecret: "secret_from_chat",
    });

    // Exactly one fetchApi call, for /complete only — no redundant /session POST.
    expect(fetchApi).toHaveBeenCalledTimes(1);
    expect(fetchApi).toHaveBeenCalledWith(
      "/budgets/b1/linked-accounts/complete",
      { method: "POST", body: JSON.stringify({ session_id: "fcsess_1" }) },
    );
    expect(collectFinancialConnectionsAccounts).toHaveBeenCalledWith({ clientSecret: "secret_from_chat" });
    expect(result).toEqual({ linked: completedAccounts });
  });
});

describe("GoCardless flow (#320)", () => {
  it("fetchGcInstitutions returns the institutions array", async () => {
    const fetchApi = vi.fn().mockResolvedValue([{ id: "MONZO_MONZ_GB", name: "Monzo" }]);
    const result = await fetchGcInstitutions({ country: "GB", fetchApi });
    expect(result).toEqual([{ id: "MONZO_MONZ_GB", name: "Monzo" }]);
    expect(fetchApi).toHaveBeenCalledWith("/gocardless/institutions?country=GB");
  });

  it("fetchGcInstitutions returns an empty array when the response isn't an array", async () => {
    const fetchApi = vi.fn().mockResolvedValue(null);
    const result = await fetchGcInstitutions({ country: "GB", fetchApi });
    expect(result).toEqual([]);
  });

  it("startGcLinkFlow posts country and institution_id and returns the session", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ redirect_url: "https://ob.gocardless.com/x", reference: "ref-1" });
    const result = await startGcLinkFlow({ budgetId: "b1", country: "GB", institutionId: "MONZO_MONZ_GB", fetchApi });
    expect(result.redirect_url).toBe("https://ob.gocardless.com/x");
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/gocardless/session", {
      method: "POST",
      body: JSON.stringify({ country: "GB", institution_id: "MONZO_MONZ_GB" }),
    });
  });

  it("completeGcLinkFlow posts gc_ref and parses the linked-accounts response", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "gocardless" }] });
    const result = await completeGcLinkFlow({ budgetId: "b1", gcRef: "ref-1", fetchApi });
    expect(result.linked).toEqual([{ id: "a1", provider: "gocardless" }]);
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/gocardless/complete", {
      method: "POST",
      body: JSON.stringify({ gc_ref: "ref-1" }),
    });
  });

  it("isConsentExpired detects the consent_expired status", () => {
    expect(isConsentExpired({ status: "consent_expired" })).toBe(true);
    expect(isConsentExpired({ status: "active" })).toBe(false);
    expect(isConsentExpired(null)).toBe(false);
  });
});

describe("Belvo (#322)", () => {
  it("startBelvoLinkFlow posts the country to the session endpoint", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ access_token: "tok_x", session_id: "sess_1" });
    const result = await startBelvoLinkFlow({ budgetId: "b1", country: "MX", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/belvo/session", {
      method: "POST",
      body: JSON.stringify({ country: "MX" }),
    });
    expect(result.access_token).toBe("tok_x");
  });

  it("completeBelvoLinkFlow posts session_id and belvo_link_id, returns parsed accounts", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "belvo" }] });
    const result = await completeBelvoLinkFlow({ budgetId: "b1", sessionId: "sess_1", belvoLinkId: "link_1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/belvo/complete", {
      method: "POST",
      body: JSON.stringify({ session_id: "sess_1", belvo_link_id: "link_1" }),
    });
    expect(result.linked).toEqual([{ id: "a1", provider: "belvo" }]);
  });

  // This repo's vitest runs `environment: "node"` (no jsdom/testing-library —
  // see every other *.test.js's identical comment), so there is no real
  // `document` global to inject a script into. A minimal fake `document`
  // (just `createElement`/`head.appendChild`) stands in instead, matching
  // this file's own `fetchApi`/`loadStripe` injection convention rather than
  // adding a new jsdom dependency for one test.
  it("loadBelvoWidget injects the CDN script once and calls createWidget with the token", async () => {
    const scriptEl = {};
    const fakeDocument = {
      createElement: vi.fn(() => scriptEl),
      head: { appendChild: vi.fn() },
    };
    // eslint-disable-next-line no-global-assign
    globalThis.document = fakeDocument;
    let capturedCallback;
    // eslint-disable-next-line no-global-assign
    globalThis.belvoSDK = {
      createWidget: vi.fn((token, opts) => {
        capturedCallback = opts;
        return { build: vi.fn() };
      }),
    };
    const onSuccess = vi.fn();
    const loadPromise = loadBelvoWidget("tok_widget", { onSuccess });
    expect(fakeDocument.createElement).toHaveBeenCalledWith("script");
    expect(scriptEl.src).toContain("belvo-widget");
    expect(fakeDocument.head.appendChild).toHaveBeenCalledWith(scriptEl);
    scriptEl.onload();
    await loadPromise;
    expect(globalThis.belvoSDK.createWidget).toHaveBeenCalledWith("tok_widget", expect.any(Object));
    capturedCallback.callback("belvo_link_abc");
    expect(onSuccess).toHaveBeenCalledWith("belvo_link_abc");
    delete globalThis.belvoSDK;
    delete globalThis.document;
  });

  it("loadBelvoWidget retries after a script-load failure instead of caching the rejection forever", async () => {
    // Code review finding: the module-scoped script-load promise must clear
    // itself on failure, or one transient CDN error would permanently break
    // the Belvo flow for the rest of the page session (a rejected promise is
    // still truthy, so a naive `if (!cached)` guard would never retry).
    // vi.resetModules() + a dynamic re-import gets a fresh, unpolluted copy
    // of the module-scoped `belvoScriptPromise` singleton for this test,
    // independent of the successful-load test above.
    vi.resetModules();
    const { loadBelvoWidget: freshLoadBelvoWidget } = await import("./linkedAccounts.js");

    const failingScriptEl = {};
    const fakeDocument = {
      createElement: vi.fn(() => failingScriptEl),
      head: { appendChild: vi.fn() },
    };
    // eslint-disable-next-line no-global-assign
    globalThis.document = fakeDocument;

    const firstAttempt = freshLoadBelvoWidget("tok_retry");
    const firstAttemptFailure = firstAttempt.catch((e) => e);
    failingScriptEl.onerror();
    const firstError = await firstAttemptFailure;
    expect(firstError).toBeInstanceOf(Error);

    const succeedingScriptEl = {};
    fakeDocument.createElement = vi.fn(() => succeedingScriptEl);
    // eslint-disable-next-line no-global-assign
    globalThis.belvoSDK = { createWidget: vi.fn(() => ({ build: vi.fn() })) };

    const secondAttempt = freshLoadBelvoWidget("tok_retry");
    expect(fakeDocument.createElement).toHaveBeenCalledWith("script");
    succeedingScriptEl.onload();
    await secondAttempt;
    expect(globalThis.belvoSDK.createWidget).toHaveBeenCalledWith("tok_retry", expect.any(Object));

    delete globalThis.belvoSDK;
    delete globalThis.document;
  });

  it("loadBelvoWidget rejects (rather than hanging forever) if the script never fires onload or onerror", async () => {
    // Silent-failure-hunter finding: a browser extension/proxy that silently
    // drops the CDN request (neither onload nor onerror ever fires) must not
    // leave the caller's promise — and the "Connecting…" modal gating on it
    // — hanging forever. Verifies the timeout added for exactly this case.
    vi.useFakeTimers();
    try {
      vi.resetModules();
      const { loadBelvoWidget: freshLoadBelvoWidget } = await import("./linkedAccounts.js");

      const hangingScriptEl = {};
      const fakeDocument = {
        createElement: vi.fn(() => hangingScriptEl),
        head: { appendChild: vi.fn() },
      };
      // eslint-disable-next-line no-global-assign
      globalThis.document = fakeDocument;

      const attempt = freshLoadBelvoWidget("tok_hang");
      const failure = attempt.catch((e) => e);
      // Neither hangingScriptEl.onload() nor .onerror() is ever called —
      // simulating a silently-dropped request — only the timeout can settle this.
      await vi.advanceTimersByTimeAsync(20000);
      const error = await failure;
      expect(error).toBeInstanceOf(Error);
      expect(error.message).toMatch(/timed out/i);

      delete globalThis.document;
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("Basiq link flow", () => {
  it("posts to the basiq session endpoint and returns redirect_url/reference", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ redirect_url: "https://consent.basiq.io/x", reference: "ref-1" });
    const result = await startBasiqLinkFlow({ budgetId: "b1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/basiq/session", { method: "POST" });
    expect(result.redirect_url).toBe("https://consent.basiq.io/x");
  });

  it("completes with the basiq_ref", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "basiq" }] });
    const result = await completeBasiqLinkFlow({ budgetId: "b1", basiqRef: "ref-1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/basiq/complete", {
      method: "POST",
      body: JSON.stringify({ basiq_ref: "ref-1" }),
    });
    expect(result.linked).toHaveLength(1);
  });
});

describe("Akahu link flow", () => {
  it("posts to the akahu session endpoint", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ redirect_url: "https://oauth.akahu.nz/authorize?x", reference: "ref-2" });
    const result = await startAkahuLinkFlow({ budgetId: "b1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/akahu/session", { method: "POST" });
    expect(result.redirect_url).toContain("oauth.akahu.nz");
  });

  it("completes with the akahu_ref and code", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "akahu" }] });
    const result = await completeAkahuLinkFlow({ budgetId: "b1", akahuRef: "ref-2", code: "auth_code", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/akahu/complete", {
      method: "POST",
      body: JSON.stringify({ akahu_ref: "ref-2", code: "auth_code" }),
    });
    expect(result.linked).toHaveLength(1);
  });
});

describe("Plaid Canada bank linking (#321)", () => {
  it("startPlaidLinkFlow creates a link token when none is supplied, then opens Plaid Link", async () => {
    const fetchApi = vi.fn()
      .mockResolvedValueOnce({ link_token: "link-abc", session_id: "sess-1" }) // POST .../plaid/link-token
      .mockResolvedValueOnce({ accounts: [{ id: "a1" }] }); // POST .../plaid/complete
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    await flushPendingPromises();
    const onSuccessArg = create.mock.calls[0][0].onSuccess;
    await onSuccessArg("public-tok", { institution: { name: "RBC" } });
    const { linked } = await resultPromise;

    expect(fetchApi).toHaveBeenNthCalledWith(1, "/budgets/b1/plaid/link-token", { method: "POST" });
    expect(create).toHaveBeenCalledWith(expect.objectContaining({ token: "link-abc" }));
    expect(open).toHaveBeenCalled();
    expect(fetchApi).toHaveBeenNthCalledWith(2, "/budgets/b1/plaid/complete", {
      method: "POST",
      body: JSON.stringify({ session_id: "sess-1", public_token: "public-tok" }),
    });
    expect(linked).toEqual([{ id: "a1" }]);
  });

  it("startPlaidLinkFlow skips link-token creation when linkToken/sessionId are already supplied (chat flow)", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ accounts: [] });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const resultPromise = startPlaidLinkFlow({
      budgetId: "b1", fetchApi, loadPlaidLink, linkToken: "link-from-chat", sessionId: "sess-from-chat",
    });
    await flushPendingPromises();
    const onSuccessArg = create.mock.calls[0][0].onSuccess;
    await onSuccessArg("public-tok-2", {});
    await resultPromise;

    expect(fetchApi).toHaveBeenCalledTimes(1); // no /link-token call — only /complete
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/plaid/complete", {
      method: "POST",
      body: JSON.stringify({ session_id: "sess-from-chat", public_token: "public-tok-2" }),
    });
  });

  it("startPlaidLinkFlow's onExit with no error resolves an empty linked list (user cancel)", async () => {
    // Deliberately a valid-shaped link-token response here: this test is
    // about the onExit cancel path, not about the fail-fast link-token
    // check (covered separately below), so it must actually reach
    // Plaid.create/onExit rather than throwing beforehand.
    const fetchApi = vi.fn().mockResolvedValueOnce({ link_token: "link-x", session_id: "sess-x" });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    await flushPendingPromises();
    const onExitArg = create.mock.calls[0][0].onExit;
    onExitArg(null, {});
    const { linked } = await resultPromise;

    expect(linked).toEqual([]);
    expect(fetchApi).not.toHaveBeenCalledWith(expect.stringContaining("/complete"), expect.anything());
  });

  // Before this fix, a malformed/missing link-token response silently became
  // `token = undefined`, flowed into Plaid.create({token: undefined, ...})
  // and handler.open(), and only surfaced later as a confusing backend
  // error from /plaid/complete (code review).
  it("startPlaidLinkFlow rejects with a clear error when the link-token response is missing link_token", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ session_id: "sess-1" }); // no link_token
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    await expect(
      startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink }),
    ).rejects.toThrow("Failed to create Plaid link token");

    expect(create).not.toHaveBeenCalled();
  });

  it("startPlaidLinkFlow rejects with a clear error when the link-token response is malformed (undefined)", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce(undefined);
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    await expect(
      startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink }),
    ).rejects.toThrow("Failed to create Plaid link token");

    expect(create).not.toHaveBeenCalled();
  });

  // Copilot review finding (#321): the original code only fail-fast-checked
  // `link_token`, never `session_id` — a malformed response with a
  // link_token but no session_id would silently omit session_id from the
  // /plaid/complete request body instead of failing clearly here.
  it("startPlaidLinkFlow rejects with a clear error when the link-token response is missing session_id", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ link_token: "link-abc" }); // no session_id
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    await expect(
      startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink }),
    ).rejects.toThrow("Failed to create Plaid link session");

    expect(create).not.toHaveBeenCalled();
  });

  it("startPlaidLinkFlow rejects when the caller supplies linkToken but no sessionId", async () => {
    const fetchApi = vi.fn();
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    await expect(
      startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink, linkToken: "link-abc" }), // no sessionId
    ).rejects.toThrow("Failed to create Plaid link session");

    expect(fetchApi).not.toHaveBeenCalled();
    expect(create).not.toHaveBeenCalled();
  });

  it("startPlaidLinkFlow's onExit WITH an error rejects with that error's message", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ link_token: "link-abc", session_id: "sess-1" });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    await flushPendingPromises();
    const onExitArg = create.mock.calls[0][0].onExit;
    onExitArg({ error_message: "user closed during OAuth" }, {});

    await expect(resultPromise).rejects.toThrow("user closed during OAuth");
  });

  // Before this fix, receivedRedirectUri was never threaded through to
  // Plaid.create(...) at all, which would break Big-5 Canadian (OAuth-flow)
  // bank linking — Plaid's documented integration contract requires the
  // caller to explicitly hand back the redirect URL to resume an OAuth Link
  // session (code review, #321).
  it("startPlaidLinkFlow passes receivedRedirectUri to Plaid.create when supplied (OAuth resume)", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ accounts: [] });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const resultPromise = startPlaidLinkFlow({
      budgetId: "b1",
      fetchApi,
      loadPlaidLink,
      linkToken: "link-from-resume",
      sessionId: "sess-from-resume",
      receivedRedirectUri: "https://example.com/?oauth_state_id=abc",
    });
    await flushPendingPromises();
    const onExitArg = create.mock.calls[0][0].onExit;
    onExitArg(null, {});
    await resultPromise;

    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({ receivedRedirectUri: "https://example.com/?oauth_state_id=abc" }),
    );
  });

  it("startPlaidLinkFlow omits receivedRedirectUri from Plaid.create when not supplied (normal Link init)", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ link_token: "link-abc", session_id: "sess-1" });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    await flushPendingPromises();
    const onExitArg = create.mock.calls[0][0].onExit;
    onExitArg(null, {});
    await resultPromise;

    expect(create.mock.calls[0][0]).not.toHaveProperty("receivedRedirectUri");
  });

  it("isPlaidOAuthReturn detects the oauth_state_id query param", () => {
    expect(isPlaidOAuthReturn("?oauth_state_id=abc123")).toBe(true);
    expect(isPlaidOAuthReturn("?gc_ref=xyz")).toBe(false);
    expect(isPlaidOAuthReturn("")).toBe(false);
  });

  it("PLAID_LINK_RESUME_KEY is the shared localStorage key constant", () => {
    expect(PLAID_LINK_RESUME_KEY).toBe("plaidLinkResume");
  });

  // App.svelte's handlePlaidOAuthReturn calls this fire-and-forget from
  // onMount with no try/catch of its own — a raw JSON.parse there would be
  // an unhandled promise rejection with zero user feedback if the stored
  // value were ever corrupted/tampered with. parsePlaidLinkResume owns that
  // parsing so it's unit-testable without any DOM/component-test tooling
  // (code review, #321).
  it("parsePlaidLinkResume parses a well-formed stored payload", () => {
    const stored = JSON.stringify({ linkToken: "lt-1", sessionId: "s-1", budgetId: "b-1" });
    expect(parsePlaidLinkResume(stored)).toEqual({ linkToken: "lt-1", sessionId: "s-1", budgetId: "b-1" });
  });

  it("parsePlaidLinkResume returns null (not a throw) for malformed JSON", () => {
    expect(() => parsePlaidLinkResume("{not valid json")).not.toThrow();
    expect(parsePlaidLinkResume("{not valid json")).toBeNull();
  });

  it("parsePlaidLinkResume returns null for a non-object JSON value", () => {
    expect(parsePlaidLinkResume("42")).toBeNull();
    expect(parsePlaidLinkResume("null")).toBeNull();
    expect(parsePlaidLinkResume('"a string"')).toBeNull();
  });

  it("parsePlaidLinkResume returns null for a missing/empty stored value", () => {
    expect(parsePlaidLinkResume(null)).toBeNull();
    expect(parsePlaidLinkResume("")).toBeNull();
  });
});

// loadPlaidLink must be idempotent under concurrent/retry calls (code
// review): two overlapping calls before the first onload/onerror fires must
// share ONE in-flight promise rather than each injecting their own <script>
// tag, and a failed load must be cleanly retryable rather than leaving a
// permanently broken script tag with no way to recover. This project's
// vitest config runs tests in the "node" environment (no jsdom — see
// vitest.config.js and the absence of a jsdom devDependency), so these tests
// stub a minimal document/window via vi.stubGlobal rather than relying on a
// real DOM. Each test resets the module registry via vi.resetModules() and
// re-imports fresh, since the in-flight-promise tracker is module-private
// state with no way to reset it from outside between tests.
describe("loadPlaidLink script loader (#321)", () => {
  let fakeScripts;

  beforeEach(() => {
    vi.resetModules();
    fakeScripts = [];
    vi.stubGlobal("document", {
      createElement: vi.fn(() => {
        const script = { onload: null, onerror: null, remove: vi.fn() };
        fakeScripts.push(script);
        return script;
      }),
      head: { appendChild: vi.fn() },
    });
    vi.stubGlobal("window", {});
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("short-circuits immediately when window.Plaid is already set, without touching the DOM", async () => {
    const fakePlaid = { create: vi.fn() };
    window.Plaid = fakePlaid;
    const { loadPlaidLink: freshLoadPlaidLink } = await import("./linkedAccounts.js");

    const result = await freshLoadPlaidLink();

    expect(result).toBe(fakePlaid);
    expect(document.createElement).not.toHaveBeenCalled();
  });

  it("rejects (and clears the load-cache) if onload fires but window.Plaid was never actually defined", async () => {
    // Copilot review finding (#321): onload firing only proves the network
    // request succeeded, not that the script body actually defined the
    // Plaid global (e.g. a CDN serving a truncated 200 response). Without
    // this check the cached promise would resolve to undefined forever.
    const { loadPlaidLink: freshLoadPlaidLink } = await import("./linkedAccounts.js");

    const attempt = freshLoadPlaidLink();
    expect(fakeScripts).toHaveLength(1);
    // window.Plaid is deliberately never set here.
    fakeScripts[0].onload();

    await expect(attempt).rejects.toThrow(/did not define window.Plaid/i);
    expect(fakeScripts[0].remove).toHaveBeenCalled();

    // A subsequent call must start a genuinely fresh attempt, not stay
    // stuck behind the cached-but-broken promise.
    const p2 = freshLoadPlaidLink();
    expect(fakeScripts).toHaveLength(2);
    window.Plaid = { create: vi.fn() };
    fakeScripts[1].onload();
    await expect(p2).resolves.toBe(window.Plaid);
  });

  it("two concurrent calls before the script loads append only one <script> tag", async () => {
    const { loadPlaidLink: freshLoadPlaidLink } = await import("./linkedAccounts.js");

    const p1 = freshLoadPlaidLink();
    const p2 = freshLoadPlaidLink();

    expect(document.createElement).toHaveBeenCalledTimes(1);
    expect(fakeScripts).toHaveLength(1);

    window.Plaid = { create: vi.fn() };
    fakeScripts[0].onload();

    const [plaid1, plaid2] = await Promise.all([p1, p2]);
    expect(plaid1).toBe(window.Plaid);
    expect(plaid2).toBe(window.Plaid);
  });

  it("a subsequent call after a failed load starts a fresh load attempt", async () => {
    const { loadPlaidLink: freshLoadPlaidLink } = await import("./linkedAccounts.js");

    const p1 = freshLoadPlaidLink();
    expect(fakeScripts).toHaveLength(1);
    fakeScripts[0].onerror();
    await expect(p1).rejects.toThrow("Failed to load Plaid Link");
    expect(fakeScripts[0].remove).toHaveBeenCalled();

    // window.Plaid was never set (the load failed), so this must NOT
    // short-circuit — it should start a genuinely fresh load attempt.
    const p2 = freshLoadPlaidLink();
    expect(document.createElement).toHaveBeenCalledTimes(2);
    expect(fakeScripts).toHaveLength(2);

    window.Plaid = { create: vi.fn() };
    fakeScripts[1].onload();
    await expect(p2).resolves.toBe(window.Plaid);
  });

  it("rejects (rather than hanging forever) if the script never fires onload or onerror", async () => {
    // Silent-failure-hunter finding (#321): mirrors the equivalent Belvo test
    // — a browser extension/proxy that silently drops the CDN request
    // (neither onload nor onerror ever fires) must not leave the caller's
    // promise hanging forever. Verifies the timeout added for exactly this case.
    vi.useFakeTimers();
    try {
      const { loadPlaidLink: freshLoadPlaidLink } = await import("./linkedAccounts.js");

      const attempt = freshLoadPlaidLink();
      const failure = attempt.catch((e) => e);
      expect(fakeScripts).toHaveLength(1);
      // Neither fakeScripts[0].onload() nor .onerror() is ever called —
      // simulating a silently-dropped request — only the timeout can settle this.
      await vi.advanceTimersByTimeAsync(20000);
      const error = await failure;
      expect(error).toBeInstanceOf(Error);
      expect(error.message).toMatch(/timed out/i);
      expect(fakeScripts[0].remove).toHaveBeenCalled();

      // A subsequent call after the timeout must start a fresh attempt, not
      // stay permanently stuck behind the timed-out one.
      const p2 = freshLoadPlaidLink();
      expect(fakeScripts).toHaveLength(2);
      window.Plaid = { create: vi.fn() };
      fakeScripts[1].onload();
      await expect(p2).resolves.toBe(window.Plaid);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("formatRelativeTime", () => {
  const now = 1_000_000_000_000;
  it("returns minutes under an hour (min 1m)", () => {
    expect(formatRelativeTime(now - 30_000, now)).toBe("1m");
    expect(formatRelativeTime(now - 5 * 60_000, now)).toBe("5m");
  });
  it("returns hours under a day", () => {
    expect(formatRelativeTime(now - 3 * 3_600_000, now)).toBe("3h");
  });
  it("returns days under a week", () => {
    expect(formatRelativeTime(now - 2 * 86_400_000, now)).toBe("2d");
  });
  it("returns weeks, months, years for larger gaps", () => {
    expect(formatRelativeTime(now - 10 * 86_400_000, now)).toBe("1w");
    expect(formatRelativeTime(now - 60 * 86_400_000, now)).toBe("2mo");
    expect(formatRelativeTime(now - 400 * 86_400_000, now)).toBe("1y");
  });
  it("clamps future timestamps to 1m and returns '' for invalid input", () => {
    expect(formatRelativeTime(now + 5000, now)).toBe("1m");
    expect(formatRelativeTime(undefined, now)).toBe("");
    expect(formatRelativeTime(NaN, now)).toBe("");
  });
});

describe("syncStatusFor", () => {
  const now = Date.parse("2026-01-01T12:00:00Z");
  it("reports synced with a relative label when last_synced_at is set", () => {
    const r = syncStatusFor({ last_synced_at: "2026-01-01T09:00:00Z" }, now);
    expect(r).toEqual({ kind: "synced", relative: "3h" });
  });
  it("reports never when last_synced_at is absent", () => {
    expect(syncStatusFor({ last_synced_at: null }, now)).toEqual({ kind: "never" });
    expect(syncStatusFor({}, now)).toEqual({ kind: "never" });
  });
  it("reports never when last_synced_at is unparseable", () => {
    expect(syncStatusFor({ last_synced_at: "not-a-date" }, now)).toEqual({ kind: "never" });
  });
});

describe("statusBadgeFor", () => {
  it("maps active to success + check", () => {
    expect(statusBadgeFor({ status: "active" })).toEqual({
      variant: "badge-success", icon: "check", labelKey: "linkedAccounts.activeBadge",
    });
  });
  it("maps consent_expired to warning + alert", () => {
    expect(statusBadgeFor({ status: "consent_expired" })).toEqual({
      variant: "badge-warning", icon: "alert", labelKey: "linkedAccounts.consentExpiredBadge",
    });
  });
  it("maps disconnected to ghost + x", () => {
    expect(statusBadgeFor({ status: "disconnected" })).toEqual({
      variant: "badge-ghost", icon: "x", labelKey: "linkedAccounts.disconnectedBadge",
    });
  });
  it("defaults unknown status to active styling", () => {
    expect(statusBadgeFor({ status: "weird" }).variant).toBe("badge-success");
  });
});

describe("initialsFor", () => {
  it("takes first letters of the first and last words of institution_name", () => {
    expect(initialsFor({ institution_name: "Bank of America" })).toBe("BA");
  });
  it("takes two letters of a single word", () => {
    expect(initialsFor({ institution_name: "Monzo" })).toBe("MO");
  });
  it("falls back to display_name then a glyph", () => {
    expect(initialsFor({ display_name: "Joint Checking" })).toBe("JC");
    expect(initialsFor({})).toBe("?");
  });
});

describe("maskedLast4", () => {
  it("masks when present", () => {
    expect(maskedLast4({ last4: "1234" })).toBe("•••• 1234");
  });
  it("returns empty string when absent", () => {
    expect(maskedLast4({})).toBe("");
  });
});

describe("groupAccountsByInstitution", () => {
  it("groups by institution_name preserving first-seen order and computes count + oldest sync", () => {
    const accts = [
      { id: "a", institution_name: "Chase", last_synced_at: "2026-01-02T00:00:00Z" },
      { id: "b", institution_name: "Monzo", last_synced_at: null },
      { id: "c", institution_name: "Chase", last_synced_at: "2026-01-01T00:00:00Z" },
    ];
    const groups = groupAccountsByInstitution(accts);
    expect(groups.map((g) => g.key)).toEqual(["Chase", "Monzo"]);
    expect(groups[0].count).toBe(2);
    expect(groups[0].oldestSyncedAtMs).toBe(Date.parse("2026-01-01T00:00:00Z"));
    expect(groups[1].count).toBe(1);
    expect(groups[1].oldestSyncedAtMs).toBe(null);
  });
  it("returns [] for non-array input", () => {
    expect(groupAccountsByInstitution(null)).toEqual([]);
  });
  it("groups accounts with a missing institution_name under an empty key with null name", () => {
    const groups = groupAccountsByInstitution([
      { id: "a", last_synced_at: null },
      { id: "b", institution_name: "", last_synced_at: null },
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].key).toBe("");
    expect(groups[0].institutionName).toBe(null);
    expect(groups[0].count).toBe(2);
  });
});

describe("filterInstitutions", () => {
  const insts = [{ id: "1", name: "Monzo" }, { id: "2", name: "Barclays" }, { id: "3" }];
  it("filters by name case-insensitively; empty query returns all", () => {
    expect(filterInstitutions(insts, "bar").map((i) => i.id)).toEqual(["2"]);
    expect(filterInstitutions(insts, "").length).toBe(3);
  });
  it("tolerates missing name and non-array input", () => {
    expect(filterInstitutions(insts, "monzo").map((i) => i.id)).toEqual(["1"]);
    expect(filterInstitutions(null, "x")).toEqual([]);
  });
});

describe("country region helpers", () => {
  it("covers all 14 countries across 4 ordered regions", () => {
    expect(COUNTRY_OPTIONS).toHaveLength(14);
    expect(REGION_ORDER).toEqual(["northAmerica", "europe", "latinAmerica", "oceania"]);
  });
  it("filters by name or ISO code, case-insensitive", () => {
    expect(filterCountries(COUNTRY_OPTIONS, "united").map((c) => c.id).sort()).toEqual(["GB", "US"]);
    expect(filterCountries(COUNTRY_OPTIONS, "de").map((c) => c.id)).toContain("DE");
    expect(filterCountries(COUNTRY_OPTIONS, "").length).toBe(14);
  });
  it("groups filtered countries by region in REGION_ORDER, skipping empty regions", () => {
    const grouped = groupCountriesByRegion(filterCountries(COUNTRY_OPTIONS, "brazil"));
    expect(grouped).toHaveLength(1);
    expect(grouped[0].region).toBe("latinAmerica");
    expect(grouped[0].countries.map((c) => c.id)).toEqual(["BR"]);
  });
  it("orders all populated regions per REGION_ORDER", () => {
    expect(groupCountriesByRegion(COUNTRY_OPTIONS).map((g) => g.region)).toEqual(
      ["northAmerica", "europe", "latinAmerica", "oceania"],
    );
  });
});

describe("linkCtaMeta", () => {
  it("widget for Belvo countries", () => {
    expect(linkCtaMeta("MX")).toEqual({
      kind: "widget", ctaKey: "linkedAccounts.ctaConnect", helperKey: "linkedAccounts.widgetHelper", ctaValues: {},
    });
  });
  it("redirect with bank name for GoCardless when a bank is chosen", () => {
    expect(linkCtaMeta("DE", { bankName: "N26" })).toEqual({
      kind: "redirect", ctaKey: "linkedAccounts.ctaContinueToBank", helperKey: "linkedAccounts.redirectHelper", ctaValues: { bank: "N26" },
    });
  });
  it("redirect generic for GoCardless without a bank, and for Basiq/Akahu", () => {
    expect(linkCtaMeta("DE").ctaKey).toBe("linkedAccounts.ctaContinueGeneric");
    expect(linkCtaMeta("AU")).toEqual({
      kind: "redirect", ctaKey: "linkedAccounts.ctaContinueGeneric", helperKey: "linkedAccounts.redirectHelper", ctaValues: {},
    });
    expect(linkCtaMeta("NZ").kind).toBe("redirect");
  });
  it("modal for Stripe (US) and Plaid (CA)", () => {
    expect(linkCtaMeta("US")).toEqual({
      kind: "modal", ctaKey: "linkedAccounts.ctaConnect", helperKey: "linkedAccounts.modalHelper", ctaValues: {},
    });
    expect(linkCtaMeta("CA").kind).toBe("modal");
  });
  it("ignores a stray bankName on non-GoCardless redirect countries (AU) and on empty-string bankName", () => {
    expect(linkCtaMeta("AU", { bankName: "Anything" }).ctaKey).toBe("linkedAccounts.ctaContinueGeneric");
    expect(linkCtaMeta("DE", { bankName: "" }).ctaKey).toBe("linkedAccounts.ctaContinueGeneric");
  });
});
