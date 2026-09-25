<script>
  import { loadStripe } from "@stripe/stripe-js";
  import { _ } from "svelte-i18n";
  import {
    CheckCircle, AlertTriangle, XCircle, Landmark,
    Plus, Search, ChevronLeft, Check, RefreshCw, Unlink, Link2,
  } from "lucide-svelte";
  import {
    fetchLinkedAccounts, startLinkFlow, refreshLinkedAccount, disconnectLinkedAccount, isProGateError,
    fetchGcInstitutions, startGcLinkFlow, isConsentExpired,
    startBelvoLinkFlow, completeBelvoLinkFlow, loadBelvoWidget,
    startBasiqLinkFlow, startAkahuLinkFlow,
    startPlaidLinkFlow, loadPlaidLink, PLAID_LINK_RESUME_KEY,
    syncStatusFor, statusBadgeFor, initialsFor, maskedLast4, groupAccountsByInstitution,
    COUNTRY_OPTIONS, filterCountries, filterInstitutions, groupCountriesByRegion, formatRelativeTime, linkCtaMeta,
  } from "./linkedAccounts.js";

  let { budgetId, isPro, onUpgrade, fetchApi } = $props();

  let accounts = $state([]);
  let loading = $state(false);
  let error = $state("");

  // GoCardless country/institution picker (#320). Stripe Financial
  // Connections stays US-only, so "US" is the one non-GoCardless option —
  // everything else routes through the redirect-based GoCardless flow below.
  const GC_COUNTRIES = ["GB", "FR", "DE", "IT", "ES", "DK", "FI", "NO"];
  // Belvo (#322): Mexico and Brazil, via Belvo's own embeddable widget
  // (client-side, no redirect and no institution-picker REST call — the
  // widget renders its own institution picker).
  const BELVO_COUNTRIES = ["MX", "BR"];
  let belvoWidgetActive = $state(false);
  let plaidLinkActive = $state(false);
  let selectedCountry = $state("US");
  let institutions = $state([]);
  let selectedInstitutionId = $state("");
  let loadingInstitutions = $state(false);
  // Guards onCountryChange against out-of-order responses: if the user
  // switches country again before an in-flight institutions fetch resolves,
  // the stale response must not clobber the (now-different) selection —
  // only the LATEST request's result is ever applied.
  let institutionsRequestSeq = 0;

  // --- #365 redesign state ---
  // Per-row refresh feedback: the set of account ids with an in-flight
  // refresh, so only that row shows a spinner (not a global flag).
  let refreshingIds = $state(new Set());
  // The account awaiting disconnect confirmation (null when the confirm
  // dialog is closed). Disconnect only fires from the dialog's confirm action.
  let pendingDisconnect = $state(null);
  let confirmDialogEl = $state(null);
  // Add-account modal (#365): a native <dialog> replacing the inline selects.
  let addDialogEl = $state(null);
  let modalStep = $state("country"); // "country" | "action"
  let countryQuery = $state("");
  let bankQuery = $state("");

  const groups = $derived(groupAccountsByInstitution(accounts));
  const filteredRegions = $derived(groupCountriesByRegion(filterCountries(COUNTRY_OPTIONS, countryQuery)));
  const selectedInstitution = $derived(institutions.find((i) => i.id === selectedInstitutionId) || null);
  const filteredInstitutions = $derived(filterInstitutions(institutions, bankQuery));
  const ctaMeta = $derived(linkCtaMeta(selectedCountry, { bankName: selectedInstitution?.name }));
  // GoCardless is the one provider with an in-app bank picker, so its connect
  // action stays disabled until a bank is chosen. Derived once so the guard
  // that gates connectFromModal() and the CTA's `disabled` can't drift apart.
  const connectDisabled = $derived(GC_COUNTRIES.includes(selectedCountry) && !selectedInstitutionId);

  // Region header i18n key, e.g. "northAmerica" -> "linkedAccounts.regionNorthAmerica".
  function regionLabelKey(region) {
    return `linkedAccounts.region${region.charAt(0).toUpperCase() + region.slice(1)}`;
  }

  function syncLabel(acct) {
    // syncStatusFor only returns "synced" or "never" today — the backend
    // exposes no per-account sync-error signal, so the syncFailed key is
    // intentionally unused for now (#365 spec §6).
    const s = syncStatusFor(acct, Date.now());
    if (s.kind === "synced") return $_("linkedAccounts.lastSynced", { values: { time: s.relative } });
    return $_("linkedAccounts.neverSynced");
  }

  async function load() {
    loading = true;
    try {
      accounts = await fetchLinkedAccounts({ budgetId, fetchApi });
      error = "";
    } catch (e) {
      error = e.message || $_("linkedAccounts.loadError");
    } finally {
      loading = false;
    }
  }

  async function onCountryChange() {
    institutions = [];
    selectedInstitutionId = "";
    if (!GC_COUNTRIES.includes(selectedCountry)) return;
    const seq = ++institutionsRequestSeq;
    loadingInstitutions = true;
    try {
      const result = await fetchGcInstitutions({ country: selectedCountry, fetchApi });
      if (seq !== institutionsRequestSeq) return; // a newer country selection has already superseded this request
      institutions = result;
    } catch (e) {
      if (seq !== institutionsRequestSeq) return;
      error = e.message || $_("linkedAccounts.linkError");
    } finally {
      if (seq === institutionsRequestSeq) loadingInstitutions = false;
    }
  }

  async function link() {
    error = "";
    if (BELVO_COUNTRIES.includes(selectedCountry)) {
      try {
        const session = await startBelvoLinkFlow({ budgetId, country: selectedCountry, fetchApi });
        belvoWidgetActive = true;
        // `session.session_id` is read from this closure's own `session`
        // const, NOT from component state — if the user re-clicks "connect"
        // before this widget's callback fires (starting a SECOND session),
        // a shared `belvoSessionId` state var would get overwritten and this
        // callback would complete the WRONG session, causing 403/409 errors
        // (Copilot review finding). Capturing it per-invocation makes each
        // concurrent link() call self-contained instead.
        await loadBelvoWidget(session.access_token, {
          onSuccess: async (belvoLinkId) => {
            belvoWidgetActive = false;
            try {
              const { linked } = await completeBelvoLinkFlow({ budgetId, sessionId: session.session_id, belvoLinkId, fetchApi });
              if (linked?.length) await load();
            } catch (e) {
              error = e.message || $_("linkedAccounts.linkError");
            }
          },
          onExit: () => { belvoWidgetActive = false; },
        });
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
        belvoWidgetActive = false;
      }
      return;
    }
    if (GC_COUNTRIES.includes(selectedCountry)) {
      if (!selectedInstitutionId) return;
      try {
        const session = await startGcLinkFlow({
          budgetId, country: selectedCountry, institutionId: selectedInstitutionId, fetchApi,
        });
        // GoCardless's consent flow is redirect-based (no client-side SDK
        // modal like Stripe's) — leaving the page is expected; App.svelte
        // picks the flow back up via the `gc_ref` query param GoCardless
        // redirects back with (see start_link_session's redirect URL).
        window.location.href = session.redirect_url;
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    if (selectedCountry === "AU") {
      try {
        const session = await startBasiqLinkFlow({ budgetId, fetchApi });
        window.location.href = session.redirect_url;
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    if (selectedCountry === "NZ") {
      try {
        const session = await startAkahuLinkFlow({ budgetId, fetchApi });
        window.location.href = session.redirect_url;
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    if (selectedCountry === "CA") {
      try {
        // Unlike the redirect-based GC/Basiq/Akahu branches above, Plaid
        // Link is a client-side modal — but Big-5 Canadian banks still
        // navigate away for their own OAuth/Interac login and back, so the
        // link_token/session_id must be persisted to localStorage BEFORE
        // opening the modal (so App.svelte's handlePlaidOAuthReturn can
        // resume on return). Calling fetchApi directly here (rather than
        // letting startPlaidLinkFlow create the token internally) makes the
        // token/session_id available to persist first.
        const created = await fetchApi(`/budgets/${budgetId}/plaid/link-token`, { method: "POST" });
        // Copilot review finding (#321): a malformed /plaid/link-token
        // response (missing link_token/session_id) must not be persisted to
        // localStorage — a resume payload built from it would be unusable if
        // the browser ever navigated away and back for a Big-5 OAuth flow,
        // failing far from this call site in a confusing way. Fail fast here
        // instead; startPlaidLinkFlow below has its own equivalent guards as
        // defense in depth, but catching it before ever touching
        // localStorage is strictly better.
        if (!created?.link_token || !created?.session_id) {
          throw new Error("Failed to create Plaid link token");
        }
        window.localStorage.setItem(PLAID_LINK_RESUME_KEY, JSON.stringify({
          linkToken: created.link_token, sessionId: created.session_id, budgetId,
        }));
        // Gates a "Connecting…" message covering the brief gap between the
        // click and Plaid Link's own modal actually appearing (loadPlaidLink
        // injecting/loading its CDN script, then Plaid.create().open())
        // (silent-failure-hunter finding, mirrors belvoWidgetActive above).
        plaidLinkActive = true;
        let linked;
        try {
          ({ linked } = await startPlaidLinkFlow({
            budgetId, fetchApi, loadPlaidLink, linkToken: created.link_token, sessionId: created.session_id,
          }));
        } finally {
          // Clears the resume payload once Plaid Link settles IN-MODAL
          // (success/cancel/error, no OAuth redirect). If Link instead
          // navigated the browser away for a Big-5 OAuth flow, this page's
          // JS context is destroyed before this promise ever settles, so
          // this never runs — which is exactly what's needed, since the
          // key must survive the redirect for handlePlaidOAuthReturn to
          // pick it up (code review: prevents a later, unrelated flow from
          // clobbering an in-flight OAuth-redirect flow's resume data).
          // plaidLinkActive is cleared here for the same reason: an actual
          // OAuth redirect destroys this page before the finally block runs,
          // which is fine since the "Connecting…" indicator is moot once the
          // browser has navigated away.
          window.localStorage.removeItem(PLAID_LINK_RESUME_KEY);
          plaidLinkActive = false;
        }
        if (linked?.length) await load();
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
        plaidLinkActive = false;
      }
      return;
    }
    try {
      const { linked } = await startLinkFlow({
        budgetId,
        fetchApi,
        loadStripe,
        publishableKey: import.meta.env.VITE_STRIPE_PUBLISHABLE_KEY,
      });
      if (linked?.length) await load();
    } catch (e) {
      if (isProGateError(e)) {
        error = $_("linkedAccounts.linkProGate");
      } else {
        error = e.message || $_("linkedAccounts.linkError");
      }
    }
  }

  async function refresh(accountId) {
    // Idempotent per id: a double-tap must not start a second concurrent
    // refresh, or the first's `finally` would clear the spinner while the
    // second is still in flight (Copilot review finding).
    if (refreshingIds.has(accountId)) return;
    // Per-row feedback (#365): mark this id in-flight so only its row spins.
    // Reassign the Set (not mutate) so Svelte's reactivity fires.
    refreshingIds = new Set(refreshingIds).add(accountId);
    try {
      await refreshLinkedAccount({ budgetId, accountId, fetchApi });
      error = "";
    } catch (e) {
      error = isProGateError(e) ? $_("linkedAccounts.refreshProGate") : (e.message || $_("linkedAccounts.refreshError"));
    } finally {
      const next = new Set(refreshingIds);
      next.delete(accountId);
      refreshingIds = next;
    }
  }

  async function disconnect(accountId) {
    try {
      await disconnectLinkedAccount({ budgetId, accountId, fetchApi });
      await load();
      error = "";
    } catch (e) {
      error = e.message || $_("linkedAccounts.disconnectError");
    }
  }

  // Re-consent reuses the general link flow rather than a dedicated
  // "reconnect" endpoint (spec Assumption 11): the user re-picks the same
  // country/institution in the add-account modal and the backend reconciles
  // the new account onto this row by IBAN match. The linking UI is only
  // rendered for Pro users (see the `{#if isPro}` add-account CTA below) — so
  // for a non-Pro user, "Reconnect" on a consent_expired account must route
  // to the upgrade prompt instead of opening a modal whose CTA is gated
  // (Copilot review finding, preserved through the #365 redesign).
  function reconnect() {
    if (isPro) {
      openAddDialog();
    } else {
      onUpgrade?.("monthly");
    }
  }

  // --- #365 dialog controls ---
  function openAddDialog() {
    modalStep = "country";
    countryQuery = "";
    bankQuery = "";
    error = ""; // don't carry a prior failure into a fresh add session
    addDialogEl?.showModal();
  }
  function closeAddDialog() {
    addDialogEl?.close();
  }
  async function chooseCountry(id) {
    selectedCountry = id;
    bankQuery = "";
    error = ""; // clear any prior country's fetch error before re-fetching
    // Advance to the action step FIRST so its `{#if loadingInstitutions}`
    // skeleton is mounted while onCountryChange fetches the GoCardless bank
    // list — otherwise the fetch resolves (clearing loadingInstitutions in its
    // own finally) before the step ever renders, so the tap reads as dead on
    // slower networks and the skeleton is unreachable (code review finding).
    modalStep = "action";
    await onCountryChange();
  }
  function connectFromModal() {
    // GoCardless needs a bank chosen first; its CTA stays disabled until then.
    if (connectDisabled) return;
    closeAddDialog();
    link();
  }
  function requestDisconnect(acct) {
    pendingDisconnect = acct;
    confirmDialogEl?.showModal();
  }
  function cancelDisconnect() {
    confirmDialogEl?.close();
    pendingDisconnect = null;
  }
  async function confirmDisconnect() {
    const acct = pendingDisconnect;
    confirmDialogEl?.close();
    pendingDisconnect = null;
    if (acct) await disconnect(acct.id);
  }

  // Re-fetch whenever budgetId changes (not just once at mount). Currently
  // unreachable in practice (the Settings route has no budget switcher
  // mounted alongside it, so budgetId can't actually change while this panel
  // stays mounted) but cheap to make correct so it isn't a landmine if that
  // ever changes.
  $effect(() => {
    void budgetId;
    load();
  });
</script>

{#snippet statusBadge(acct)}
  {@const badge = statusBadgeFor(acct)}
  {@const label = $_(badge.labelKey)}
  <span class="badge {badge.variant} badge-sm" role="img" aria-label={label} title={label}>
    {#if badge.icon === "check"}<CheckCircle class="w-3.5 h-3.5" aria-hidden="true" />
    {:else if badge.icon === "alert"}<AlertTriangle class="w-3.5 h-3.5" aria-hidden="true" />
    {:else}<XCircle class="w-3.5 h-3.5" aria-hidden="true" />{/if}
  </span>
{/snippet}

{#snippet accountCard(acct, inGroup = false)}
  <div class="card card-compact bg-base-100 border border-base-300 rounded-xl">
    <div class="card-body flex-row items-center gap-3">
      <div class="avatar avatar-placeholder shrink-0">
        <div class="bg-primary/10 text-primary w-10 h-10 rounded-full">
          <span class="text-sm font-medium">{initialsFor(acct)}</span>
        </div>
      </div>
      <div class="min-w-0 flex-1">
        <p class="font-medium truncate">{acct.display_name ?? $_("linkedAccounts.unknownAccount")}</p>
        <!-- Inside a group, the institution name is already shown in the
             collapse header (#372), so the card drops it and shows only the
             masked account number. A standalone (single-account) card has NO
             header above it, so it keeps institution · last4 — removing it
             there would erase the institution entirely. -->
        {#if inGroup}
          {#if maskedLast4(acct)}
            <p class="text-sm opacity-70 truncate">{maskedLast4(acct)}</p>
          {/if}
        {:else}
          <p class="text-sm opacity-70 truncate">
            {acct.institution_name ?? $_("linkedAccounts.unknownInstitution")}{#if maskedLast4(acct)} · {maskedLast4(acct)}{/if}
          </p>
        {/if}
        <p class="text-xs opacity-50 truncate">{syncLabel(acct)}</p>
      </div>
      <div class="flex items-center gap-1 shrink-0">
        {@render statusBadge(acct)}
        {#if acct.status === "active" || isConsentExpired(acct)}
          {#if acct.status === "active"}
            {#if refreshingIds.has(acct.id)}
              <span class="min-h-11 min-w-11 inline-flex items-center justify-center" role="status" aria-label={$_("linkedAccounts.refreshing")}>
                <span class="loading loading-spinner loading-sm" aria-hidden="true"></span>
              </span>
            {:else}
              <button type="button" class="btn btn-ghost btn-sm btn-square min-h-11 min-w-11" aria-label={$_("linkedAccounts.refresh")} title={$_("linkedAccounts.refresh")} onclick={() => refresh(acct.id)}>
                <RefreshCw class="w-5 h-5" aria-hidden="true" />
              </button>
            {/if}
          {/if}
          {#if isConsentExpired(acct)}
            <button type="button" class="btn btn-ghost btn-sm btn-square min-h-11 min-w-11" aria-label={$_("linkedAccounts.reconnect")} title={$_("linkedAccounts.reconnect")} onclick={reconnect}>
              <Link2 class="w-5 h-5" aria-hidden="true" />
            </button>
          {/if}
          <button type="button" class="btn btn-ghost btn-sm btn-square min-h-11 min-w-11 text-error" aria-label={$_("linkedAccounts.disconnect")} title={$_("linkedAccounts.disconnect")} onclick={() => requestDisconnect(acct)}>
            <Unlink class="w-5 h-5" aria-hidden="true" />
          </button>
        {/if}
      </div>
    </div>
  </div>
{/snippet}

<div class="linked-accounts">
  <h3 class="font-semibold text-lg mb-2">{$_("linkedAccounts.title")}</h3>

  {#if error}
    <div role="alert" class="alert alert-error alert-sm mb-3">
      <span>{error}</span>
    </div>
  {/if}

  {#if loading}
    <div class="space-y-2" aria-busy="true">
      {#each [0, 1, 2] as i (i)}
        <div class="skeleton h-16 w-full rounded-xl"></div>
      {/each}
    </div>
  {:else if accounts.length === 0}
    {#if isPro}
      <div class="bg-base-200 rounded-xl p-8 text-center flex flex-col items-center gap-3">
        <div class="bg-primary/10 rounded-full p-4">
          <Landmark class="w-8 h-8 text-primary" aria-hidden="true" />
        </div>
        <h4 class="font-semibold text-base">{$_("linkedAccounts.emptyHeadline")}</h4>
        <p class="text-sm opacity-70 max-w-xs">{$_("linkedAccounts.emptySubtext")}</p>
        <button type="button" class="btn btn-primary gap-1.5 min-h-11" onclick={openAddDialog}>
          <Plus class="w-4 h-4" aria-hidden="true" /> {$_("linkedAccounts.link")}
        </button>
      </div>
    {:else}
      <div class="card bg-base-200 border border-primary/20 p-4">
        <div class="flex items-center gap-2 mb-3">
          <span class="badge badge-primary badge-outline">{$_("linkedAccounts.proBadge")}</span>
          <span class="text-sm opacity-70">{$_("linkedAccounts.proPrice")}</span>
        </div>
        <h4 class="font-semibold mb-2">{$_("linkedAccounts.emptyHeadline")}</h4>
        <ul class="space-y-1.5 mb-4">
          {#each ["proBenefit1", "proBenefit2", "proBenefit3"] as b (b)}
            <li class="flex items-start gap-2 text-sm">
              <Check class="w-4 h-4 text-primary shrink-0 mt-0.5" aria-hidden="true" />
              <span>{$_(`linkedAccounts.${b}`)}</span>
            </li>
          {/each}
        </ul>
        <button type="button" class="btn btn-primary min-h-11" onclick={() => onUpgrade?.("monthly")}>
          {$_("linkedAccounts.upgradeToLink")}
        </button>
      </div>
    {/if}
  {:else}
    <div class="space-y-2">
      {#each groups as group (group.key)}
        {#if group.count > 1}
          <div class="collapse collapse-arrow bg-base-200 rounded-xl">
            <input type="checkbox" checked aria-label={group.institutionName ?? $_("linkedAccounts.unknownInstitution")} />
            <div class="collapse-title flex items-center gap-3">
              <div class="avatar avatar-placeholder shrink-0">
                <div class="bg-primary/10 text-primary w-9 h-9 rounded-full">
                  <span class="text-xs font-medium">{initialsFor(group.accounts[0])}</span>
                </div>
              </div>
              <div class="min-w-0">
                <p class="font-medium truncate">{group.institutionName ?? $_("linkedAccounts.unknownInstitution")}</p>
                <p class="text-xs opacity-60">
                  {$_("linkedAccounts.groupCount", { values: { count: group.count } })}{#if group.oldestSyncedAtMs != null} · {$_("linkedAccounts.lastSynced", { values: { time: formatRelativeTime(group.oldestSyncedAtMs, Date.now()) } })}{/if}
                </p>
              </div>
            </div>
            <div class="collapse-content space-y-2">
              {#each group.accounts as acct (acct.id)}
                {@render accountCard(acct, true)}
              {/each}
            </div>
          </div>
        {:else}
          {@render accountCard(group.accounts[0])}
        {/if}
      {/each}
    </div>

    {#if isPro}
      <button type="button" class="btn btn-primary btn-block gap-1.5 mt-3 min-h-11" onclick={openAddDialog}>
        <Plus class="w-4 h-4" aria-hidden="true" /> {$_("linkedAccounts.addAccount")}
      </button>
    {:else}
      <button type="button" class="btn btn-outline btn-block mt-3 min-h-11" onclick={() => onUpgrade?.("monthly")}>
        {$_("linkedAccounts.upgradeToLink")}
      </button>
    {/if}
  {/if}

  {#if belvoWidgetActive}
    <!-- Covers the brief gap between the click and the Belvo widget overlay
         actually appearing (script/overlay load). Retained from the pre-#365
         UI, restyled as an alert with a spinner. -->
    <div role="status" class="alert alert-info alert-sm my-3">
      <span class="loading loading-spinner loading-sm"></span>
      <span>{$_("linkedAccounts.connectingBelvo")}</span>
    </div>
  {/if}

  {#if plaidLinkActive}
    <!-- Covers the brief gap between the click and Plaid Link's modal actually
         rendering (script load + Plaid.create().open()), same rationale as the
         Belvo indicator above (silent-failure-hunter finding, #321). -->
    <div role="status" class="alert alert-info alert-sm my-3">
      <span class="loading loading-spinner loading-sm"></span>
      <span>{$_("linkedAccounts.connectingPlaid")}</span>
    </div>
  {/if}
</div>

<!-- Add-account modal (#365): native <dialog> traps focus and restores it to
     the trigger on close. -->
<dialog bind:this={addDialogEl} class="modal modal-bottom sm:modal-middle">
  <div class="modal-box">
    {#if modalStep === "country"}
      <h3 class="font-semibold text-lg mb-3">{$_("linkedAccounts.addAccount")}</h3>
      <label class="input input-bordered flex items-center gap-2 mb-3">
        <Search class="w-4 h-4 opacity-50" aria-hidden="true" />
        <input type="text" class="grow" placeholder={$_("linkedAccounts.searchCountry")} aria-label={$_("linkedAccounts.searchCountry")} bind:value={countryQuery} />
      </label>
      <div class="max-h-80 overflow-y-auto space-y-3">
        {#each filteredRegions as region (region.region)}
          <div>
            <p class="text-xs font-semibold opacity-50 uppercase px-1 mb-1">{$_(regionLabelKey(region.region))}</p>
            <ul class="menu p-0 gap-1">
              {#each region.countries as c (c.id)}
                <li>
                  <button type="button" class="flex items-center gap-3 min-h-11" onclick={() => chooseCountry(c.id)}>
                    <span class="text-xl" aria-hidden="true">{c.flag}</span>
                    <span>{c.name}</span>
                  </button>
                </li>
              {/each}
            </ul>
          </div>
        {/each}
        {#if filteredRegions.length === 0}
          <p class="text-sm opacity-60 text-center py-4">{$_("linkedAccounts.noCountriesFound")}</p>
        {/if}
      </div>
      <div class="modal-action">
        <button type="button" class="btn btn-ghost min-h-11" onclick={closeAddDialog}>{$_("linkedAccounts.cancel")}</button>
      </div>
    {:else}
      <div class="flex items-center gap-2 mb-3">
        <button type="button" class="btn btn-ghost btn-sm btn-square min-h-11 min-w-11" aria-label={$_("accounts.back")} onclick={() => { modalStep = "country"; }}>
          <ChevronLeft class="w-5 h-5" aria-hidden="true" />
        </button>
        <h3 class="font-semibold text-lg">{$_("linkedAccounts.addAccount")}</h3>
      </div>

      <!-- Surface load/link failures INSIDE the open dialog. onCountryChange's
           catch sets `error`, but the main-view error alert paints behind this
           modal's backdrop where it's invisible — without this a failed
           GoCardless bank-list fetch would masquerade as an empty "no banks
           found" list (silent-failure review finding). -->
      {#if error}
        <div role="alert" class="alert alert-error alert-sm mb-3">
          <span>{error}</span>
        </div>
      {/if}

      {#if GC_COUNTRIES.includes(selectedCountry)}
        {#if loadingInstitutions}
          <div class="space-y-2">
            {#each [0, 1, 2] as i (i)}<div class="skeleton h-11 w-full rounded-lg"></div>{/each}
          </div>
        {:else}
          <label class="input input-bordered flex items-center gap-2 mb-3">
            <Search class="w-4 h-4 opacity-50" aria-hidden="true" />
            <input type="text" class="grow" placeholder={$_("linkedAccounts.searchBank")} aria-label={$_("linkedAccounts.searchBank")} bind:value={bankQuery} />
          </label>
          <ul class="menu p-0 gap-1 max-h-72 overflow-y-auto mb-3">
            {#each filteredInstitutions as inst (inst.id)}
              <li>
                <button type="button" class="min-h-11 {selectedInstitutionId === inst.id ? 'active' : ''}" onclick={() => { selectedInstitutionId = inst.id; }}>
                  {inst.name}
                </button>
              </li>
            {/each}
            {#if filteredInstitutions.length === 0 && !error}
              <li class="pointer-events-none"><span class="text-sm opacity-60">{$_("linkedAccounts.noBanksFound")}</span></li>
            {/if}
          </ul>
        {/if}
      {/if}

      {#if selectedCountry === "CA"}
        <div role="note" class="alert alert-info alert-sm mb-3">
          <span>{$_("linkedAccounts.disconnectPlaidSiblingsNote")}</span>
        </div>
      {/if}

      <p class="text-sm opacity-70 mb-3">{$_(ctaMeta.helperKey)}</p>

      <div class="modal-action">
        <button type="button" class="btn btn-ghost min-h-11" onclick={closeAddDialog}>{$_("linkedAccounts.cancel")}</button>
        <button
          type="button"
          class="btn btn-primary min-h-11"
          disabled={connectDisabled}
          onclick={connectFromModal}
        >
          {$_(ctaMeta.ctaKey, { values: ctaMeta.ctaValues })}
        </button>
      </div>
    {/if}
  </div>
  <form method="dialog" class="modal-backdrop">
    <button aria-label={$_("linkedAccounts.cancel")}>close</button>
  </form>
</dialog>

<!-- Disconnect confirmation (#365): destructive action requires an explicit
     confirm before disconnect() fires. -->
<dialog bind:this={confirmDialogEl} class="modal modal-bottom sm:modal-middle">
  <div class="modal-box">
    <h3 class="font-semibold text-lg mb-2">
      {$_("linkedAccounts.confirmDisconnectTitle", { values: { name: pendingDisconnect?.display_name ?? $_("linkedAccounts.unknownAccount") } })}
    </h3>
    <p class="text-sm opacity-70">{$_("linkedAccounts.confirmDisconnectBody")}</p>
    <div class="modal-action">
      <button type="button" class="btn btn-ghost min-h-11" onclick={cancelDisconnect}>{$_("linkedAccounts.cancel")}</button>
      <button type="button" class="btn btn-error min-h-11" onclick={confirmDisconnect}>{$_("linkedAccounts.disconnect")}</button>
    </div>
  </div>
  <form method="dialog" class="modal-backdrop">
    <button aria-label={$_("linkedAccounts.cancel")} onclick={cancelDisconnect}>close</button>
  </form>
</dialog>
