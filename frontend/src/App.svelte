<script>
  import { onMount, tick } from "svelte";
  import { startRegistration, startAuthentication } from "@simplewebauthn/browser";
  import {
    Info,
    AlertTriangle,
    Send,
    UserCheck,
    CheckCircle2,
    Copy,
    Check,
    Menu,
    X,
    Sparkles,
    Mic,
    History,
    PenSquare,
    ShieldCheck,
  } from "lucide-svelte";
  import MainMenu from "./lib/MainMenu.svelte";
  import Settings from "./lib/Settings.svelte";
  import AccountsView from "./lib/AccountsView.svelte";
  import Insights from "./lib/Insights.svelte";
  import RetirementView from "./lib/RetirementView.svelte";
  import CategoriesView from "./lib/CategoriesView.svelte";
  import TransactionsView from "./lib/TransactionsView.svelte";
  import BudgetDetails from "./lib/BudgetDetails.svelte";
  import BudgetsView from "./lib/BudgetsView.svelte";
  import DeleteAccount from "./lib/DeleteAccount.svelte";
  import HistoryPage from "./lib/History.svelte";
  import {
    ROUTES,
    hashForRoute,
    resolveRoute,
    applyFlaggedRoutes,
    hashForBudgetDetails,
    hashForBudgetsList,
  } from "./lib/router.js";
  import Notifications from "./lib/Notifications.svelte";
  import InstallPrompt from "./lib/InstallPrompt.svelte";
  import BudgetStatusInfo from "./lib/BudgetStatusInfo.svelte";
  import QueuedMessages from "./lib/QueuedMessages.svelte";
  import nelsMark from "./assets/nels-mark.png";
  import { _ } from "svelte-i18n";
  import { get } from "svelte/store";
  import { currentLocale } from "./lib/i18n/index.js";
  import { createDictation } from "./lib/speech.svelte.js";
  import { isMobileOS } from "./lib/platform.js";
  import { fmtMoney } from "./lib/money.js";
  import { parseCheckoutDeepLink } from "./lib/checkoutDeepLink.js";
  import {
    displayBudgetName,
    zeroBasedAvailable,
    zeroBasedAllocated,
    zeroBasedLeft,
  } from "./lib/budgetDisplay.js";
  import {
    startLinkFlow, completeGcLinkFlow, completeBelvoLinkFlow, loadBelvoWidget,
    completeBasiqLinkFlow, completeAkahuLinkFlow,
    startPlaidLinkFlow, loadPlaidLink, isPlaidOAuthReturn, parsePlaidLinkResume, PLAID_LINK_RESUME_KEY,
  } from "./lib/linkedAccounts.js";
  import {
    isCommand,
    parseCommand,
    matchCommands,
    knownCommandName,
    parseIssueArgs,
    resolveBudgetByName,
    resolveActiveBudget,
  } from "./lib/commands.js";
  import { windowMessages } from "./lib/chatWindow.js";
  import { inlineCategoriesTableHtml } from "./lib/chatMessage.js";
  import { buildChoiceChips, finalizePayloadFor } from "./lib/categoryChoice.js";
  import {
    QUEUE_CAP,
    canEnqueue,
    enqueue,
    removeFromQueue,
    runDrain,
  } from "./lib/chatQueue.js";
  import {
    fetchWithTimeout,
    DEFAULT_FETCH_TIMEOUT_MS,
    TIMEOUT_MESSAGE,
  } from "./lib/fetchTimeout.js";
  import { parseApiResponse } from "./lib/parseApiResponse.js";
  import { providerErrorKey } from "./lib/aiProviderView.js";

  const t = (key, opts) => get(_)(key, opts);

  // --- reactive Svelte 5 states (Runes) ---
  let token = $state(localStorage.getItem("nels_token") || "");
  let activeScreen = $state("auth"); // 'chat', 'auth'
  let user = $state(null);
  // Runtime feature flag for the #469 retirement planner, surfaced by the
  // backend via GET /api/auth/me (`retirement_planner_enabled`) and read at
  // request time from RETIREMENT_PLANNER_ENABLED. Defaults OFF (planner hidden);
  // when off, the "retirement" route collapses to "chat" (see applyFlaggedRoutes
  // and the guard in navigate) and the menu item is hidden (MainMenu).
  const retirementPlannerEnabled = $derived(user?.retirement_planner_enabled === true);
  let budgets = $state([]);
  let activeBudget = $state(null);
  let insights = $state(null);
  // Stripe billing entitlement (#25). Shape:
  // { status, tier ("none"|"basic"|"pro"), payment_warning, is_pro (legacy),
  //   price_id, current_period_end, cancel_at_period_end }.
  let subscription = $state(null);
  // True briefly after a successful checkout while the webhook-synced entitlement
  // hasn't landed yet, so Settings can show a "finalizing" hint (#25).
  let billingFinalizing = $state(false);
  let chatMessages = $state([]);
  // When false (default) the transcript renders only its most recent slice to
  // keep long mobile sessions responsive; the "Show earlier messages" affordance
  // flips this to reveal the full history. Reset at every conversation boundary
  // and on send so a revealed history re-collapses to the window.
  let showAllMessages = $state(false);
  const chatWindow = $derived(windowMessages(chatMessages, showAllMessages));
  // A deletion proposed by the assistant, awaiting confirmation in a custom
  // modal. Shape: { kind: "category"|"budget"|"transaction", id, name, budget_id }.
  let pendingDeletion = $state(null);
  // A category choice offered by the assistant when it would otherwise
  // auto-create a brand-new category for an un-categorized transaction (#376).
  // Shape: the backend's `pending_category_choice`
  // { budget_id, amount, description, proposed_new_name, candidates:[{id,name}] }.
  // Rendered as inline chips under the AI message; resolved via
  // POST /budgets/:id/transactions/finalize.
  let pendingCategoryChoice = $state(null);
  // Guards against a double-tap re-firing finalize while a chip request is in
  // flight (the chips stay mounted on failure so the user can retry) (#376).
  let choiceSubmitting = $state(false);
  // Whether Belvo's embeddable Connect Widget (#322) is currently rendered
  // in its modal-shell container below, following a LINK_BANK_ACCOUNT chat
  // action against a Mexico/Brazil budget.
  let belvoWidgetActive = $state(false);

  // Conversation / sidebar state
  let conversations = $state([]);
  let activeConversationId = $state(null);
  // A conversation deletion staged from the sidebar, awaiting confirmation in a
  // custom modal (mirrors pendingDeletion). Shape: { id, name }.
  let pendingConversationDeletion = $state(null);
  let menuOpen = $state(false);
  let suggestedQuestion = $state("");
  let hamburgerEl = $state(null);
  // Bound instance of the notification feed (#55) so we can poke it to re-poll
  // after a chat turn that may have generated a server-side alert.
  let notificationsRef = $state(null);

  // Form states
  let emailInput = $state("");
  let chatInput = $state("");
  let chatInputEl = $state(null); // bound <textarea> for auto-grow
  const CHAT_INPUT_MAX_PX = 160; // ~6 lines, then it scrolls

  // Grow the chat textarea to fit its content, capped at CHAT_INPUT_MAX_PX.
  function autoGrowChatInput() {
    const el = chatInputEl;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, CHAT_INPUT_MAX_PX) + "px";
  }

  // Keep the textarea sized whenever its value changes — covers typing,
  // dictation, suggested prompts, and clearing after send.
  $effect(() => {
    chatInput;
    autoGrowChatInput();
  });

  // --- Prompt-history recall (#124) ---
  // Session-scoped, in-memory list of prompts the user has sent (oldest→newest).
  // Not persisted across reloads (out of scope). Capped to bound memory.
  // Consecutive duplicates are intentionally kept (no de-dup — out of scope).
  const MAX_PROMPT_HISTORY = 50;
  let promptHistory = $state([]);
  // Cursor into promptHistory while walking history. -1 means "not navigating"
  // (the composer reflects the newest/empty state, shell-style).
  let historyCursor = $state(-1);

  // Append a sent prompt as the newest entry and reset the recall cursor.
  function recordPrompt(text) {
    promptHistory.push(text);
    if (promptHistory.length > MAX_PROMPT_HISTORY) promptHistory.shift();
    historyCursor = -1;
  }

  // After a programmatic recall, place the caret at the end of the recalled
  // text (Svelte leaves it at position 0 otherwise). Waits for the DOM to
  // reflect the new value, then focuses the textarea first — setSelectionRange
  // is a no-op on an unfocused field, which matters when the touch recall
  // button (rather than the keyboard) triggered the recall.
  async function moveCaretToEnd() {
    await tick();
    const el = chatInputEl;
    if (!el) return;
    el.focus();
    const end = el.value.length;
    el.setSelectionRange(end, end);
  }

  // History.svelte's onPromptSelected hook (#261): populate the chat input
  // with a past conversation's opening message and return to chat with the
  // caret at the end. History.svelte itself gates this behind its own
  // overwrite-confirm dialog when the draft is non-empty (hasDraft prop) —
  // by the time this fires, the user has already agreed to replace it.
  function applyHistoryPrompt(text) {
    chatInput = text;
    navigate("chat");
    void moveCaretToEnd();
  }

  // Walk one step toward older prompts (ArrowUp / recall button). Clamps at the
  // oldest entry. No-op when history is empty.
  function recallPrev() {
    if (promptHistory.length === 0) return;
    if (historyCursor === -1) historyCursor = promptHistory.length - 1;
    else if (historyCursor > 0) historyCursor -= 1;
    chatInput = promptHistory[historyCursor];
    void moveCaretToEnd();
  }

  // The touch recall button mirrors the keyboard guard: it only recalls from an
  // empty composer or an unedited recalled prompt, so an accidental tap never
  // clobbers text the user is in the middle of typing.
  function recallFromButton() {
    if (chatInput === "") historyCursor = -1;
    else if (
      !(historyCursor !== -1 && chatInput === promptHistory[historyCursor])
    )
      return;
    recallPrev();
  }

  // Walk one step toward newer prompts (ArrowDown). Stepping past the newest
  // entry returns to an empty composer (cursor -1).
  function recallNext() {
    if (historyCursor === -1) return;
    if (historyCursor < promptHistory.length - 1) {
      historyCursor += 1;
      chatInput = promptHistory[historyCursor];
      void moveCaretToEnd();
    } else {
      historyCursor = -1;
      chatInput = "";
    }
  }

  // --- Slash-command palette (#56) ---
  // The autocomplete list shown when the input starts with "/". `paletteIndex`
  // tracks the keyboard-highlighted row.
  let paletteIndex = $state(0);
  // Matching commands for the current input; empty => palette hidden.
  let paletteMatches = $derived(matchCommands(chatInput));
  let paletteOpen = $derived(paletteMatches.length > 0);

  // Reset the highlighted row whenever the match set changes so the selection
  // never points past the end of the list.
  $effect(() => {
    paletteMatches;
    paletteIndex = 0;
  });

  // Insert the chosen command name into the input and refocus, leaving a
  // trailing space so the user can type arguments immediately.
  function applyPaletteSelection(name) {
    chatInput = name + " ";
    chatInputEl?.focus();
  }

  // Desktop: Enter sends, Shift+Enter inserts a newline. Touch devices keep
  // Enter as a newline (send via the button) so it's usable on phones.
  function handleChatInputKeydown(e) {
    // Ignore Enter while an IME composition is active — it commits the
    // candidate, it should not send the message.
    if (e.isComposing) return;

    // When the palette is open, Up/Down navigate it and Enter/Tab accept the
    // highlighted command instead of sending.
    if (paletteOpen) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        paletteIndex = (paletteIndex + 1) % paletteMatches.length;
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        paletteIndex =
          (paletteIndex - 1 + paletteMatches.length) % paletteMatches.length;
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        applyPaletteSelection(paletteMatches[paletteIndex].name);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        chatInput = "";
        return;
      }
    }

    // Prompt-history recall (#124): runs only when the palette is closed.
    // `onRecalledPrompt` is true while the composer still shows an UNEDITED
    // recalled prompt. ArrowUp recalls from an empty composer or continues
    // walking back through an unedited recalled prompt; ArrowDown walks forward.
    // Once the user edits the recalled text, chatInput no longer matches the
    // history entry, so arrows resume normal cursor movement and never clobber
    // what the user is typing.
    const onRecalledPrompt =
      historyCursor !== -1 && chatInput === promptHistory[historyCursor];
    // Arrow-key recall stays usable while a turn is in flight (#230) — the composer is never
    // disabled anymore, so there's no reason to suppress it during isChatLoading/draining.
    if (e.key === "ArrowUp" && (chatInput === "" || onRecalledPrompt)) {
      e.preventDefault();
      // An empty composer always starts recall from the most recent prompt,
      // even if a stale cursor lingers from an earlier, since-edited recall.
      if (chatInput === "") historyCursor = -1;
      recallPrev();
      return;
    }
    if (e.key === "ArrowDown" && onRecalledPrompt) {
      e.preventDefault();
      recallNext();
      return;
    }

    const coarsePointer = window.matchMedia("(pointer: coarse)").matches;
    if (e.key === "Enter" && !e.shiftKey && !coarsePointer) {
      e.preventDefault();
      sendChatMessage();
    }
  }

  // Voice dictation (browser-native speech-to-text into the chat input).
  const dictation = createDictation();
  let dictationBase = ""; // chatInput contents captured when listening began

  // True on iOS/Android, where the in-composer mic + recall buttons are hidden
  // (the native mobile keyboard already provides dictation and recent-input
  // affordances, and screen width is scarce). Initialized synchronously — this
  // is a client-rendered SPA so `navigator` is available at component init, and
  // computing it here (rather than in onMount) avoids a first-paint flash of the
  // buttons before they're hidden. OS can't change mid-session, so it's set once. (#154)
  let isMobile = $state(isMobileOS());

  // UI state indicators
  let isLoading = $state(false);
  let isChatLoading = $state(false);
  // FIFO queue for messages/commands submitted while a turn is in flight (#230). Draining is
  // guarded by isDraining so only one drainQueue() runs at a time; see performSend and
  // drainQueue (defined above sendChatMessage).
  let pendingQueue = $state([]);
  let isDraining = $state(false);
  let errorAlert = $state("");
  let successAlert = $state("");
  let isRegFlow = $state(false); // Toggle register vs login
  let showRegSuccess = $state(false);
  let tempRegToken = $state("");
  let tempRegUser = $state(null);
  let copiedText = $state(false);

  // Recovery-code flow state (nels#551): shown once at registration, and
  // redeemable to enroll a fresh passkey when a device is lost.
  let tempRecoveryCodes = $state([]);
  let copiedRecoveryCodes = $state(false);
  let showRecoveryForm = $state(false); // Toggle the "use a recovery code" form on the auth screen
  let recoveryEmailInput = $state("");
  let recoveryCodeInput = $state("");

  // --- API Request Helper ---
  // Production builds inject VITE_API_BASE (e.g. https://nels-api.fly.dev/api);
  // local dev falls back to the backend on localhost.
  const API_BASE = import.meta.env.VITE_API_BASE ?? "http://localhost:3000/api";

  async function fetchApi(endpoint, options = {}) {
    // `timeoutMs` is fetchApi's own extension to `options` (not a native RequestInit key), so
    // it must be pulled out before the rest is spread into fetchWithTimeout/fetch — otherwise it
    // would ride along as an unrecognized key on the native fetch() call. See #246.
    const { timeoutMs, ...fetchOptions } = options;
    const headers = {
      "Content-Type": "application/json",
      ...fetchOptions.headers,
    };
    if (token) {
      headers["Authorization"] = `Bearer ${token}`;
    }

    const res = await fetchWithTimeout(
      `${API_BASE}${endpoint}`,
      {
        ...fetchOptions,
        headers,
      },
      timeoutMs,
    );

    if (res.status === 401) {
      handleLogout();
      throw new Error("Unauthorized");
    }

    if (!res.ok) {
      const errText = await res.text();
      const err = new Error(errText || "API error");
      // Attach the HTTP status so callers can distinguish error kinds (e.g.
      // linkedAccounts.js's isProGateError checks for 402) without parsing
      // errText. Purely additive — no existing catch site reads `.status`.
      err.status = res.status;
      throw err;
    }

    return await parseApiResponse(res);
  }

  // --- Auth Handlers (WebAuthn/FIDO2 passkeys, nels#551) ---
  // Each is a single click-to-completion ceremony: /start returns WebAuthn
  // options, the browser prompts for the passkey, then /finish verifies the
  // result — no intermediate code-entry step (that was TOTP's UI, not
  // needed here).
  async function handleAuthStart(e) {
    e.preventDefault();
    if (!emailInput) {
      triggerError(t("alerts.enterEmail"));
      return;
    }

    isLoading = true;
    errorAlert = "";
    successAlert = "";

    try {
      if (isRegFlow) {
        const startRes = await fetchApi("/auth/register/start", {
          method: "POST",
          body: JSON.stringify({ email: emailInput }),
        });
        const credential = await startRegistration({ optionsJSON: startRes.options });
        const finishRes = await fetchApi("/auth/register/finish", {
          method: "POST",
          body: JSON.stringify({ flow_id: startRes.flow_id, credential }),
        });
        tempRegToken = finishRes.token;
        tempRegUser = finishRes.user;
        tempRecoveryCodes = finishRes.recovery_codes ?? [];
        showRegSuccess = true;
        triggerSuccess(t("alerts.regSuccess"));
      } else {
        const startRes = await fetchApi("/auth/login/start", {
          method: "POST",
          body: JSON.stringify({ email: emailInput }),
        });
        const credential = await startAuthentication({ optionsJSON: startRes.options });
        const finishRes = await fetchApi("/auth/login/finish", {
          method: "POST",
          body: JSON.stringify({ flow_id: startRes.flow_id, credential }),
        });
        saveSession(finishRes.token, finishRes.user);
      }
    } catch (err) {
      console.error(err);
      const noPasskeyForThisApp = err.status === 404;
      triggerError(
        err.message ||
          (isRegFlow
            ? t("alerts.startRegisterFailed")
            : t("alerts.startLoginFailed")),
      );
      if (!isRegFlow && noPasskeyForThisApp) {
        // Steer straight to the fix instead of leaving the user stuck on a
        // bare error message.
        showRecoveryForm = true;
        recoveryEmailInput = emailInput;
      }
    } finally {
      isLoading = false;
    }
  }

  // Redeem a one-time recovery code to enroll a fresh passkey — the path for
  // a lost/new device, or bootstrapping a passkey on a second app origin
  // (e.g. the admin console) using a code already issued elsewhere.
  async function handleRecoveryStart(e) {
    e.preventDefault();
    if (!recoveryEmailInput || !recoveryCodeInput) {
      triggerError(t("alerts.enterRecoveryDetails"));
      return;
    }

    isLoading = true;
    errorAlert = "";
    successAlert = "";

    try {
      const startRes = await fetchApi("/auth/recovery/start", {
        method: "POST",
        body: JSON.stringify({
          email: recoveryEmailInput,
          recovery_code: recoveryCodeInput.trim(),
        }),
      });
      const credential = await startRegistration({ optionsJSON: startRes.options });
      const finishRes = await fetchApi("/auth/recovery/finish", {
        method: "POST",
        body: JSON.stringify({ flow_id: startRes.flow_id, credential }),
      });
      showRecoveryForm = false;
      recoveryEmailInput = "";
      recoveryCodeInput = "";
      saveSession(finishRes.token, finishRes.user);
      triggerSuccess(t("alerts.recoverySuccess"));
    } catch (err) {
      console.error(err);
      triggerError(err.message || t("alerts.recoveryFailed"));
    } finally {
      isLoading = false;
    }
  }

  function saveSession(newToken, newUser) {
    token = newToken;
    user = newUser;
    localStorage.setItem("nels_token", newToken);
    activeScreen = "chat";
    const resolvedOnLogin = resolveSessionStartRoute(window.location.hash);
    route = resolvedOnLogin.route;
    budgetDetailsId = resolvedOnLogin.budgetId;
    startFreshSession();
    // Catch a marketing ?upgrade deep-link now that the user is authed (#25):
    // a logged-out visitor from the pricing page lands here after login.
    handleBillingQueryParams();
    // The login/register finish payload is a UserResponse (no preference fields);
    // refresh from /auth/me so the per-user status-strip summary toggles (#208)
    // load on session start. Fire-and-forget — the chat screen already shows.
    fetchApi("/auth/me")
      .then((me) => {
        user = me;
      })
      .catch((err) => {
        console.error(err);
      });
  }

  // On login / app open: start a fresh empty conversation (never auto-load
  // prior history), then load the sidebar list and a suggested question.
  function startFreshSession() {
    chatMessages = [];
    showAllMessages = false;
    activeConversationId = null;
    chatInput = "";
    dictationBase = "";
    promptHistory = [];
    historyCursor = -1;
    fetchBudgets();
    fetchConversations();
    fetchSuggestedQuestion();
    loadSubscription();
  }

  function handleRegSuccessContinue() {
    showRegSuccess = false;
    saveSession(tempRegToken, tempRegUser);
    tempRegToken = "";
    tempRegUser = null;
    tempRecoveryCodes = [];
    copiedText = false;
    copiedRecoveryCodes = false;
  }

  function copyToClipboard(text) {
    navigator.clipboard.writeText(text);
    copiedText = true;
    setTimeout(() => {
      copiedText = false;
    }, 2000);
  }

  function copyRecoveryCodes() {
    navigator.clipboard.writeText(tempRecoveryCodes.join("\n"));
    copiedRecoveryCodes = true;
    setTimeout(() => {
      copiedRecoveryCodes = false;
    }, 2000);
  }

  function handleLogout() {
    if (token) {
      fetchApi("/auth/logout", { method: "POST" }).catch(() => {});
    }
    token = "";
    user = null;
    budgets = [];
    activeBudget = null;
    insights = null;
    conversations = [];
    activeConversationId = null;
    chatMessages = [];
    chatInput = "";
    dictationBase = "";
    promptHistory = [];
    historyCursor = -1;
    // A background drainQueue() must not survive a session boundary — otherwise a message
    // typed under one account before logging out could still be in flight (or waiting its
    // turn) when a different account logs back in, and would land in the new account's
    // conversation. Dropping the queue and the in-flight flag here means the next
    // drainQueue() call (if any old promise is still resolving) sees an empty queue and
    // stops immediately. #230 review.
    pendingQueue = [];
    isDraining = false;
    menuOpen = false;
    deleteEmailInput = "";
    localStorage.removeItem("nels_token");
    activeScreen = "auth";
    // Reset the route/hash BEFORE the auth screen takes over — otherwise a
    // route like "settings"/"deleteAccount" (both hash-tracked, #261) stays
    // in window.location.hash, and the next login on this browser (any
    // account) would land straight back on that page via saveSession's own
    // resolveSessionStartRoute(window.location.hash) call — for
    // deleteAccount specifically (which that helper additionally excludes as
    // a passive landing target regardless), that's a stale-state page, not
    // just a wrong landing screen.
    navigate("chat");
  }

  // Download the account data export. fetchApi force-parses JSON, so it can't be
  // used here — the backend returns a JSON file attachment we save via a Blob.
  //
  // fetchWithTimeout's own timer only covers the fetch() call itself (it's cleared in a
  // `finally` the moment that promise settles) — per the Fetch spec, fetch() resolves once
  // response *headers* arrive, not after the body is read. A slow-streaming export body would
  // therefore hang the subsequent `res.blob()` with no timeout protection at all. So this
  // function owns its own AbortController + timer (not cleared until this function's own
  // try/finally) and passes the signal through `options.signal` — fetchWithTimeout's documented
  // pass-through path for a caller that wants to own its own cancellation — so the same signal
  // that would abort a hung fetch() also aborts a hung res.blob(). See #249.
  async function downloadExport() {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), DEFAULT_FETCH_TIMEOUT_MS);
    try {
      const res = await fetchWithTimeout(`${API_BASE}/account/export`, {
        headers: token ? { Authorization: `Bearer ${token}` } : {},
        signal: controller.signal,
      });
      if (res.status === 401) {
        handleLogout();
        return;
      }
      if (!res.ok) {
        throw new Error((await res.text()) || "Export failed");
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `nels-export-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      triggerSuccess($_("account.exportStarted"));
    } catch (e) {
      // fetchWithTimeout only translates AbortError -> TIMEOUT_MESSAGE on its own internal
      // (non-pass-through) timer; since this call supplies its own signal, that translation is
      // this function's own responsibility.
      const message = e.name === "AbortError" ? TIMEOUT_MESSAGE : e.message;
      triggerError(message || $_("account.exportFailed"));
    } finally {
      clearTimeout(timer);
    }
  }

  // --- Account deletion (typed email + TOTP confirmation) ---
  // Governs #chat-scroll-area's content: "chat" (default) | "categories" |
  // "insights" | "budgetDetails" (#233, #240). Synced to window.location.hash
  // so browser back/forward and a hard refresh on a deep link both work; see
  // frontend/src/lib/router.js for the pure hash<->route mapping. The
  // "budgetDetails" route additionally carries a dynamic budget id, which
  // ROUTES/navigate() below don't know about — it lives in the sibling
  // budgetDetailsId state instead (mirrors how activeConversationId sits
  // beside route/activeScreen).
  let route = $state("chat");
  let budgetDetailsId = $state(null);

  function navigate(name) {
    // The retirement planner is a flagged surface (#469): when the runtime flag
    // is off, an attempt to navigate there (menu, chat open_retirement signal,
    // anything else) collapses to chat so the URL never records a #/retirement.
    // Mirror of applyFlaggedRoutes, which guards the hash-resolution sites.
    if (name === "retirement" && !retirementPlannerEnabled) name = "chat";
    route = ROUTES.includes(name) ? name : "chat";
    // navigate() can never target "budgetDetails" (it's not in ROUTES), so
    // leaving any stale id behind here would only ever be dead state — clear
    // it so a later re-entry into budgetDetails always starts from a fresh id.
    budgetDetailsId = null;
    const hash = hashForRoute(route);
    // Always a plain hash assignment (never history.replaceState) — even
    // clearing the hash back to "" this way pushes a new history entry, so
    // every route transition (entering AND leaving a route) is independently
    // back/forward-navigable. A prior version special-cased the "leaving"
    // direction with replaceState, which silently destroyed the entry being
    // left and broke Back after using the in-app "back to chat" affordance.
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    }
  }

  // Navigates to the budget details outlet for an arbitrary budget id (#240)
  // — separate from navigate() above since it carries a parameter ROUTES
  // doesn't model. Mirrors navigate()'s hash-sync convention.
  function navigateToBudgetDetails(budgetId) {
    route = "budgetDetails";
    budgetDetailsId = budgetId;
    const hash = hashForBudgetDetails(budgetId);
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    }
  }

  // Navigates to the budgets list/overview outlet (#241) — separate from
  // navigate() above since "budgetsList" is not in the flat ROUTES array
  // (same reason navigateToBudgetDetails is separate, minus the parameter —
  // this route carries no id). Mirrors navigate()'s hash-sync convention.
  function navigateToBudgetsList() {
    route = "budgetsList";
    budgetDetailsId = null;
    const hash = hashForBudgetsList();
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    }
  }

  // Resolves the route a session (fresh login/register via saveSession, or a
  // token-restore via fetchMe) should land on from window.location.hash —
  // shared by both so they apply the same exclusion. A destructive
  // confirmation page (deleteAccount) is never a valid PASSIVE landing
  // target: unlike settings/categories/etc it's only meant to be reached via
  // an explicit menu click (openDeleteAccount). Without this, a shared/
  // bookmarked #/deleteAccount link (saveSession) or a browser tab-restore
  // after closing the app mid-flow (fetchMe) would land straight on the
  // delete-account confirmation with no explicit navigation from the user.
  function resolveSessionStartRoute(hash) {
    const resolved = applyFlaggedRoutes(resolveRoute(hash), { retirementPlannerEnabled });
    if (resolved.route === "deleteAccount") {
      window.location.hash = "";
      return { route: "chat", budgetId: null };
    }
    return resolved;
  }

  // Browser back/forward. Only takes effect once the chat screen is showing —
  // the auth screen has no outlet to route.
  function handleHashChange() {
    if (activeScreen === "chat") {
      const resolved = applyFlaggedRoutes(resolveRoute(window.location.hash), { retirementPlannerEnabled });
      route = resolved.route;
      budgetDetailsId = resolved.budgetId;
    }
  }
  let deleteEmailInput = $state("");
  let deleteBusy = $state(false);

  let canDeleteAccount = $derived(
    !!user &&
      deleteEmailInput.trim().toLowerCase() ===
        (user?.email ?? "").toLowerCase() &&
      !deleteBusy,
  );

  function openDeleteAccount() {
    deleteEmailInput = "";
    navigate("deleteAccount");
  }

  async function confirmDeleteAccount() {
    if (!canDeleteAccount) return;
    deleteBusy = true;
    try {
      const challenge = await fetchApi("/account/delete-challenge", { method: "POST" });
      const credential = await startAuthentication({ optionsJSON: challenge.options });
      await fetchApi("/account", {
        method: "DELETE",
        body: JSON.stringify({
          confirm_email: deleteEmailInput.trim(),
          flow_id: challenge.flow_id,
          credential,
        }),
      });
      // Wipe client state (same set handleLogout resets) — the account is gone
      // server-side, so there's nothing to log out from.
      pendingDeletion = null;
      token = "";
      user = null;
      budgets = [];
      activeBudget = null;
      insights = null;
      conversations = [];
      activeConversationId = null;
      chatMessages = [];
      chatInput = "";
      dictationBase = "";
      promptHistory = [];
      historyCursor = -1;
      menuOpen = false;
      deleteEmailInput = "";
      localStorage.removeItem("nels_token");
      activeScreen = "auth";
      // Same stale-hash concern as handleLogout (see its comment) — reset the
      // route/hash so the next login on this browser doesn't land back on
      // the deleteAccount page.
      navigate("chat");
    } catch (e) {
      triggerError(e.message || $_("deleteAccount.failed"));
    } finally {
      deleteBusy = false;
    }
  }

  // --- Data Fetching ---
  async function fetchMe() {
    try {
      user = await fetchApi("/auth/me");
      activeScreen = "chat";
      const resolvedOnRestore = resolveSessionStartRoute(window.location.hash);
      route = resolvedOnRestore.route;
      budgetDetailsId = resolvedOnRestore.budgetId;
      // Token auto-loaded from localStorage must NOT repopulate old messages.
      startFreshSession();
    } catch (e) {
      handleLogout();
    }
  }

  // Refresh the budget LIST only (active-budget selection). Never loads chat
  // history into the panel — conversation messages are managed separately.
  async function fetchBudgets() {
    try {
      budgets = await fetchApi("/budgets");
      // #255: the viewer's explicit per-viewer preference (is_active, from
      // users.active_budget_id) takes priority over resolveActiveBudget's
      // owner-scoped is_default resolution (#266) below — layered on top of,
      // not merged into, that helper so both fixes compose independently of
      // merge order. resolveActiveBudget (#266) scopes its own fallback to
      // owned rows and already returns null for an empty/all-shared list, so
      // no separate budgets.length branch is needed here.
      const preferred = budgets.find((b) => b.is_active);
      activeBudget = preferred || resolveActiveBudget(budgets);
      // Refresh the status-strip figures whenever a budget is active. Skip the
      // round-trip when there is no active budget (the strip can't show rows
      // anyway). Fire-and-forget — the strip is advisory, never block the list.
      if (activeBudget) {
        loadInsights();
      } else {
        insights = null;
      }
    } catch (e) {
      triggerError(t("alerts.fetchBudgetsFailed"));
    }
  }

  // Active-budget figures for the status strip (#208). Reuses /insights so the
  // traditional row never disagrees with the Insights panel — per-budget
  // budgeted/spent/remaining come from each budget's own time_frame window, so
  // the fixed `this_month` period (which only drives trend/top-categories) does
  // not diverge from whatever period the panel has selected. Failure is
  // non-fatal — the strip degrades to the zero-based row / ready indicator.
  async function loadInsights() {
    try {
      insights = await fetchApi("/insights?period=this_month");
    } catch (err) {
      console.error(err);
      insights = null;
    }
  }

  // The active budget's row from the Insights payload (limit/spent/remaining),
  // matched by id so figures are identical to the Insights panel.
  let activeInsight = $derived(
    activeBudget && insights?.budgets
      ? insights.budgets.find((b) => b.id === activeBudget.id) ?? null
      : null,
  );
  // Zero-based figures from the active budget's server-resolved aggregates (#358).
  // Available = total income and the savings half of Allocated are OWN-only sums
  // (income/savings never roll up — mirror categories are always expense-type);
  // the expense half (aggregated_base_amount) is mirror-inclusive, so a rollup
  // parent folds its children's expenses into Allocated. For a standalone budget
  // all three equal the budget's own amounts. Allocated = savings + expenses;
  // Left = Available − Allocated.
  let zbAvailable = $derived(zeroBasedAvailable(activeBudget));
  let zbAllocated = $derived(zeroBasedAllocated(activeBudget));
  let zbLeft = $derived(zeroBasedLeft(activeBudget));
  // Driven by the ACTIVE budget's own budget_strategy (#300) instead of the removed
  // global user toggles -- exactly one strip renders (or none, transiently, for a
  // limit_spent_remaining budget until /insights resolves -- see loadInsights()).
  let showZb = $derived(
    !!activeBudget && activeBudget.budget_strategy === "zero_based",
  );
  let showTrad = $derived(
    !!activeBudget &&
      activeBudget.budget_strategy === "limit_spent_remaining" &&
      !!activeInsight,
  );

  // POST /budgets/:id/default returns 200 with an EMPTY body, so it cannot go
  // through fetchApi (which always res.json()s a non-204 response). Mirror
  // fetchApi's auth header + 401 handling, but do not parse a body. Uses
  // fetchWithTimeout directly (same helper fetchApi delegates to) so a hung
  // POST /budgets/:id/default still eventually rejects instead of leaving this
  // promise pending forever. See #249. (Owner-only gating is enforced server-side
  // by `Permission::Owner`, #238; there is no client-side caller today — see below.)
  // #391: intentionally unused in the UI today — the budgets-list Switch button moved
  // to setActiveBudget. Kept as the only correct caller shape for POST /budgets/:id/default
  // should an explicit "set my default budget" surface return.
  async function setDefaultBudget(budgetId) {
    const headers = {};
    if (token) headers["Authorization"] = `Bearer ${token}`;
    const res = await fetchWithTimeout(`${API_BASE}/budgets/${budgetId}/default`, {
      method: "POST",
      headers,
    });
    if (res.status === 401) {
      handleLogout();
      throw new Error("Unauthorized");
    }
    if (!res.ok) {
      const errText = await res.text();
      throw new Error(errText || "API error");
    }
  }

  // POST /budgets/:id/activate returns 200 with an EMPTY body (mirrors
  // /budgets/:id/default, #249's raw-fetch pattern) — sets the caller's
  // per-viewer active-budget preference (#255), which the backend allows for
  // ANY accessible budget (owned or shared), unlike setDefaultBudget's
  // owner-only /default. Used by /budgets-switch so a shared budget can be
  // made "active" without ever mutating the owner's is_default row.
  async function setActiveBudget(budgetId) {
    const headers = {};
    if (token) headers["Authorization"] = `Bearer ${token}`;
    const res = await fetchWithTimeout(`${API_BASE}/budgets/${budgetId}/activate`, {
      method: "POST",
      headers,
    });
    if (res.status === 401) {
      handleLogout();
      throw new Error("Unauthorized");
    }
    if (!res.ok) {
      const errText = await res.text();
      // #391: carry the status so BudgetsView can name the cause (403 revoked /
      // 404 deleted) instead of falling back to the generic "couldn't switch".
      const err = new Error(errText || "API error");
      err.status = res.status;
      throw err;
    }
  }

  async function fetchConversations() {
    try {
      conversations = await fetchApi("/conversations");
    } catch (e) {
      // Non-fatal: an empty sidebar is acceptable.
      conversations = [];
    }
  }

  async function fetchSuggestedQuestion() {
    try {
      const res = await fetchApi(`/suggested-question?locale=${currentLocale()}`);
      suggestedQuestion = res?.question || "";
    } catch (e) {
      suggestedQuestion = "";
    }
  }

  // --- Stripe billing (#25) ---
  async function loadSubscription() {
    try {
      subscription = await fetchApi("/billing/subscription");
    } catch (e) {
      // Non-fatal: billing is optional / may be unconfigured. Log so a real
      // misconfiguration is distinguishable from a lagging webhook during debug.
      console.error("loadSubscription failed", e);
    }
  }

  async function startCheckout(cadence, tier = null) {
    try {
      // tier is optional: the backend defaults an absent tier to Pro (legacy /
      // Settings upgrade path). The two-tier marketing deep-link passes it.
      const body = tier ? { cadence, tier } : { cadence };
      const { url } = await fetchApi("/billing/checkout", {
        method: "POST",
        body: JSON.stringify(body),
      });
      if (url) window.location.href = url;
      else triggerError($_("settings.billingError"));
    } catch (e) {
      // Surface the failure (e.g. the 409 "already subscribed" guard, or a
      // network error) via the standard non-fatal error UI.
      triggerError(e.message || $_("settings.billingError"));
    }
  }

  async function openPortal() {
    try {
      const { url } = await fetchApi("/billing/portal", { method: "POST" });
      if (url) window.location.href = url;
      else triggerError($_("settings.billingError"));
    } catch (e) {
      triggerError(e.message || $_("settings.billingError"));
    }
  }

  // Marketing deep-link → start checkout once authed. Two-tier: ?plan=basic|pro
  // &cadence=... (legacy ?upgrade=monthly|annual still supported, tier→Pro).
  // Post-checkout return (?billing=success|cancel) → bounded poll for entitlement
  // (it arrives via the async Stripe webhook, so it may lag the redirect).
  async function handleBillingQueryParams() {
    const params = new URLSearchParams(window.location.search);
    const billing = params.get("billing");
    const checkoutIntent = parseCheckoutDeepLink(window.location.search);
    if (checkoutIntent && token) {
      await startCheckout(checkoutIntent.cadence, checkoutIntent.tier);
      // On success we've navigated to Stripe; on failure clear the param so it
      // doesn't re-fire on the next mount.
      window.history.replaceState({}, "", window.location.pathname);
      return;
    }
    if (billing === "success") {
      // Show the "finalizing" hint immediately (not Free/upgrade) for the whole
      // poll window, so the user can't re-purchase while the async webhook lands.
      billingFinalizing = true;
      for (let i = 0; i < 5; i++) {
        await loadSubscription();
        if (subscription?.is_pro) {
          billingFinalizing = false;
          break;
        }
        await new Promise((r) => setTimeout(r, 1500));
      }
      window.history.replaceState({}, "", window.location.pathname);
    } else if (billing === "cancel") {
      window.history.replaceState({}, "", window.location.pathname);
    }
  }

  // GoCardless consent-flow return (?gc_ref=..., #320): GoCardless redirects
  // the user's browser straight back to our own app_url with this param (see
  // gocardless::start_link_session's `redirect` field) — there is no
  // client-side SDK modal like Stripe's, so this is the ONLY place the
  // frontend learns the consent flow finished. Scrub the param immediately
  // so a reload/back-nav can't re-fire it. `activeBudget` is not yet
  // populated at onMount time (fetchMe()/fetchBudgets() are still in
  // flight), so this awaits fetchBudgets() itself rather than relying on the
  // fire-and-forget call inside startFreshSession().
  async function handleGoCardlessQueryParams() {
    const gcRef = new URLSearchParams(window.location.search).get("gc_ref");
    if (!gcRef) return;
    window.history.replaceState({}, "", window.location.pathname);
    if (!token) return;
    if (!activeBudget) await fetchBudgets();
    if (!activeBudget) return;
    try {
      const { linked } = await completeGcLinkFlow({ budgetId: activeBudget.id, gcRef, fetchApi });
      // Give explicit success feedback symmetric with the failure path below
      // — otherwise a user returning from GoCardless's redirect lands back
      // on the app with zero visible confirmation the link actually worked.
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("gocardless complete failed", e);
      triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
    }
  }

  // Handle a Basiq consent-flow return (#323, Australia). Mirrors
  // handleGoCardlessQueryParams() exactly — Basiq's hosted "Basiq Connect"
  // page redirects back with `?basiq_ref=...` (see basiq::start_link_session's
  // redirect URL) as the sole signal the flow finished.
  async function handleBasiqQueryParams() {
    const basiqRef = new URLSearchParams(window.location.search).get("basiq_ref");
    if (!basiqRef) return;
    window.history.replaceState({}, "", window.location.pathname);
    if (!token) return;
    if (!activeBudget) await fetchBudgets();
    if (!activeBudget) return;
    try {
      const { linked } = await completeBasiqLinkFlow({ budgetId: activeBudget.id, basiqRef, fetchApi });
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("basiq complete failed", e);
      triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
    }
  }

  // Handle an Akahu consent-flow return (#323, New Zealand). Akahu's hosted
  // "Akahu Connect" is OAuth2-based, so its redirect_uri carries both
  // `akahu_ref` (our own session reference) and `code` (the OAuth
  // authorization code) — a missing `code` means the user abandoned or denied
  // consent on Akahu's side, so there is nothing to complete.
  async function handleAkahuQueryParams() {
    const params = new URLSearchParams(window.location.search);
    const akahuRef = params.get("akahu_ref");
    const code = params.get("code");
    const state = params.get("state");
    if (!akahuRef) return;
    window.history.replaceState({}, "", window.location.pathname);
    if (!code) {
      // The user reached our redirect_uri without a `code` — Akahu's
      // hosted flow was abandoned or denied; nothing to complete.
      return;
    }
    // Client-side CSRF/mix-up hardening (Copilot review finding, #323):
    // `create_consent_session` sends `state=<akahu_ref>` to Akahu's
    // authorize endpoint specifically so it round-trips back here. The
    // backend's own ownership check (session.user_id/budget_id) is the real
    // security boundary regardless, but bailing out early on a `state`
    // mismatch avoids even attempting to complete with a mixed-up
    // session/code pairing.
    if (state && state !== akahuRef) {
      console.error("akahu complete failed", "state param does not match akahu_ref — possible CSRF/mix-up, aborting");
      triggerError($_("linkedAccounts.gcCompleteError"));
      return;
    }
    if (!token) return;
    if (!activeBudget) await fetchBudgets();
    if (!activeBudget) return;
    try {
      const { linked } = await completeAkahuLinkFlow({ budgetId: activeBudget.id, akahuRef, code, fetchApi });
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("akahu complete failed", e);
      triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
    }
  }

  // Plaid OAuth-redirect return (#321): Big-5 Canadian institutions navigate
  // the browser away to their own OAuth/Interac login and back to
  // PLAID_REDIRECT_URI with an oauth_state_id param. Per Plaid's documented
  // pattern, resuming requires the SAME link_token used to open the
  // original session — persisted in localStorage before the navigation (see
  // LinkedAccounts.svelte's link() CA branch) since a fresh page load has
  // lost all in-memory state.
  async function handlePlaidOAuthReturn() {
    if (!isPlaidOAuthReturn(window.location.search)) return;
    // Capture BEFORE replaceState scrubs the query string — receivedRedirectUri
    // must be the URL Plaid actually redirected back to (including
    // oauth_state_id), not the scrubbed one (code review: an earlier version
    // of this function read window.location.href AFTER the scrub, which
    // would have silently broken every Big-5 OAuth resume).
    const redirectUri = window.location.href;
    const stored = window.localStorage.getItem(PLAID_LINK_RESUME_KEY);
    window.localStorage.removeItem(PLAID_LINK_RESUME_KEY);
    window.history.replaceState({}, "", window.location.pathname);
    // parsePlaidLinkResume never throws (unlike a raw JSON.parse) — a
    // corrupted/malformed stored value just abandons the resume attempt
    // instead of becoming an unhandled promise rejection in this
    // fire-and-forget onMount call (code review).
    const resume = parsePlaidLinkResume(stored);
    if (!resume || !token) return;
    const { linkToken, sessionId, budgetId } = resume;
    // Copilot review finding (#321): `resume` can be a truthy object
    // (parsePlaidLinkResume only rejects non-object JSON) while still
    // missing linkToken/sessionId — e.g. corrupted localStorage. Without
    // this check, a falsy `linkToken` would make startPlaidLinkFlow create a
    // BRAND NEW link token while still passing `receivedRedirectUri`, which
    // violates Plaid's "resume with the same link_token" requirement and can
    // silently break Big-5 OAuth resumption. Abandon the resume attempt
    // instead — same "never throws, just gives up" philosophy as
    // parsePlaidLinkResume itself.
    if (!linkToken || !sessionId) return;
    if (!activeBudget) await fetchBudgets();
    if (!activeBudget) return;
    try {
      const { linked } = await startPlaidLinkFlow({
        budgetId: budgetId || activeBudget.id, fetchApi, loadPlaidLink,
        linkToken, sessionId, receivedRedirectUri: redirectUri,
      });
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("plaid oauth resume failed", e);
      triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
    }
  }

  // --- Slash-command execution (#56) ---
  // Dispatch a typed "/command" instead of sending it to the AI. Each command
  // echoes the user's input, then pushes an assistant-style feedback message
  // (success, usage hint, or error) so the chat thread stays the single source
  // of truth for command results.
  async function runCommand(rawInput) {
    const parsed = parseCommand(rawInput);
    if (!parsed) return;

    // Echo the command as a user message for traceability.
    chatMessages = [
      ...chatMessages,
      {
        id: Math.random().toString(),
        sender: "user",
        message_text: rawInput,
        created_at: new Date().toISOString(),
      },
    ];

    if (knownCommandName(rawInput) === null) {
      pushAiMessage(t("commands.unknown", { values: { name: parsed.name } }));
      return;
    }

    if (parsed.name === "/clear") {
      // Non-destructive: start a fresh conversation view client-side. The
      // server-side thread is preserved (reachable via the sidebar); only the
      // active view is reset, mirroring the "New conversation" affordance.
      chatMessages = [];
      showAllMessages = false;
      activeConversationId = null;
      fetchSuggestedQuestion();
      pushAiMessage(t("commands.clearDone"));
      return;
    }

    if (parsed.name === "/issues-list") {
      isChatLoading = true;
      try {
        const issues = await fetchApi("/github/issues");
        if (!issues || issues.length === 0) {
          pushAiMessage(t("commands.issuesNone"));
        } else {
          const lines = issues
            .map((i) => `- #${i.number} ${i.title} (${i.state})`)
            .join("\n");
          pushAiMessage(`${t("commands.issuesListHeader")}\n${lines}`);
        }
      } catch (err) {
        pushAiMessage(commandErrorMessage(err));
      } finally {
        isChatLoading = false;
      }
      return;
    }

    if (parsed.name === "/issues-create") {
      const { title, body } = parseIssueArgs(parsed.args);
      if (!title) {
        pushAiMessage(t("commands.issuesCreateUsage"));
        return;
      }
      isChatLoading = true;
      try {
        const created = await fetchApi("/github/issues", {
          method: "POST",
          body: JSON.stringify({ title, body }),
        });
        pushAiMessage(
          t("commands.issuesCreated", {
            values: {
              number: created.number,
              url: created.html_url,
            },
          }),
        );
      } catch (err) {
        pushAiMessage(commandErrorMessage(err));
      } finally {
        isChatLoading = false;
      }
      return;
    }

    if (parsed.name === "/budgets-list") {
      navigateToBudgetsList();
      pushAiMessage(t("commands.budgetsListOpened"));
      return;
    }

    if (parsed.name === "/tokens") {
      isChatLoading = true;
      try {
        const stats = await fetchApi("/user/token-stats");
        const nf = new Intl.NumberFormat(currentLocale());
        pushAiMessage(
          t("commands.tokensResult", {
            values: {
              input: nf.format(stats.input_tokens ?? 0),
              thinking: nf.format(stats.thinking_tokens ?? 0),
              output: nf.format(stats.output_tokens ?? 0),
              total: nf.format(stats.total_tokens ?? 0),
            },
          }),
        );
      } catch (err) {
        pushAiMessage(t("commands.tokensUnavailable"));
      } finally {
        isChatLoading = false;
      }
      return;
    }

    if (parsed.name === "/categories-list") {
      if (!activeBudget) {
        pushAiMessage(t("commands.noActiveBudget"));
        return;
      }
      pushAiMessage(t("commands.categoriesOpened"));
      navigate("categories");
      return;
    }

    if (parsed.name === "/transactions-list") {
      if (!activeBudget) {
        pushAiMessage(t("commands.noActiveBudget"));
        return;
      }
      pushAiMessage(t("commands.transactionsOpened"));
      navigate("transactions");
      return;
    }

    if (parsed.name === "/budgets-insights") {
      navigate("insights");
      pushAiMessage(t("commands.insightsOpened"));
      return;
    }

    if (parsed.name === "/budgets-switch") {
      const resolution = resolveBudgetByName(budgets, parsed.args);
      if (resolution.status === "empty") {
        pushAiMessage(t("commands.switchUsage"));
        return;
      }
      if (resolution.status === "unknown") {
        pushAiMessage(t("commands.switchUnknown", { values: { name: parsed.args } }));
        return;
      }
      if (resolution.status === "ambiguous") {
        pushAiMessage(t("commands.switchAmbiguous", { values: { name: parsed.args } }));
        return;
      }
      const target = resolution.budget;
      if (target.id === activeBudget?.id) {
        pushAiMessage(t("commands.switchAlready", { values: { name: target.name } }));
        return;
      }
      // #255: any budget in `budgets` is, by construction, one the viewer can
      // already access (list_budgets only returns owned + shared-with-you
      // rows) — the backend's check_permission != None gate on /activate is
      // the actual authority, so no client-side ownership gate is needed
      // here.
      isChatLoading = true;
      try {
        await setActiveBudget(target.id);
        // fetchBudgets() handles its own errors with a toast and does NOT
        // rethrow, so a failed refresh would otherwise still print the success
        // message over stale state. Confirm the active budget actually reflects
        // the switch before claiming success; otherwise surface the refresh gap.
        await fetchBudgets();
        if (activeBudget && activeBudget.id === target.id) {
          pushAiMessage(
            t("commands.switchDone", { values: { name: target.name } }),
          );
        } else {
          pushAiMessage(
            t("commands.switchRefreshFailed", {
              values: { name: target.name },
            }),
          );
        }
      } catch (err) {
        pushAiMessage(commandErrorMessage(err));
      } finally {
        isChatLoading = false;
      }
      return;
    }
  }

  // Map a command failure to a user-facing message. The backend returns a
  // clear "not configured" body (503) for the GitHub commands; surface it
  // verbatim, else a generic fallback.
  function commandErrorMessage(err) {
    const msg = (err && err.message) || "";
    if (msg.includes("not configured")) {
      return t("commands.notConfigured");
    }
    return t("commands.failed");
  }

  // --- AI Copilot Handlers ---
  // Runs ONE already-dequeued item (or the very first, non-busy send) through the actual send
  // path: slash-command interception, or the optimistic-bubble + POST /chat round trip. Never
  // reads or clears `chatInput` — callers (sendChatMessage / drainQueue) own that. Never throws —
  // both branches below catch their own failures — so drainQueue's while loop can never be
  // wedged by an item that fails (#230).
  async function performSend(rawInput) {
    // Sending is a deliberate action: re-pin so the user's own message, the
    // loading indicator, and the reply scroll into view even if they had
    // scrolled up to read history (AC 2). The scroll-up guard still applies to
    // passively-arriving replies, not to content the user just submitted. #108
    // Re-pinning per dequeued item (not just the first) keeps this true across
    // an entire queue drain, not only the send that started it.
    pinnedToBottom = true;
    showAllMessages = false;
    // Always return to the chat transcript on a deliberate submit (#278) —
    // otherwise a reply appended while an outlet view (categories/insights/
    // budgetDetails/budgetsList/history/settings/deleteAccount) is showing is
    // invisible behind that view. Runs before slash-command interception and
    // before the /chat round-trip below, so any navigation the response
    // itself triggers (a navigating slash-command, or open_insights /
    // open_budgets_list / categories_table_html further down this function)
    // still runs afterward and wins — this is only the default landing spot,
    // not an override of an explicit navigation. navigate("chat") is a no-op
    // on window.location.hash when already on chat (it compares before
    // assigning), so this adds no spurious browser-history entry for the
    // common case of submitting from chat itself.
    navigate("chat");

    // Intercept slash-commands before they reach the AI endpoint.
    if (isCommand(rawInput)) {
      recordPrompt(rawInput);
      try {
        await runCommand(rawInput);
      } catch (err) {
        // Defensive: every existing runCommand branch already catches its own network calls, so
        // this should never actually trigger — but performSend must never be a throw point
        // either way, or a queued command failure would wedge the rest of the drain.
        console.error("performSend: runCommand threw unexpectedly", err);
        pushAiMessage(commandErrorMessage(err));
      }
      return;
    }

    recordPrompt(rawInput);

    // A fresh message supersedes any pending category choice from a prior turn
    // so stale chips never linger under an old message (#376).
    pendingCategoryChoice = null;

    // Optimistically push user message
    chatMessages = [
      ...chatMessages,
      {
        id: Math.random().toString(),
        sender: "user",
        message_text: rawInput,
        created_at: new Date().toISOString(),
      },
    ];

    isChatLoading = true;

    try {
      const chatRes = await fetchApi("/chat", {
        method: "POST",
        body: JSON.stringify({
          message: rawInput,
          budget_id: activeBudget ? activeBudget.id : null,
          conversation_id: activeConversationId,
          locale: currentLocale(),
        }),
      });

      // Track the thread the backend used (created lazily on first send).
      activeConversationId = chatRes.conversation_id;

      // Push AI reply (keep optimistic messages; never reload from history)
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: chatRes.response,
          created_at: new Date().toISOString(),
          // nels#301: a CATEGORY_BALANCE answer must show the limit/spent/remaining
          // figures inline in the chat transcript (rule 20c tells the model not to
          // state them itself in response_text, since they come from this table
          // instead). inlineCategoriesTableHtml returns null for LIST_CATEGORIES
          // (which navigates away instead — its table stays discarded, unchanged
          // from #233's existing convention).
          categories_table_html: inlineCategoriesTableHtml(chatRes),
          // nels-oss#3: the user's own AI key failed this turn (auth_rejected |
          // rate_limited | unavailable | key_unavailable). The backend's
          // response text stays as sent; the bubble adds a link to Settings.
          aiProviderError: chatRes.ai_provider_error ?? null,
        },
      ];

      // Keep the header / active-budget display in sync after a chat-driven
      // change. The header binds the active budget's name + status badges
      // (project/closed/archived/rollover), all of which are budget-row fields
      // that an in-place edit (rename, limit/rollover, close, archive) changes
      // WITHOUT changing the budget id — so the previous id-only gate left the
      // header stale until a manual reload (issue #87). Refresh the budget LIST
      // (never chat history — that would clobber the thread) when EITHER:
      //   - the active budget switched / was created (id differs / no active), OR
      //   - the chat reported any mutation (`action_taken`). The chat response
      //     exposes only a human-readable mutation log, not the action type, so
      //     we cannot cheaply tell a header-relevant edit from an unrelated one
      //     (e.g. logging an expense). We deliberately re-fetch on any mutation:
      //     `/budgets` is a small list query already issued on switch/delete, and
      //     guaranteed header correctness is worth one extra GET over coupling
      //     this gate to the backend's log wording. Pure Q&A turns leave
      //     `action_taken` null and skip the fetch.
      const budgetSwitched =
        chatRes.budget_id &&
        (!activeBudget || activeBudget.id !== chatRes.budget_id);
      if (budgetSwitched || chatRes.action_taken) {
        await fetchBudgets();
        // A mutation (e.g. logging an expense) can trip a budget/category limit
        // alert server-side — re-poll the feed so the badge updates promptly
        // instead of waiting for the 60s poll.
        notificationsRef?.refresh();
      }

      // The assistant never deletes directly; it proposes a deletion and we
      // confirm it through a custom modal before calling the REST endpoint.
      if (chatRes.pending_deletion) {
        pendingDeletion = chatRes.pending_deletion;
      }

      // #376: when Nels would auto-create a brand-new category for an
      // un-categorized transaction, it returns a choice instead of logging.
      // Surface it as inline chips; the transaction is only created once the
      // user resolves the choice via finalize (see onCategoryChoiceChip).
      if (chatRes.pending_category_choice) {
        pendingCategoryChoice = chatRes.pending_category_choice;
      }

      // The assistant's LINK_BANK_ACCOUNT action (#303) hands back a Stripe
      // Financial Connections session client_secret instead of mutating
      // anything itself — open Stripe.js's hosted modal here, then tell our
      // backend the session completed so it can persist the linked
      // account(s). `chatRes.budget_id` (not `activeBudget?.id`) is used
      // because it's the exact budget id the backend ran this action
      // against (see action_outcome_budget_id in rag.rs), which stays
      // correct even if the active budget were to change between request
      // and response.
      //
      // This calls the same startLinkFlow() the Accounts page's "Link
      // account" button uses (linkedAccounts.js), passing the client_secret
      // we already have so it skips its own POST /session (that would open
      // a second, redundant Financial Connections session). Sharing one
      // function keeps AGENTS.md's "share one code path" claim true and
      // gives this flow the same tested error-vs-cancel handling. Wrapped
      // in its own try/catch: this must not fall through to the outer
      // catch's generic "unable to connect to my AI node" message, which
      // would be actively wrong here since the chat request already
      // succeeded and the assistant's reply is already showing (#303 code
      // review).
      if (chatRes.financial_connections_client_secret) {
        try {
          const { loadStripe } = await import("@stripe/stripe-js");
          // The return value (linked accounts, if any) needs no handling
          // here — a plain user-cancel resolves with an empty list and
          // needs no feedback, matching the prior behavior. Only a thrown
          // error (a real Stripe failure) needs surfacing, below.
          await startLinkFlow({
            budgetId: chatRes.budget_id,
            fetchApi,
            loadStripe,
            publishableKey: import.meta.env.VITE_STRIPE_PUBLISHABLE_KEY,
            clientSecret: chatRes.financial_connections_client_secret,
          });
        } catch (e) {
          pushAiMessage(e.message || t("linkedAccounts.linkError"));
        }
      }

      // GoCardless's LINK_BANK_ACCOUNT chat action (#320): unlike Stripe's
      // client-side-SDK-modal flow above, GoCardless's consent flow is a
      // full-page redirect — there is no client_secret to hand to an SDK,
      // just a URL to navigate the browser to. The user's session on
      // GoCardless's/the bank's own site ends with a redirect back to this
      // app (`?gc_ref=...`), handled by handleGoCardlessQueryParams() on
      // mount, same as the REST-button flow in LinkedAccounts.svelte.
      if (chatRes.bank_link_redirect_url) {
        window.location.href = chatRes.bank_link_redirect_url;
      }

      // Belvo's LINK_BANK_ACCOUNT chat action (#322): unlike GoCardless's
      // full-page redirect and Stripe's SDK modal, Belvo's flow is a
      // client-side embeddable widget rendered INLINE — there is no
      // navigation away and no return-trip query param to pick up on mount.
      if (chatRes.belvo_widget_access_token && chatRes.belvo_link_session_id) {
        belvoWidgetActive = true;
        try {
          await loadBelvoWidget(chatRes.belvo_widget_access_token, {
            onSuccess: async (belvoLinkId) => {
              belvoWidgetActive = false;
              try {
                const { linked } = await completeBelvoLinkFlow({
                  budgetId: chatRes.budget_id,
                  sessionId: chatRes.belvo_link_session_id,
                  belvoLinkId,
                  fetchApi,
                });
                if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
              } catch (e) {
                console.error("belvo complete failed", e);
                triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
              }
            },
            onExit: () => { belvoWidgetActive = false; },
          });
        } catch (e) {
          pushAiMessage(e.message || $_("linkedAccounts.linkError"));
          belvoWidgetActive = false;
        }
      }

      // Plaid's LINK_BANK_ACCOUNT chat action (#321): Plaid Link is a
      // client-side modal (like Stripe's/Belvo's above), not a redirect
      // (like GoCardless/Basiq/Akahu above) — but Big-5 Canadian banks can
      // still navigate away for their own OAuth login, so the link_token/
      // session_id must be persisted to localStorage BEFORE opening the
      // modal, exactly mirroring LinkedAccounts.svelte's CA branch, so
      // handlePlaidOAuthReturn can resume on return.
      if (chatRes.plaid_link_token) {
        try {
          window.localStorage.setItem(PLAID_LINK_RESUME_KEY, JSON.stringify({
            linkToken: chatRes.plaid_link_token,
            sessionId: chatRes.plaid_link_session_id,
            budgetId: chatRes.budget_id,
          }));
          try {
            await startPlaidLinkFlow({
              budgetId: chatRes.budget_id,
              fetchApi,
              loadPlaidLink,
              linkToken: chatRes.plaid_link_token,
              sessionId: chatRes.plaid_link_session_id,
            });
          } finally {
            // See the CA branch in LinkedAccounts.svelte for why this only
            // ever runs on an in-modal settle, never on an actual OAuth
            // redirect (the page navigates away before this promise settles).
            window.localStorage.removeItem(PLAID_LINK_RESUME_KEY);
          }
        } catch (e) {
          pushAiMessage(e.message || t("linkedAccounts.linkError"));
        }
      }

      // Navigate to the insights outlet when the assistant signals it (#157,
      // #233). Mirrors the sidebar path, which calls the same navigate().
      // Advisory only — no data changes; this just surfaces the existing
      // insights view.
      if (chatRes.open_insights) {
        navigate("insights");
      }

      // Navigate to the retirement planner outlet when the assistant signals a
      // RETIREMENT_PROJECTION turn (#469). Mirrors the open_insights handling
      // above; advisory only — the retirement view re-fetches everything on
      // mount.
      if (chatRes.open_retirement) {
        navigate("retirement");
      }

      // Navigate to the budgets list outlet when the assistant signals an
      // OPEN_BUDGETS_LIST turn (#241). Mirrors the open_insights handling
      // immediately above.
      if (chatRes.open_budgets_list) {
        navigateToBudgetsList();
      }

      // Navigate to the categories outlet when the assistant signals a
      // LIST_CATEGORIES turn (#233). Gated on the dedicated open_categories_list
      // flag (nels#301) rather than the mere presence of categories_table_html —
      // CATEGORY_BALANCE also populates that HTML field (a single-category table)
      // but must answer inline in the chat transcript instead of navigating away
      // (see the message push above: inlineCategoriesTableHtml already attached
      // that table to the just-pushed AI message for rendering in the bubble).
      // For LIST_CATEGORIES specifically the HTML is never attached to a message
      // (inlineCategoriesTableHtml returns null when this flag is set) and is
      // simply discarded here — CategoriesView re-fetches its own fresh copy on
      // mount, the same convention Insights already uses (self-fetch via
      // fetchApi, no data via props).
      if (chatRes.open_categories_list) {
        navigate("categories");
      }

      // Navigate to the transactions outlet when the assistant signals an
      // (unscoped) LIST_TRANSACTIONS turn (#376). Mirrors open_categories_list.
      if (chatRes.open_transactions_list) {
        navigate("transactions");
      }

      // Surface the new thread / generated title in the sidebar.
      await fetchConversations();
    } catch (e) {
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: `⚠️ I was unable to connect to my AI node. Error: ${e.message}. If offline, try asking standard budgeting queries!`,
          created_at: new Date().toISOString(),
        },
      ];
    } finally {
      isChatLoading = false;
    }
  }

  // Drains pendingQueue strictly FIFO, one item at a time, via the shared chatQueue.js runDrain
  // helper. Guarded by isDraining so a fire-and-forget call from sendChatMessage while a drain is
  // already running is a safe no-op — the already-running while loop picks up a newly-queued item
  // (or a mid-drain cancel/clear) on its next iteration because runDrain re-reads the LIVE
  // `pendingQueue` via the `() => pendingQueue` accessor every iteration, never a cached snapshot
  // (see chatQueue.js's runDrain doc comment — this is the exact bug a plan review caught in an
  // earlier draft that passed a plain array instead). try/finally is defense-in-depth alongside
  // runDrain's own internal catch: isDraining must reset even if performSend somehow throws (#230
  // spec review).
  //
  // The textarea is no longer `disabled` during a turn (#230), so — unlike the old single-send
  // path — it never loses focus mid-send and doesn't need the tick()-then-refocus workaround
  // that used to live in a `finally` here (issue #104; removed as a consequence of #230). One
  // gap that workaround incidentally covered and this one doesn't: a user who clicks the Send
  // button with the mouse (rather than pressing Enter) leaves focus on the button itself, and
  // nothing moves it back afterward. Restore it once per full drain (not per item — refocusing
  // mid-drain would fight a user who is still typing the next message), guarded the same way
  // the old workaround was.
  async function drainQueue() {
    if (isDraining) return;
    isDraining = true;
    try {
      await runDrain(
        () => pendingQueue,
        performSend,
        (rest) => {
          pendingQueue = rest;
        },
      );
    } finally {
      isDraining = false;
      await tick();
      if (!pendingDeletion && route === "chat") chatInputEl?.focus();
    }
  }

  // Entry point for both the form submit and Enter-to-send. Every call — whether Nels is idle or
  // mid-turn — enqueues the trimmed input and (re-)kicks off drainQueue(); when idle, the queue
  // was empty so drainQueue's first loop iteration dequeues and sends this item immediately
  // (synchronously, before any paint), which is why a direct send never flashes a "1 queued"
  // indicator. This unifies the "not busy" and "busy" cases into one mechanism instead of two
  // (#230).
  async function sendChatMessage(e) {
    if (e) e.preventDefault();
    const raw = chatInput.trim();
    if (!raw) return;
    if (!canEnqueue(pendingQueue)) return; // queue cap hit; leave the composer text untouched
    chatInput = "";
    pendingQueue = enqueue(pendingQueue, Math.random().toString(), raw);
    drainQueue();
  }

  function removeQueuedMessage(id) {
    pendingQueue = removeFromQueue(pendingQueue, id);
  }

  function clearQueuedMessages() {
    pendingQueue = [];
  }

  function handlePromptChip(promptText) {
    chatInput = promptText;
    sendChatMessage();
  }

  // User tapped one of the inline category-choice chips (#376). An existing or
  // "create new" chip resolves the pending transaction via the finalize
  // endpoint (which logs it with an embedding, flagging a created category
  // auto_created); the "Something else…" escape just clears the chips and
  // focuses the input so the user can name a category in a fresh message.
  async function onCategoryChoiceChip(chip) {
    const choice = pendingCategoryChoice;
    if (!choice) return;
    if (chip.kind === "freetype") {
      pendingCategoryChoice = null;
      chatInputEl?.focus();
      return;
    }
    const payload = finalizePayloadFor(choice, chip);
    if (!payload) return;
    choiceSubmitting = true;
    try {
      const res = await fetchApi(
        `/budgets/${choice.budget_id}/transactions/finalize`,
        { method: "POST", body: JSON.stringify(payload) },
      );
      // Only clear the chips once the transaction is actually logged.
      pendingCategoryChoice = null;
      pushAiMessage(
        t("categoryChoice.logged", {
          values: {
            amount: fmtMoney(choice.amount),
            name: res?.transaction?.category_name ?? "",
          },
        }),
      );
      await fetchBudgets();
      notificationsRef?.refresh();
    } catch (e) {
      // Keep the chips mounted so the user can retry, and surface the real
      // server error instead of a generic transient-sounding message.
      console.error("finalize category choice failed", e);
      pushAiMessage(
        e?.message
          ? `${t("categoryChoice.error")} (${e.message})`
          : t("categoryChoice.error"),
      );
    } finally {
      choiceSubmitting = false;
    }
  }

  // Append an assistant-style message to the chat thread (used for delete
  // confirm/cancel/error feedback that originates on the client).
  function pushAiMessage(text) {
    chatMessages = [
      ...chatMessages,
      {
        id: Math.random().toString(),
        sender: "ai",
        message_text: text,
        created_at: new Date().toISOString(),
      },
    ];
  }

  // User confirmed the pending deletion in the modal: actually perform it.
  async function confirmDeletion() {
    const pd = pendingDeletion;
    pendingDeletion = null;
    if (!pd) return;
    let endpoint;
    if (pd.kind === "category") {
      endpoint = `/budgets/${pd.budget_id}/categories/${pd.id}`;
    } else if (pd.kind === "transaction") {
      endpoint = `/budgets/${pd.budget_id}/transactions/${pd.id}`;
    } else {
      endpoint = `/budgets/${pd.id}`;
    }

    // Perform the actual delete. A 404 means the row is already gone (delete
    // endpoints 404 rather than 204 in that case), so "please try again" would
    // be a retry that can never succeed — the honest answer is that it was
    // already deleted (#494). The raw response body is NEVER interpolated into
    // user-facing copy; only the status decides the message, per the
    // formMessageFor convention in categoriesView.js.
    let deleted = false;
    try {
      await fetchApi(endpoint, { method: "DELETE" });
      deleted = true;
    } catch (e) {
      console.error("delete failed", e);
      pushAiMessage(
        e?.status === 404
          ? t("confirmDelete.alreadyGone", { values: { name: pd.name } })
          : t("confirmDelete.failed", { values: { name: pd.name } }),
      );
    }

    // Refresh the list on BOTH paths: after a success the deleted row must
    // disappear, and after a 404 the stale row still on screen is exactly what
    // invites the doomed retry. fetchBudgets swallows its own failures into the
    // "Failed to fetch budgets list" alert, so a refresh failure after a real
    // delete is surfaced separately — never as a failed deletion.
    await fetchBudgets();

    if (deleted) {
      pushAiMessage(t("confirmDelete.success", { values: { name: pd.name } }));
    }
  }

  // User dismissed the modal: nothing is deleted.
  function cancelDeletion() {
    const pd = pendingDeletion;
    pendingDeletion = null;
    if (pd) {
      pushAiMessage(t("confirmDelete.cancelled", { values: { name: pd.name } }));
    }
  }

  // Toggle voice dictation. Live transcript is appended to whatever the user had
  // already typed; the text stays in the box for review before sending.
  function toggleDictation() {
    if (dictation.listening) {
      dictation.stop();
      return;
    }
    dictationBase = chatInput.trim() ? chatInput.trim() + " " : "";
    dictation.start(currentLocale(), {
      onResult: (text) => {
        chatInput = dictationBase + text;
      },
    });
  }

  // --- Menu / conversation handlers ---
  function handleNewConversation() {
    chatMessages = [];
    showAllMessages = false;
    activeConversationId = null;
    fetchSuggestedQuestion();
    closeMenu();
  }

  // Reveal the full transcript for the current conversation. Older bubbles are
  // withheld from the DOM by default to keep long mobile sessions responsive.
  // The full transcript is already in chatMessages (loaded when the thread was
  // opened, or accumulated during this session) — windowing only hides the
  // older bubbles from rendering — so we reveal in place without re-fetching.
  // Re-fetching here would race with an in-flight send and could clobber the
  // optimistically-appended message. Drop the scroll pin so revealing does not
  // yank the view back to the newest message.
  function showEarlierMessages() {
    pinnedToBottom = false;
    showAllMessages = true;
  }

  async function handleRenameConversation(id, title) {
    try {
      await fetchApi(`/conversations/${id}`, {
        method: "PATCH",
        body: JSON.stringify({ title }),
      });
      await fetchConversations();
    } catch (e) {
      triggerError(t("alerts.renameConversationFailed"));
    }
  }

  // Stage a conversation deletion: open the confirmation modal instead of
  // deleting immediately, to guard against accidental loss of message history.
  function requestDeleteConversation(id, name) {
    pendingConversationDeletion = { id, name };
  }

  // Dismiss the confirmation modal without deleting anything.
  function cancelConversationDeletion() {
    pendingConversationDeletion = null;
  }

  // User confirmed in the modal: perform the owner-scoped DELETE. If the active
  // conversation was the one removed, fall back to a fresh conversation.
  async function confirmConversationDeletion() {
    const pcd = pendingConversationDeletion;
    pendingConversationDeletion = null;
    if (!pcd) return;
    try {
      await fetchApi(`/conversations/${pcd.id}`, { method: "DELETE" });
      if (pcd.id === activeConversationId) {
        chatMessages = [];
        showAllMessages = false;
        activeConversationId = null;
        fetchSuggestedQuestion();
      }
      await fetchConversations();
    } catch (e) {
      triggerError(t("alerts.deleteConversationFailed"));
    }
  }

  function openMenu() {
    menuOpen = true;
    // MainMenu.svelte moves focus to its own first row once rendered (its
    // own $effect on `open`) — nothing to do here.
  }

  function closeMenu() {
    if (!menuOpen) return;
    menuOpen = false;
    setTimeout(() => hamburgerEl?.focus(), 50);
  }

  function toggleMenu() {
    if (menuOpen) closeMenu();
    else openMenu();
  }

  function handleKeydown(e) {
    if (e.key === "Escape" && pendingDeletion) {
      // Dismissing the confirmation defaults to NOT deleting.
      cancelDeletion();
    } else if (e.key === "Escape" && pendingConversationDeletion) {
      // Dismissing the confirmation defaults to NOT deleting.
      cancelConversationDeletion();
    } else if (e.key === "Escape" && activeScreen === "chat" && route !== "chat") {
      // Esc returns to the chat view from the categories/insights outlet
      // (takes priority over the sidebar). Also gated on activeScreen==="chat"
      // (mirroring handleHashChange's guard) — route can be left stale at
      // "insights"/"categories" after a logout, and without this guard Escape
      // would needlessly call navigate() (mutating the hash) while the auth
      // screen is showing, where there's no outlet to return to.
      navigate("chat");
    } else if (e.key === "Escape" && menuOpen) {
      closeMenu();
    }
  }

  // --- UI Helpers ---
  function triggerError(msg) {
    errorAlert = msg;
    setTimeout(() => {
      errorAlert = "";
    }, 6000);
  }

  function triggerSuccess(msg) {
    successAlert = msg;
    setTimeout(() => {
      successAlert = "";
    }, 6000);
  }

  onMount(() => {
    if (token) {
      fetchMe();
    }
    // Handle a marketing upgrade deep-link / post-checkout return (#25). Safe to
    // call unauthenticated — it no-ops without the relevant query params, and the
    // ?upgrade path is gated on `token`.
    handleBillingQueryParams();
    // Handle a GoCardless consent-flow return (#320). Safe to call
    // unauthenticated/without the param — it no-ops immediately in both cases.
    handleGoCardlessQueryParams();
    // Handle Basiq/Akahu consent-flow returns (#323). Same no-op-safe
    // contract as handleGoCardlessQueryParams() above.
    handleBasiqQueryParams();
    handleAkahuQueryParams();
    // Handle a Plaid Big-5 OAuth-redirect return (#321). Same no-op-safe
    // contract as the handlers above.
    handlePlaidOAuthReturn();
  });

  // True when the chat view is at/near the bottom. Updated by the container's
  // onscroll handler so we capture the user's intent BEFORE a new message grows
  // scrollHeight — reading distance inside the post-update $effect would mis-read
  // it as "scrolled away". #108
  const NEAR_BOTTOM_PX = 80;
  let pinnedToBottom = $state(true);
  let prevScreen = activeScreen;

  function chatNearBottom(el) {
    return el.scrollHeight - el.scrollTop - el.clientHeight <= NEAR_BOTTOM_PX;
  }

  function handleChatScroll(e) {
    // Only the chat view's own scroll position should affect pinnedToBottom —
    // #chat-scroll-area is shared by the categories/insights outlet views too
    // (#233), and scrolling through THEIR content must not be misread as the
    // user scrolling the chat transcript.
    if (route !== "chat") return;
    pinnedToBottom = chatNearBottom(e.currentTarget);
  }

  // tick() flushes the pending DOM update; rAF lets layout reflect the grown
  // bubble before we read scrollHeight. Fire-and-forget at every call site. #108
  async function scrollChatToBottom() {
    await tick();
    requestAnimationFrame(() => {
      const el = document.getElementById("chat-scroll-area");
      if (el) el.scrollTop = el.scrollHeight;
    });
  }

  function scrollChatToBottomIfPinned() {
    if (pinnedToBottom) scrollChatToBottom();
  }

  // Re-pin + jump to bottom only on an actual transition INTO the chat screen.
  // Do not read chatMessages.length here: doing so would re-fire this effect on
  // every append and force pinnedToBottom = true, defeating the scroll-up guard. #108
  $effect(() => {
    const entering = activeScreen === "chat" && prevScreen !== "chat";
    prevScreen = activeScreen;
    if (entering) {
      pinnedToBottom = true;
      scrollChatToBottom();
    }
  });

  // Auto-scroll on new content, but only while pinned. The two reads below are
  // REQUIRED $effect dependencies — keep them as explicit assignments so a linter
  // or formatter does not drop them as "unused" and silently break auto-scroll. #108
  $effect(() => {
    const _len = chatMessages.length;
    const _loading = isChatLoading;
    void _len;
    void _loading;
    // Only auto-scroll #chat-scroll-area to the bottom while it's actually
    // showing the chat transcript — a message arriving while the categories/
    // insights outlet view is showing must not yank that view's scroll
    // position (#233; #chat-scroll-area is shared by all three outlet views).
    if (route === "chat") scrollChatToBottomIfPinned();
  });
</script>

<svelte:window onkeydown={handleKeydown} onhashchange={handleHashChange} />

<!-- App shell -->
<InstallPrompt />

<!-- Destructive-action confirmation. Deletions proposed by the assistant are
     never executed until the user confirms here (custom modal, not the browser's). -->
{#if pendingDeletion}
  <div class="modal modal-open" role="alertdialog" aria-modal="true">
    <div class="modal-box bg-base-200 border border-error/40 max-w-md">
      <div class="flex items-center gap-2 text-error">
        <AlertTriangle class="w-5 h-5 shrink-0" />
        <h3 class="font-bold text-lg">{$_("confirmDelete.title")}</h3>
      </div>
      <p class="py-4 text-base-content/80">
        {pendingDeletion.kind === "budget"
          ? $_("confirmDelete.budgetBody", {
              values: { name: pendingDeletion.name },
            })
          : pendingDeletion.kind === "transaction"
            ? $_("confirmDelete.transactionBody", {
                values: { name: pendingDeletion.name },
              })
            : $_("confirmDelete.categoryBody", {
                values: { name: pendingDeletion.name },
              })}
      </p>
      <div class="modal-action">
        <button class="btn btn-ghost" onclick={cancelDeletion}
          >{$_("common.cancel")}</button
        >
        <button class="btn btn-error" onclick={confirmDeletion}
          >{$_("confirmDelete.confirm")}</button
        >
      </div>
    </div>
    <button
      class="modal-backdrop"
      aria-label={$_("common.cancel")}
      onclick={cancelDeletion}
    ></button>
  </div>
{/if}

<!-- Belvo's embeddable Connect Widget (#322), triggered by the assistant's
     LINK_BANK_ACCOUNT chat action for a Mexico/Brazil budget. Mirrors the
     modal shell above (modal/modal-box) but has no confirm/cancel actions of
     its own — the widget itself calls back into onSuccess/onExit (wired in
     the chatRes.belvo_widget_access_token handling above), which flips
     belvoWidgetActive off. ASSUMED (not fully verified against a live
     account — see the spec's §8 "Widget DOM-mounting mode" risk):
     createWidget(token, config).build()'s documented config has no
     container/selector option, so this assumes the widget self-manages its
     own overlay rather than mounting into a caller-owned DOM node. If the
     widget never visibly renders in a live sandbox, check whether it
     actually needs Belvo's conventional `#belvo` container id first. (Code
     review finding: an earlier version rendered a placeholder div with a
     mismatched id that no JS here ever read — removed as dead markup
     either way.) This modal just covers the brief gap between the click
     and the widget script/overlay actually appearing. -->
{#if belvoWidgetActive}
  <div class="modal modal-open" role="dialog" aria-modal="true">
    <div class="modal-box bg-base-200 max-w-md">
      <p class="text-sm opacity-70">{$_("linkedAccounts.connectingBelvo")}</p>
    </div>
  </div>
{/if}

<!-- Conversation deletion. Staged from the sidebar; nothing is removed until
     the user confirms here (custom modal, not the browser's confirm). -->
{#if pendingConversationDeletion}
  <div class="modal modal-open" role="alertdialog" aria-modal="true">
    <div class="modal-box bg-base-200 border border-error/40 max-w-md">
      <div class="flex items-center gap-2 text-error">
        <AlertTriangle class="w-5 h-5 shrink-0" />
        <h3 class="font-bold text-lg">{$_("confirmDelete.title")}</h3>
      </div>
      <p class="py-4 text-base-content/80">
        {$_("confirmDelete.conversationBody", {
          values: { name: pendingConversationDeletion.name },
        })}
      </p>
      <div class="modal-action">
        <button class="btn btn-ghost" onclick={cancelConversationDeletion}
          >{$_("common.cancel")}</button
        >
        <button class="btn btn-error" onclick={confirmConversationDeletion}
          >{$_("confirmDelete.confirm")}</button
        >
      </div>
    </div>
    <button
      class="modal-backdrop"
      aria-label={$_("common.cancel")}
      onclick={cancelConversationDeletion}
    ></button>
  </div>
{/if}


<div class="flex flex-col bg-base-100 h-dvh">
  <!-- Consolidated Top Bar -->
  <div
    class="bg-base-200 border-b border-base-300 px-4 h-[52px] flex items-center justify-between shrink-0"
  >
    <!-- LEFT: brand identity -->
    <div class="flex items-center gap-2.5">
      {#if activeScreen === "chat"}
        <!-- Hamburger: opens the main menu (#261) -->
        <button
          bind:this={hamburgerEl}
          type="button"
          class="p-1.5 -ml-1 rounded-lg text-base-content/80 hover:text-base-content hover:bg-base-100"
          onclick={toggleMenu}
          aria-label={menuOpen ? $_("menu.triggerAriaClose") : $_("menu.triggerAriaOpen")}
          aria-expanded={menuOpen}
          aria-controls="main-menu"
        >
          {#if menuOpen}
            <X class="w-5 h-5" />
          {:else}
            <Menu class="w-5 h-5" />
          {/if}
        </button>
      {/if}
      <!-- Light chip keeps the wizard's dark outline legible on the dark theme;
           reads as an app-icon badge on light. Fixed light bg (not base-100) so
           it works for system-dark too, where no data-theme attribute is set. -->
      <div
        class="w-8 h-8 rounded-xl bg-[#f7f5f0] ring-1 ring-black/5 shadow-sm flex items-center justify-center shrink-0"
      >
        <img
          src={nelsMark}
          alt="Nels"
          class="w-7 h-7"
          width="28"
          height="28"
          decoding="async"
        />
      </div>
      <!-- Mobile identity line -->
      <div class="flex items-baseline gap-1 md:hidden">
        <span class="text-sm font-bold text-base-content">Nels</span>
        {#if activeBudget}
          <span class="text-[11px] text-base-content/60">&middot;</span>
          <button
            type="button"
            class="text-sm font-semibold text-base-content hover:underline"
            aria-label={`${displayBudgetName(activeBudget.name)} — ${$_("header.budgetNameAria")}`}
            onclick={() => navigateToBudgetDetails(activeBudget.id)}
          >
            {displayBudgetName(activeBudget.name)}
          </button>
        {/if}
      </div>
      <!-- Desktop wordmark -->
      <div class="hidden md:flex md:flex-col">
        <span
          class="font-black text-xl tracking-wider bg-gradient-to-r from-primary to-secondary bg-clip-text text-transparent leading-none"
          >NELS</span
        >
        <span
          class="text-[9px] uppercase font-extrabold text-secondary tracking-widest mt-0.5"
          >{$_("header.tagline")}</span
        >
      </div>
      {#if activeBudget}
        <span class="hidden md:inline text-[11px] text-base-content/60">&middot;</span>
        <button
          type="button"
          class="hidden md:inline text-sm font-semibold text-base-content hover:underline"
          aria-label={`${displayBudgetName(activeBudget.name)} — ${$_("header.budgetNameAria")}`}
          onclick={() => navigateToBudgetDetails(activeBudget.id)}
        >
          {displayBudgetName(activeBudget.name)}
        </button>
      {/if}
      <BudgetStatusInfo budget={activeBudget} />
    </div>

    <!-- RIGHT: "+ New chat" + notification bell (chat screen only). Account
         actions/History/Categories/Budgets/Insights/Settings all live in the
         main menu (see MainMenu.svelte), reachable from the hamburger. -->
    <div class="flex items-center gap-1.5">
      {#if activeScreen === "chat"}
        <button
          type="button"
          class="p-1.5 rounded-lg text-base-content/80 hover:text-base-content hover:bg-base-100"
          onclick={handleNewConversation}
          aria-label={$_("sidebar.newConversation")}
        >
          <PenSquare class="w-5 h-5" />
        </button>
        <Notifications bind:this={notificationsRef} {fetchApi} {budgets} />
      {/if}
    </div>
  </div>

  <!-- Content row: sidebar + chat panel below the full-width top bar -->
  <div class="relative flex flex-row flex-grow overflow-hidden">
    {#if activeScreen === "chat"}
      <MainMenu
        open={menuOpen}
        {user}
        {token}
        {retirementPlannerEnabled}
        onClose={closeMenu}
        onNavigate={(r) => navigate(r)}
        onOpenBudgets={() => {
          navigateToBudgetsList();
          closeMenu();
        }}
        onExportData={downloadExport}
        onDeleteAccount={openDeleteAccount}
        onLogout={handleLogout}
      />
      <!-- Menu backdrop -->
      {#if menuOpen}
        <button
          type="button"
          aria-hidden="true"
          tabindex="-1"
          class="fixed inset-0 top-[52px] z-40 bg-black/60 transition-opacity motion-reduce:transition-none"
          onclick={closeMenu}
        ></button>
      {/if}
    {/if}

    <!-- Main Active Workspace Screen -->
    <main
      inert={menuOpen ? true : undefined}
      class="flex-grow min-w-0 relative flex flex-col overflow-hidden"
    >
    <!-- Global Alerts -->
    {#if errorAlert}
      <div
        class="alert alert-error shadow-lg mb-4 absolute top-4 left-4 right-4 z-50 max-w-3xl mx-auto flex items-center gap-2 animate-fade"
      >
        <AlertTriangle class="w-5 h-5" />
        <span>{errorAlert}</span>
      </div>
    {/if}

    {#if successAlert}
      <div
        class="alert alert-success shadow-lg mb-4 absolute top-4 left-4 right-4 z-50 max-w-3xl mx-auto flex items-center gap-2"
      >
        <CheckCircle2 class="w-5 h-5" />
        <span>{successAlert}</span>
      </div>
    {/if}

    <!-- --- SCREEN: AUTHENTICATION --- -->
    {#if activeScreen === "auth"}
      <div
        class="flex-grow overflow-y-auto flex flex-col items-center justify-center p-4 md:p-8 max-w-5xl w-full mx-auto"
      >
        {#if showRegSuccess}
          <div
            class="card w-full max-w-md bg-base-200 border border-base-300 shadow-2xl p-6 md:p-8 text-center animate-fade-in"
          >
            <div
              class="w-16 h-16 rounded-full bg-success/10 border border-success/30 flex items-center justify-center text-success mx-auto mb-4"
            >
              <CheckCircle2 class="w-8 h-8" />
            </div>
            <h2 class="text-2xl font-black text-base-content mb-2">
              {$_("auth.regSuccessTitle")}
            </h2>
            <p class="text-base-content/60 text-sm mb-6">
              {$_("auth.regSuccessBody")}
            </p>

            <div class="form-control mb-6">
              <div
                class="flex gap-2 items-center bg-base-100 border border-base-300 rounded-xl p-3 select-all"
              >
                <code
                  class="text-xs text-base-content/80 font-mono flex-grow break-all text-left"
                  >{tempRegToken}</code
                >
                <button
                  type="button"
                  class="btn btn-sm btn-ghost text-primary hover:text-primary/80 p-1 shrink-0"
                  onclick={() => copyToClipboard(tempRegToken)}
                  aria-label={$_("common.copyCode")}
                >
                  {#if copiedText}
                    <Check class="w-5 h-5 text-success" />
                  {:else}
                    <Copy class="w-5 h-5" />
                  {/if}
                </button>
              </div>
              {#if copiedText}
                <span
                  class="text-xs text-success font-semibold mt-1 self-start"
                  >{$_("common.copiedToClipboard")}</span
                >
              {/if}
            </div>

            {#if tempRecoveryCodes.length > 0}
              <div class="form-control mb-6 text-left">
                <p class="text-xs font-semibold text-warning mb-2">
                  {$_("auth.recoveryCodesTitle")}
                </p>
                <p class="text-xs text-base-content/60 mb-2">
                  {$_("auth.recoveryCodesBody")}
                </p>
                <div
                  class="flex gap-2 items-start bg-base-100 border border-base-300 rounded-xl p-3 select-all"
                >
                  <code
                    class="text-xs text-base-content/80 font-mono flex-grow whitespace-pre-wrap break-all text-left"
                    >{tempRecoveryCodes.join("\n")}</code
                  >
                  <button
                    type="button"
                    class="btn btn-sm btn-ghost text-primary hover:text-primary/80 p-1 shrink-0"
                    onclick={copyRecoveryCodes}
                    aria-label={$_("common.copyCode")}
                  >
                    {#if copiedRecoveryCodes}
                      <Check class="w-5 h-5 text-success" />
                    {:else}
                      <Copy class="w-5 h-5" />
                    {/if}
                  </button>
                </div>
                {#if copiedRecoveryCodes}
                  <span class="text-xs text-success font-semibold mt-1 self-start"
                    >{$_("common.copiedToClipboard")}</span
                  >
                {/if}
              </div>
            {/if}

            <button
              type="button"
              class="btn btn-primary w-full font-bold shadow-md"
              onclick={handleRegSuccessContinue}
            >
              {$_("auth.continueToDashboard")}
            </button>
          </div>
        {:else if showRecoveryForm}
          <div
            class="card w-full max-w-md bg-base-200 border border-base-300 shadow-2xl p-6 md:p-8"
          >
            <div class="text-center mb-6">
              <h1 class="text-2xl font-black tracking-tight text-base-content mb-1">
                {$_("auth.recoveryTitle")}
              </h1>
              <p class="text-base-content/60 text-sm">
                {$_("auth.recoveryDesc")}
              </p>
            </div>

            <form onsubmit={handleRecoveryStart} class="space-y-4">
              <div class="form-control">
                <label class="label" for="recovery-email">
                  <span class="label-text text-base-content/80 font-medium"
                    >{$_("auth.emailLabel")}</span
                  >
                </label>
                <input
                  id="recovery-email"
                  type="email"
                  placeholder="name@company.com"
                  class="input input-bordered bg-base-100 border-base-300 text-base-content focus:border-primary focus:outline-none w-full"
                  bind:value={recoveryEmailInput}
                  required
                />
              </div>
              <div class="form-control">
                <label class="label" for="recovery-code">
                  <span class="label-text text-base-content/80 font-medium"
                    >{$_("auth.recoveryCodeLabel")}</span
                  >
                </label>
                <input
                  id="recovery-code"
                  type="text"
                  autocomplete="off"
                  placeholder="ABCDE-FGHJK-MNPQR"
                  class="input input-bordered bg-base-100 border-base-300 text-base-content font-mono focus:border-primary focus:outline-none w-full"
                  bind:value={recoveryCodeInput}
                  required
                />
              </div>

              <button
                type="submit"
                class="btn btn-primary w-full font-bold shadow-md flex items-center justify-center gap-2"
                disabled={isLoading}
              >
                {#if isLoading}
                  <span class="loading loading-spinner"></span>
                {/if}
                {$_("auth.recoverySubmit")}
              </button>

              <button
                type="button"
                class="btn btn-ghost btn-sm w-full text-base-content/60 hover:text-base-content/80 font-semibold"
                onclick={() => {
                  showRecoveryForm = false;
                  errorAlert = "";
                  successAlert = "";
                }}
              >
                {$_("auth.backToSignIn")}
              </button>
            </form>
          </div>
        {:else}
          <div
            class="card w-full max-w-md bg-base-200 border border-base-300 shadow-2xl p-6 md:p-8"
          >
            <div class="text-center mb-6">
              <!-- Light chip badge: legible in both themes (see top-bar note). -->
              <div
                class="w-24 h-24 rounded-2xl bg-[#f7f5f0] ring-1 ring-black/5 shadow-lg flex items-center justify-center mx-auto mb-3"
              >
                <img
                  src={nelsMark}
                  alt="Nels"
                  class="w-20 h-20"
                  width="80"
                  height="80"
                  decoding="async"
                />
              </div>
              <h1 class="text-3xl font-black tracking-tight text-base-content mb-1">
                {$_("auth.heroTitle")}
              </h1>
              <p class="text-base-content/60 text-sm">
                {$_("auth.heroSubtitle")}
              </p>
            </div>

            <form onsubmit={handleAuthStart} class="space-y-4">
              <div class="form-control">
                <label class="label" for="email">
                  <span class="label-text text-base-content/80 font-medium"
                    >{$_("auth.emailLabel")}</span
                  >
                </label>
                <input
                  id="email"
                  type="email"
                  placeholder="name@company.com"
                  class="input input-bordered bg-base-100 border-base-300 text-base-content focus:border-primary focus:outline-none w-full"
                  bind:value={emailInput}
                  required
                />
              </div>

              <button
                type="submit"
                class="btn btn-primary w-full font-bold shadow-md flex items-center justify-center gap-2"
                disabled={isLoading}
              >
                {#if isLoading}
                  <span class="loading loading-spinner"></span>
                {:else}
                  <UserCheck class="w-5 h-5" />
                {/if}
                {isRegFlow ? $_("auth.registerSubmit") : $_("auth.signIn")}
              </button>
            </form>

            <div class="divider border-base-300 my-6">{$_("auth.dividerOr")}</div>

            <div class="text-center space-y-1">
              <button
                class="btn btn-ghost btn-sm text-primary hover:text-primary/80 font-semibold"
                onclick={() => {
                  isRegFlow = !isRegFlow;
                  errorAlert = "";
                  successAlert = "";
                }}
              >
                {isRegFlow ? $_("auth.toggleToSignIn") : $_("auth.toggleToRegister")}
              </button>
              {#if !isRegFlow}
                <div>
                  <button
                    class="btn btn-ghost btn-xs text-base-content/60 hover:text-base-content/80"
                    onclick={() => {
                      showRecoveryForm = true;
                      recoveryEmailInput = emailInput;
                      errorAlert = "";
                      successAlert = "";
                    }}
                  >
                    {$_("auth.useRecoveryCode")}
                  </button>
                </div>
              {/if}
            </div>

            <div
              class="mt-6 p-4 rounded-xl bg-base-100/60 border border-base-300 flex items-start gap-3"
            >
              <Info class="w-5 h-5 text-secondary shrink-0 mt-0.5" />
              <p class="text-sm text-base-content/70 leading-relaxed">
                {@html $_("auth.passkeyInfo")}
              </p>
            </div>

            <!-- Security reassurance (#382): a compact, honest trust moment that
                 directly answers "is this safe for my bank data?". Every claim
                 here mirrors the public marketing/privacy copy and the actual
                 architecture (WebAuthn/FIDO2 passkeys, nels#551,
                 encrypted-in-transit, read-only bank links via regulated
                 aggregators, credentials never seen). -->
            <div class="mt-4 pt-4 border-t border-base-300">
              <div class="flex items-center gap-2 mb-2">
                <ShieldCheck
                  class="w-4 h-4 text-success shrink-0"
                  aria-hidden="true"
                />
                <h2 class="text-sm font-semibold text-base-content">
                  {$_("auth.securityHeading")}
                </h2>
              </div>
              <ul class="space-y-1 text-xs text-base-content/70 leading-relaxed">
                <li>{$_("auth.securityPasswordless")}</li>
                <li>{$_("auth.securityData")}</li>
                <li>{$_("auth.securityBank")}</li>
              </ul>
            </div>
          </div>
        {/if}
      </div>

      <!-- --- SCREEN: AI COPILOT CHAT --- -->
    {:else if activeScreen === "chat"}
      <div class="flex flex-col flex-grow bg-base-100 overflow-hidden">
        <!-- Status micro-strip -->
        <div
          class="bg-base-100 border-b border-base-300 flex items-center justify-between px-4 min-h-6 py-0.5 text-[11px] shrink-0"
        >
          {#if showZb || showTrad}
            <div class="flex flex-col justify-center gap-0.5 py-0.5 w-full">
              {#if showZb}
                <div class="flex items-center gap-2 text-base-content/70">
                  <span class="font-medium text-base-content/50">{$_("chat.zbLabel")}</span>
                  <span>{$_("chat.zbAvailable")}: {fmtMoney(zbAvailable)}</span>
                  <span>{$_("chat.zbAllocated")}: {fmtMoney(zbAllocated)}</span>
                  <span
                    class={zbLeft === 0
                      ? "text-base-content/50"
                      : zbLeft > 0
                        ? "text-warning"
                        : "text-error"}
                    >{$_("chat.zbLeft")}: {fmtMoney(zbLeft)}</span
                  >
                </div>
              {/if}
              {#if showTrad}
                <div class="flex items-center gap-2 text-base-content/70">
                  <span class="font-medium text-base-content/50">{$_("chat.tradLabel")}</span>
                  <span>{$_("chat.tradLimit")}: {fmtMoney(activeInsight.budgeted)}</span>
                  <span>{$_("chat.tradSpent")}: {fmtMoney(activeInsight.spent)}</span>
                  <span class={activeInsight.remaining < 0 ? "text-error" : "text-success"}
                    >{$_("chat.tradRemaining")}: {fmtMoney(activeInsight.remaining)}</span
                  >
                </div>
              {/if}
            </div>
          {:else}
            <div class="flex items-center gap-1.5">
              <span class="relative flex w-2 h-2">
                <span
                  class="animate-ping absolute inline-flex h-full w-full rounded-full bg-success opacity-75"
                ></span>
                <span class="relative inline-flex rounded-full w-2 h-2 bg-success"
                ></span>
              </span>
              <span class="text-base-content/60">{$_("chat.statusReady")}</span>
            </div>
          {/if}
        </div>

        <!-- Conversation Message Window -->
        <div
          id="chat-scroll-area"
          class="flex-grow p-4 md:p-6 overflow-y-auto space-y-4 bg-base-100/40"
          onscroll={handleChatScroll}
        >
          {#if route === "categories"}
            <CategoriesView {fetchApi} {activeBudget} onBack={() => navigate("chat")} />
          {:else if route === "transactions"}
            <TransactionsView {fetchApi} {activeBudget} onBack={() => navigate("chat")} />
          {:else if route === "insights"}
            <Insights {fetchApi} onClose={() => navigate("chat")} />
          {:else if route === "budgetDetails"}
            <BudgetDetails
              {fetchApi}
              budgetId={budgetDetailsId}
              onBack={() => navigate("chat")}
              onBudgetUpdated={fetchBudgets}
            />
          {:else if route === "budgetsList"}
            <BudgetsView
              {fetchApi}
              {activeBudget}
              onSwitch={setActiveBudget}
              onBudgetUpdated={fetchBudgets}
              onBack={() => navigate("chat")}
              onOpenDetails={navigateToBudgetDetails}
            />
          {:else if route === "history"}
            <HistoryPage
              {conversations}
              {fetchApi}
              onBack={() => navigate("chat")}
              onPromptSelected={applyHistoryPrompt}
              hasDraft={chatInput.trim() !== ""}
              onRename={handleRenameConversation}
              onDelete={requestDeleteConversation}
            />
          {:else if route === "accounts"}
            <AccountsView
              {fetchApi}
              {activeBudget}
              {subscription}
              onUpgrade={startCheckout}
              onBack={() => navigate("chat")}
            />
          {:else if route === "retirement"}
            <RetirementView
              {fetchApi}
              onUpgrade={startCheckout}
              onBack={() => navigate("chat")}
            />
          {:else if route === "settings"}
            <Settings
              {fetchApi}
              {subscription}
              {billingFinalizing}
              onUpgrade={startCheckout}
              onManage={openPortal}
              onBack={() => navigate("chat")}
            />
          {:else if route === "deleteAccount"}
            <DeleteAccount
              userEmail={user?.email ?? ""}
              emailValue={deleteEmailInput}
              busy={deleteBusy}
              canConfirm={canDeleteAccount}
              onEmailInput={(v) => (deleteEmailInput = v)}
              onConfirm={confirmDeleteAccount}
              onBack={() => {
                deleteEmailInput = "";
                navigate("chat");
              }}
            />
          {:else if chatMessages.length === 0}
            <!-- Welcome message: the first thing the user sees in chat -->
            <div class="chat chat-start animate-fade">
              <div
                class="chat-bubble bg-base-200 border border-base-300 text-base-content text-sm leading-relaxed whitespace-pre-line max-w-xl shadow-md"
              >
                {#if user?.name}
                  {$_("chat.welcomeNamed", { values: { name: user.name } })}
                {:else}
                  {$_("chat.welcomeAnon")}
                {/if}
              </div>
            </div>
            <!-- Dynamic, history-aware suggested question. Suppressed during
                 name onboarding: when the user has no name yet, the welcome
                 bubble asks "what should I call you?" and we let them answer
                 conversationally instead of pushing an unrelated suggestion. -->
            {#if suggestedQuestion && user?.name}
              <div class="chat chat-start animate-fade">
                <button
                  onclick={() => handlePromptChip(suggestedQuestion)}
                  class="chat-bubble flex items-center gap-2 max-w-xl text-left text-sm
                         text-base-content/80 bg-base-200 border border-base-300
                         hover:border-primary/60 hover:bg-base-100 transition-colors"
                >
                  <Sparkles class="w-3.5 h-3.5 text-primary shrink-0" />
                  {suggestedQuestion}
                </button>
              </div>
            {/if}
          {:else}
            {#if chatWindow.hiddenCount > 0}
              <div class="flex justify-center">
                <button
                  type="button"
                  onclick={showEarlierMessages}
                  class="btn btn-ghost btn-xs gap-1 text-base-content/60"
                >
                  <History class="w-3.5 h-3.5" />
                  {$_("chat.showEarlier", { values: { count: chatWindow.hiddenCount } })}
                </button>
              </div>
            {/if}
            {#each chatWindow.visible as msg}
              <div class="chat {msg.sender === 'user' ? 'chat-end' : 'chat-start'} animate-fade">
                <div
                  class="chat-bubble {msg.sender === 'user'
                    ? 'bg-primary text-primary-content'
                    : 'bg-base-200 border border-base-300 text-base-content'} text-sm leading-relaxed whitespace-pre-line max-w-xl shadow-md"
                >
                  {#if msg.message_text}{msg.message_text}{/if}
                  {#if msg.categories_table_html}
                    <!-- `msg.categories_table_html` is ONLY ever backend-built table
                         markup (CATEGORY_BALANCE's single-category table, nels#301),
                         never model free-text — {@html} is safe here for the same
                         reason CategoriesView.svelte's identical comment gives. -->
                    <div class="mt-2 rounded-lg bg-base-100 border border-base-300 p-2">
                      {@html msg.categories_table_html}
                    </div>
                  {/if}
                  {#if msg.aiProviderError}
                    <!-- nels-oss#3: a failing BYO key is fixed in Settings. -->
                    <div class="mt-2 whitespace-normal">
                      <button
                        type="button"
                        class="btn btn-xs btn-outline"
                        title={$_(providerErrorKey(msg.aiProviderError))}
                        onclick={() => navigate("settings")}
                      >
                        {$_("aiProvider.openSettings")}
                      </button>
                    </div>
                  {/if}
                </div>
              </div>
            {/each}
          {/if}

          {#if isChatLoading}
            <div class="chat chat-start">
              <div
                class="chat-bubble bg-base-200 border border-base-300 flex items-center gap-1.5 p-3.5 shadow-md"
              >
                <span class="loading loading-dots loading-sm text-primary"
                ></span>
                <span class="text-xs text-base-content/60 font-medium"
                  >{$_("chat.analyzing")}</span
                >
              </div>
            </div>
          {/if}

          <!-- #376: inline category-choice chips. Shown when the assistant
               returned a pending_category_choice instead of auto-creating a
               brand-new category. Tapping resolves the transaction via finalize;
               "Something else…" clears the chips and focuses the input. -->
          {#if pendingCategoryChoice}
            <div class="chat chat-start animate-fade">
              <div class="flex flex-wrap gap-1.5 max-w-xl">
                {#each buildChoiceChips(pendingCategoryChoice, $_) as chip (chip.key)}
                  <button
                    type="button"
                    disabled={choiceSubmitting}
                    onclick={() => onCategoryChoiceChip(chip)}
                    class="chat-bubble text-sm text-base-content/80 bg-base-200
                           border border-base-300 hover:border-primary/60
                           hover:bg-base-100 transition-colors
                           disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {chip.label}
                  </button>
                {/each}
              </div>
            </div>
          {/if}
        </div>

        <!-- Bottom Chat Inputs -->
        <div class="p-3 bg-base-200 border-t border-base-300 shrink-0 relative">
          <!-- Slash-command palette (#56): surfaces available commands when
               the input starts with "/". -->
          {#if paletteOpen}
            <ul
              class="menu menu-sm absolute bottom-full left-3 right-3 mb-1 z-20 bg-base-100 border border-base-300 rounded-box shadow-lg p-1"
              role="listbox"
              aria-label={$_("commands.paletteAria")}
            >
              {#each paletteMatches as cmd, i (cmd.name)}
                <li>
                  <button
                    type="button"
                    role="option"
                    aria-selected={i === paletteIndex}
                    class="flex flex-col items-start gap-0 {i === paletteIndex
                      ? 'active'
                      : ''}"
                    onmousedown={(e) => {
                      e.preventDefault();
                      applyPaletteSelection(cmd.name);
                    }}
                  >
                    <span class="font-mono font-semibold text-primary"
                      >{cmd.name}</span
                    >
                    <span class="text-xs text-base-content/60"
                      >{$_(cmd.descKey)}</span
                    >
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
          <div class="flex justify-end mb-1">
            <QueuedMessages
              queue={pendingQueue}
              onRemove={removeQueuedMessage}
              onClearAll={clearQueuedMessages}
            />
          </div>
          <form onsubmit={sendChatMessage} class="flex gap-2 items-end">
            <textarea
              rows="1"
              placeholder={dictation.listening
                ? $_("chat.micListening")
                : $_("chat.inputPlaceholder")}
              class="textarea textarea-bordered bg-base-100 border-base-300 text-base-content flex-grow resize-none leading-relaxed focus:outline-none focus:border-primary"
              bind:value={chatInput}
              bind:this={chatInputEl}
              onkeydown={handleChatInputKeydown}
            ></textarea>
            <!--
              Compact control cluster (#143): the mic + recall icons stack
              vertically in a narrow column so they take minimal horizontal
              space, keeping the input wide. The tall send button sits beside
              the stack and spans its full height via items-stretch.
            -->
            <div class="flex gap-2 items-stretch shrink-0">
              <!--
                The mic + recall icon column is hidden on iOS/Android (#154):
                the native mobile keyboard already covers dictation and recent
                input, and the narrow screen is better spent on the input box.
              -->
              {#if !isMobile}
                <div class="flex flex-col gap-1">
                  {#if dictation.supported}
                    <button
                      type="button"
                      class="btn size-10 min-h-10 p-0 border-none {dictation.listening
                        ? 'btn-error text-error-content animate-pulse'
                        : 'btn-ghost text-base-content'}"
                      onclick={toggleDictation}
                      aria-pressed={dictation.listening}
                      aria-label={dictation.listening
                        ? $_("chat.micStop")
                        : $_("chat.micStart")}
                    >
                      <Mic class="w-4 h-4" />
                    </button>
                  {/if}
                  <button
                    type="button"
                    class="btn size-10 min-h-10 p-0 btn-ghost border-none text-base-content"
                    onclick={recallFromButton}
                    disabled={promptHistory.length === 0}
                    aria-label={$_("chat.recallAria")}
                  >
                    <History class="w-4 h-4" />
                  </button>
                </div>
              {/if}
              <!--
                p-0 + explicit dimensions make these icon-only buttons (rather
                than relying on btn-square's fixed sizing). h-auto + self-stretch
                let the send button span the icon column's height (items-stretch
                on the cluster row above); min-h-[5.25rem] is a floor (~ the two
                stacked size-10 buttons + gap) so the button stays a tall control
                even when dictation is unsupported and the column holds only the
                recall button — bump it if the icon button size or gap grows.
                On iOS/Android the column is hidden (#154), so there's no column
                to match; the floor is kept anyway to keep send a tall tap target.
              -->
              <button
                type="submit"
                class="btn h-auto min-h-[5.25rem] w-14 p-0 self-stretch btn-primary border-none text-primary-content"
                disabled={!chatInput.trim() || !canEnqueue(pendingQueue)}
                aria-label={$_("chat.sendAria")}
              >
                <Send class="w-5 h-5" />
              </button>
            </div>
          </form>
          {#if !canEnqueue(pendingQueue)}
            <p class="text-warning text-xs mt-2" role="status">
              {$_("chat.queueFull", { values: { max: QUEUE_CAP } })}
            </p>
          {/if}
          {#if dictation.error}
            <p class="text-error text-xs mt-2" role="alert">
              {dictation.error === "denied"
                ? $_("chat.micDenied")
                : $_("chat.micError")}
            </p>
          {/if}
        </div>
      </div>
    {/if}
    </main>
  </div>
</div>

<style>
  /* custom scrollbar overrides */
  .no-scrollbar::-webkit-scrollbar {
    display: none;
  }
  .no-scrollbar {
    -ms-overflow-style: none;
    scrollbar-width: none;
  }

  /* Categories table (#176, responsive #181). Rendered via {@html} from
     backend-built markup, so the styles must be :global() — scoped Svelte
     styles do not apply to {@html} content.

     #181: the categories outlet panel (CategoriesView.svelte, rendered inside
     #chat-scroll-area) is only ~width-of-viewport wide on a phone, and four
     columns (three of them currency-bearing) cannot comfortably fit ~360px
     without clipping the numbers or shrinking the font to an unreadable size
     (the three non-Category columns alone need substantial width even at a tiny
     font). So this uses two presentations off ONE backend markup: a normal
     table on wider screens, and at <=480px a stacked layout where each row
     becomes a labeled card.
     The <thead> is visually hidden on mobile and each cell shows its
     `data-label` (emitted by build_categories_table_html) instead. Either way
     the content fits the panel with only #chat-scroll-area's vertical
     scrollbar — no horizontal scrollbar. */

  /* ---- Desktop / wide: a normal, readable table (>=481px). ---- */
  :global(.cat-table) {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.75rem;
  }
  :global(.cat-table th),
  :global(.cat-table td) {
    padding: 0.25rem 0.5rem;
    text-align: left;
    border-bottom: 1px solid var(--color-base-300, oklch(0% 0 0 / 0.1));
    vertical-align: top;
  }
  /* Category column wraps (and breaks pathological unbroken long names) so it
     never pushes the numeric columns off-screen. */
  :global(.cat-table th:first-child),
  :global(.cat-table td:first-child) {
    white-space: normal;
    overflow-wrap: break-word;
    word-break: break-word;
  }
  /* Currency columns (Limit, Spent, Remaining) stay on one line, right-aligned. */
  :global(.cat-table th:nth-child(n + 2)),
  :global(.cat-table td:nth-child(n + 2)) {
    white-space: nowrap;
    text-align: right;
  }
  :global(.cat-table thead th) {
    font-weight: 600;
  }
  :global(.cat-table tfoot td) {
    font-weight: 600;
    border-top: 2px solid var(--color-base-300, oklch(0% 0 0 / 0.2));
    border-bottom: none;
  }

  /* Group header rows (#239): a full-width label row separating Income /
     Savings / Expense sections. Only rendered by the backend when a budget
     mixes more than one category type; a single-type budget has none of
     these rows, so this CSS is inert (and harmless) in that case. */
  :global(.cat-table tr.cat-group th) {
    font-weight: 700;
    background: var(--color-base-200, oklch(0% 0 0 / 0.04));
    border-bottom: 1px solid var(--color-base-300, oklch(0% 0 0 / 0.15));
  }
  :global(.cat-table tbody tr.cat-group:first-child th) {
    border-top: none;
  }

  /* ---- Mobile (<=480px): stacked, label-per-cell layout. ---- */
  @media (max-width: 480px) {
    :global(.cat-table) {
      display: block;
      width: 100%;
    }
    /* Hide the header row visually (still present for semantics); the per-cell
       data-label replaces it. */
    :global(.cat-table thead) {
      position: absolute;
      width: 1px;
      height: 1px;
      padding: 0;
      margin: -1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
      border: 0;
    }
    :global(.cat-table tbody),
    :global(.cat-table tfoot),
    :global(.cat-table tr) {
      display: block;
      width: 100%;
    }
    :global(.cat-table tbody tr) {
      padding: 0.375rem 0;
    }
    /* Each cell becomes a "Label  value" row. The numeric value stays on one
       line (right-aligned); the label sits left. overflow is reset so the
       desktop nowrap/border rules cannot clip the value. */
    :global(.cat-table td) {
      display: flex;
      align-items: baseline;
      justify-content: space-between;
      gap: 0.75rem;
      padding: 0.125rem 0.25rem;
      border: none;
      overflow: visible;
      white-space: normal;
      text-align: right;
    }
    :global(.cat-table td::before) {
      content: attr(data-label);
      flex: 0 0 auto;
      font-weight: 600;
      text-align: left;
      white-space: nowrap;
      opacity: 0.65;
    }
    /* Defensive: hide any value-less cell so the stacked card never shows a
       stray label with no value. A cell with any value text is not :empty, so
       data cells are unaffected. */
    :global(.cat-table td:empty) {
      display: none;
    }
    /* The first cell is the category name: a full-width, left-aligned heading
       for the card; its label is suppressed. */
    :global(.cat-table tbody td:first-child),
    :global(.cat-table tfoot td:first-child) {
      font-weight: 600;
      justify-content: flex-start;
      text-align: left;
      overflow-wrap: break-word;
      word-break: break-word;
    }
    :global(.cat-table tbody td:first-child::before),
    :global(.cat-table tfoot td:first-child::before) {
      content: none;
    }
    /* Totals card: a clear divider above it. */
    :global(.cat-table tfoot tr) {
      display: block;
      margin-top: 0.25rem;
      padding-top: 0.375rem;
      border-top: 2px solid var(--color-base-300, oklch(0% 0 0 / 0.2));
    }

    /* Group header rows (#239) on mobile: the group header row is 4 real
       <th> cells (#299 — Group / amount column / activity column /
       Remaining), not the old single colspan label — but the mobile
       stacked card should still show just the bare group name, matching
       pre-#299 appearance (every data row already carries its own
       data-label prefix, so repeating the column names here would be
       redundant). Only the first <th> renders (overriding the browser's
       default table-cell display with a plain left-aligned block, and
       adding spacing above each new group so sections stay visually
       separated in the stacked card layout); the other 3 are hidden. */
    :global(.cat-table tr.cat-group th:first-child) {
      display: block;
      padding: 0.375rem 0.25rem;
      text-align: left;
      /* Redundant with the un-gated `.cat-table tr.cat-group th` rule above
         (also 700) — restated explicitly rather than relied on implicitly,
         since this selector's cells are real <th>s: no `td`-typed rule
         (e.g. the mobile `tbody/tfoot td:first-child` rule at 600) can ever
         match a <th>, so there is no specificity race to defend against
         here, unlike when this row's cells were <td>s before #299. */
      font-weight: 700;
    }
    :global(.cat-table tr.cat-group th:nth-child(n + 2)) {
      display: none;
    }
    :global(.cat-table tbody tr.cat-group:not(:first-child)) {
      margin-top: 0.5rem;
    }
  }
</style>
