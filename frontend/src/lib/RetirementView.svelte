<script>
  // Retirement planner dashboard (nels#469), the last child of epic #454. This
  // route composes the pure helpers in retirementView.js (the only unit-tested
  // part of this feature's frontend) against the three user-scoped endpoints:
  //   GET  /retirement/profile   (ungated; null when no profile exists)
  //   GET  /assets               (Pro-gated; the caller's OWN assets, §20)
  //   POST /retirement/projection (Pro-gated; overrides are ephemeral, §25)
  //   PUT  /retirement/profile   (Pro-gated; persists what-if slider choices)
  // plus the #468 investments ingestion endpoints behind the connect flow.
  //
  // THE FOUR COMPLIANCE CONSTRAINTS live here in markup, not just in helpers:
  //   1. The headline is the BAND RANGE; the deterministic figure and the
  //      percent-of-goal dial are secondary and sit inside the same card.
  //   2. Assumptions render on the same screen as the number AND are editable.
  //   3. The fixed hypothetical-illustration disclosure sits directly under
  //      the band and is rendered by DISCLOSURE_KEY, never free-typed.
  //   4. No recommendation is emitted anywhere — the register-interest copy is
  //      a factual statement about product scope, not advice.
  import { _ } from "svelte-i18n";
  import { get } from "svelte/store";
  import { onMount } from "svelte";
  import { ArrowLeft, PiggyBank, AlertTriangle, RefreshCw, Wallet, Link2, Save, TrendingUp, Landmark } from "lucide-svelte";  import { formatAmount } from "./money.js";
  import { currentLocale } from "./i18n/index.js";
  import { loadPlaidLink } from "./linkedAccounts.js";
  import {
    VIEW_STATE,
    viewStateFor,
    bandParts,
    dialPercent,
    dialBand,
    percentLabel,
    needleAngle,
    incomeSegments,
    depletionInfo,
    activeAssets,
    buildProjectionOverrides,
    buildProfileSaveBody,
    debounce,
    segmentLabelKey,
    assetTypeLabelKey,
    taxTreatmentLabelKey,
    DISCLOSURE_KEY,
    SLIDER_DISCLOSURE_KEY,
  } from "./retirementView.js";

  let { fetchApi, onUpgrade, onBack } = $props();

  const t = (key, opts) => get(_)(key, opts);

  // --- load state ------------------------------------------------------------
  let profile = $state(null);
  let assets = $state([]);
  let projection = $state(null);
  let loadError = $state(null);
  let loading = $state(true);

  // What-if controls. `taxRate` is projection-only (the profile has no
  // tax-rate column) — buildProfileSaveBody strips it.
  let controls = $state({
    preTax: 0,
    roth: 0,
    realReturn: 5,
    retirementAge: 65,
    grossIncome: 0,
    replacementRatio: 0.75,
    taxRate: 0.15,
    ssClaimingAge: 0,
  });
  let recomputing = $state(false);
  // Guards a stale what-if response: only the LATEST debounced recompute may
  // write `projection`. Without this, an in-flight response landing after the
  // user dragged again would clobber the newer numbers with older ones.
  let recomputeSeq = $state(0);
  let saveMsg = $state("");
  let saveFailed = $state(false);

  // Connect / classify state (#468).
  let connecting = $state(false);
  let connectError = $state("");
  let pending = $state([]); // [{linked_account_id, display_name, last4}]
  let classifySessionId = $state(null);
  let classifyChoices = $state({});
  let classifying = $state(false);
  let classifyError = $state("");
  let classifyEl = $state(null);

  let backButtonEl = $state(null);

  // Mounting this route IS the "opened" signal (matches AccountsView.svelte's
  // convention) — load the three user-scoped reads and move focus to the back
  // button for keyboard/a11y users.
  onMount(load);
  $effect(() => {
    backButtonEl?.focus();
  });

  const currentState = $derived(
    viewStateFor({ profile, projection, assets, error: loadError }),
  );

  // The dial: percent-of-goal marker plus the band as a tinted arc.
  const DIAL_R = 52;
  const dialC = $derived(2 * Math.PI * DIAL_R);
  const goalPct = $derived(dialPercent(projection?.percent_of_goal));
  const band = $derived(dialBand(projection));
  const bandStart = $derived(band ? band.lowPct * dialC : 0);
  const bandLen = $derived(band ? (band.highPct - band.lowPct) * dialC : 0);
  const goalAngle = $derived(needleAngle(goalPct));
  const headlineBand = $derived(bandParts(projection?.percentile_band));

  // The SS claiming-age slider is only meaningful when the user has a COMPLETE
  // entered SS triple — the server's override applies only then, so a slider
  // on a half-entered profile would silently do nothing.
  const hasCompleteSs = $derived(
    profile?.ss_monthly_benefit != null &&
      profile?.ss_benefit_at_age_months != null &&
      profile?.ss_claiming_age_months != null,
  );

  const assumptionRows = $derived.by(() => {
    const a = projection?.assumptions;
    if (!a) return [];
    return [
      { labelKey: "assumptionCurrentAge", value: String(a.current_age) },
      { labelKey: "assumptionRealReturn", value: `${a.expected_real_return}%` },
      { labelKey: "assumptionLifeExpectancy", value: String(a.life_expectancy_age) },
      { labelKey: "assumptionPreTax", value: `${a.contribution_rate_pre_tax}%` },
      { labelKey: "assumptionRoth", value: `${a.contribution_rate_roth}%` },
      { labelKey: "assumptionGrossIncome", value: formatAmount(a.current_gross_income, "USD") },
      { labelKey: "assumptionReplacementRatio", value: percentLabel(a.target_replacement_ratio) },
      { labelKey: "assumptionTaxRate", value: `${Math.round(a.effective_tax_rate * 100)}%` },
      // Social Security row: the AC's "figure and its source" must be a row in
      // the assumptions read-out, not only a footnote under the income bar.
      // It carries DATA (not resolved copy) so the template renders
      // ssAdjustedLabel / ssNotEntered through the reactive `$_`, exactly like
      // every other row — a `t()` call here would freeze the locale.
      {
        key: "socialSecurity",
        labelKey: "assumptionSocialSecurity",
        ssAdjusted: a.ss_adjusted_monthly_benefit,
        ssClaimingAge: a.ss_claiming_age_used,
      },
    ];
  });

  const segs = $derived(
    incomeSegments(projection?.income_sources, projection?.assumptions?.ss_adjusted_monthly_benefit == null),
  );
  const hasVisibleIncome = $derived(segs.some((s) => s.pct > 0));
  const dep = $derived(depletionInfo(projection?.depletes, projection?.depletion_age));
  const listedAssets = $derived(activeAssets(assets));

  const SEGMENT_STYLES = {
    ownSavings: "bg-primary",
    employer: "bg-secondary",
    socialSecurity: "bg-info",
    otherAssets: "bg-accent",
    gap: "bg-error",
  };

  const SLIDERS = [
    { field: "preTax", key: "controlPreTax", min: 0, max: 30, step: 1, suffix: "%" },
    { field: "roth", key: "controlRoth", min: 0, max: 30, step: 1, suffix: "%" },
    { field: "realReturn", key: "controlRealReturn", min: -5, max: 15, step: 0.5, suffix: "%" },
    { field: "retirementAge", key: "controlRetirementAge", min: 55, max: 75, step: 1, suffix: "" },
    // A live what-if over `ss_claiming_age_months`. Rendered only when
    // `hasCompleteSs`; never persisted by the save button (see
    // buildProjectionOverrides/buildProfileSaveBody).
    { field: "ssClaimingAge", key: "controlSsClaimingAge", min: 62, max: 70, step: 1, suffix: "" },
  ];

  function seedControlsFrom(p) {
    if (!p) return;
    controls = {
      preTax: p.contribution_rate_pre_tax ?? 0,
      roth: p.contribution_rate_roth ?? 0,
      realReturn: p.expected_real_return ?? 5,
      retirementAge: p.target_retirement_age ?? 65,
      grossIncome: p.current_gross_income ?? 0,
      replacementRatio: p.target_replacement_ratio ?? 0.75,
      taxRate: controls.taxRate,
      // Only set when the profile has a stored claiming age (whole years from
      // the months column); a no-SS profile keeps 0 and omits the override.
      ssClaimingAge: p.ss_claiming_age_months ? Math.round(p.ss_claiming_age_months / 12) : 0,
    };
  }

  // Trailing-edge debounce: the deterministic headline recomputes ~150ms after
  // the last slider move (the band follows on the same settle, since the engine
  // computes both in one request). The last-good projection stays rendered and
  // is dimmed while a recompute is in flight — never blanked.
  const recompute = debounce(async (next) => {
    recomputing = true;
    const seq = ++recomputeSeq;
    try {
      const proj = await fetchApi("/retirement/projection", {
        method: "POST",
        body: JSON.stringify(buildProjectionOverrides(next)),
      });
      if (seq !== recomputeSeq) return; // a newer what-if has already landed
      if (proj) projection = proj;
    } catch (e) {
      // A failed what-if recompute keeps the last-good projection; only a load
      // with NO projection yet escalates to the error state.
      if (seq === recomputeSeq && !projection) loadError = e;
    } finally {
      if (seq === recomputeSeq) recomputing = false;
    }
  }, 150);

  function onControlInput() {
    saveMsg = "";
    saveFailed = false;
    recompute(controls);
  }

  async function load() {
    loading = true;
    loadError = null;
    try {
      const [profRes, assetRes] = await Promise.all([
        fetchApi("/retirement/profile"),
        fetchApi("/assets"),
      ]);
      profile = profRes ?? null;
      assets = Array.isArray(assetRes) ? assetRes : [];
      seedControlsFrom(profile);
      if (profile && profile.supported !== false && assets.length > 0) {
        const proj = await fetchApi("/retirement/projection", {
          method: "POST",
          body: JSON.stringify(buildProjectionOverrides(controls)),
        });
        if (proj) projection = proj;
      }
    } catch (e) {
      loadError = e;
    } finally {
      loading = false;
    }
  }

  async function saveToProfile() {
    saveMsg = "";
    saveFailed = false;
    try {
      await fetchApi("/retirement/profile", {
        method: "PUT",
        body: JSON.stringify(buildProfileSaveBody(controls)),
      });
      saveMsg = t("retirement.assumptionsSaved");
    } catch (e) {
      saveFailed = true;
    }
  }

  // --- connect + classify (#468) --------------------------------------------

  async function connect() {
    connecting = true;
    connectError = "";
    try {
      const created = await fetchApi("/investments/link-token", { method: "POST" });
      const token = created?.link_token;
      const sessionId = created?.session_id;
      if (!token || !sessionId) throw new Error("bad link-token response");
      const Plaid = await loadPlaidLink();
      const response = await new Promise((resolve, reject) => {
        const handler = Plaid.create({
          token,
          onSuccess: async (publicToken) => {
            try {
              const resp = await fetchApi("/investments/complete", {
                method: "POST",
                body: JSON.stringify({ session_id: sessionId, public_token: publicToken }),
              });
              resolve(resp);
            } catch (e) {
              reject(e);
            }
          },
          onExit: (err) => {
            if (err) reject(new Error(err.error_message || ""));
            else resolve({ accounts: [] });
          },
        });
        handler.open();
      });
      const accounts = Array.isArray(response?.accounts) ? response.accounts : [];
      if (accounts.length === 0) {
        // Plain cancel or no new accounts — refresh whatever we have.
        await load();
        return;
      }
      classifySessionId = sessionId;
      pending = accounts;
      classifyChoices = {};
      for (const a of accounts) {
        classifyChoices[a.linked_account_id] = { asset_type: "", tax_treatment: "" };
      }
      classifyEl?.showModal();
    } catch (e) {
      connectError = e?.message || t("retirement.connectError");
    } finally {
      connecting = false;
    }
  }

  async function saveClassification() {
    const classifications = [];
    for (const a of pending) {
      const choice = classifyChoices[a.linked_account_id];
      if (!choice?.asset_type || !choice?.tax_treatment) {
        classifyError = t("retirement.classifyRequired");
        return;
      }
      classifications.push({
        linked_account_id: a.linked_account_id,
        asset_type: choice.asset_type,
        tax_treatment: choice.tax_treatment,
      });
    }
    classifying = true;
    classifyError = "";
    try {
      await fetchApi("/investments/classify", {
        method: "POST",
        body: JSON.stringify({ session_id: classifySessionId, classifications }),
      });
      classifyEl?.close();
      pending = [];
      await load();
    } catch (e) {
      classifyError = t("retirement.classifyError");
    } finally {
      classifying = false;
    }
  }

  function resetClassify() {
    pending = [];
    classifySessionId = null;
    classifyChoices = {};
    classifyError = "";
  }

  const ASSET_TYPES = ["retirement_account", "brokerage", "cash", "pension", "annuity", "real_estate", "other"];
  const TAX_TREATMENTS = ["pre_tax", "roth", "taxable", "hsa", "other"];

  function formatDate(iso) {
    if (!iso) return "";
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "";
    return new Intl.DateTimeFormat(currentLocale()).format(d);
  }
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg flex items-center gap-2">
      <PiggyBank class="w-5 h-5" aria-hidden="true" />
      {$_("retirement.title")}
    </h3>
    <button type="button" bind:this={backButtonEl} class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" /> {$_("retirement.back")}
    </button>
  </div>

  <div class="flex-grow overflow-y-auto">
    {#if loading}
      <p class="text-sm text-base-content/50 py-4 flex items-center gap-2">
        <span class="loading loading-spinner loading-xs"></span>
        {$_("retirement.loading")}
      </p>

    {:else if currentState === VIEW_STATE.error}
      <div class="card bg-base-100 shadow-sm p-5">
        <p class="flex items-center gap-2 text-base-content/70">
          <AlertTriangle class="w-4 h-4 shrink-0" aria-hidden="true" />
          {$_("retirement.loadError")}
        </p>
        <div class="mt-3">
          <button type="button" class="btn btn-sm gap-1.5" onclick={load}>
            <RefreshCw class="w-4 h-4" /> {$_("retirement.tryAgain")}
          </button>
        </div>
      </div>

    {:else if currentState === VIEW_STATE.proRequired}
      <div class="card bg-base-100 shadow-sm p-6 text-center">
        <Wallet class="w-8 h-8 mx-auto text-primary" aria-hidden="true" />
        <h4 class="font-semibold text-lg mt-3">{$_("retirement.proRequiredTitle")}</h4>
        <p class="text-sm text-base-content/70 mt-1 max-w-sm mx-auto">
          {$_("retirement.proRequiredBody")}
        </p>
        <div class="mt-4">
          <button type="button" class="btn btn-primary" onclick={() => onUpgrade?.()}>
            {$_("retirement.proRequiredCta")}
          </button>
        </div>
      </div>

    {:else if currentState === VIEW_STATE.noProfile}
      <div class="card bg-base-100 shadow-sm p-6 text-center">
        <PiggyBank class="w-8 h-8 mx-auto text-primary" aria-hidden="true" />
        <h4 class="font-semibold text-lg mt-3">{$_("retirement.noProfileTitle")}</h4>
        <p class="text-sm text-base-content/70 mt-1 max-w-sm mx-auto">
          {$_("retirement.noProfileBody")}
        </p>
      </div>

    {:else if currentState === VIEW_STATE.unsupported}
      <div class="card bg-base-100 shadow-sm p-6 text-center">
        <Landmark class="w-8 h-8 mx-auto text-primary" aria-hidden="true" />
        <h4 class="font-semibold text-lg mt-3">{$_("retirement.unsupportedTitle")}</h4>
        <p class="text-sm text-base-content/70 mt-1 max-w-sm mx-auto">
          {$_("retirement.unsupportedBody")}
        </p>
        <div class="mt-4">
          <a class="btn btn-primary" href="mailto:support@nels.money?subject=Retirement%20planner%20outside%20the%20US">
            {$_("retirement.registerInterest")}
          </a>
        </div>
      </div>

    {:else if currentState === VIEW_STATE.noAccounts}
      <div class="card bg-base-100 shadow-sm p-6 text-center">
        <Link2 class="w-8 h-8 mx-auto text-primary" aria-hidden="true" />
        <h4 class="font-semibold text-lg mt-3">{$_("retirement.noAccountsTitle")}</h4>
        <p class="text-sm text-base-content/70 mt-1 max-w-sm mx-auto">
          {$_("retirement.noAccountsBody")}
        </p>
        <div class="mt-4 flex flex-col items-center gap-2">
          <button type="button" class="btn btn-primary gap-1.5" disabled={connecting} onclick={connect}>
            {#if connecting}
              <span class="loading loading-spinner loading-xs"></span>
            {:else}
              <Link2 class="w-4 h-4" />
            {/if}
            {connecting ? $_("retirement.connecting") : $_("retirement.connectAccounts")}
          </button>
          {#if connectError}
            <p class="text-xs text-error">{connectError}</p>
          {/if}
        </div>
      </div>

    {:else if currentState === VIEW_STATE.loaded}
      <!-- Headline card: band first, deterministic + dial secondary (constraint 1). -->
      <div
        class="card bg-base-100 shadow-sm p-5 transition-opacity"
        class:opacity-60={recomputing}
        aria-busy={recomputing}
      >
        <div class="flex flex-col sm:flex-row items-center gap-5">
          <!-- Dial. The numeric caption and its label are real text (NOT
               aria-hidden) so the dial's value is readable; only the SVG arc
               and needle are decorative. -->
          <div class="shrink-0 relative w-32 h-32">
            <svg viewBox="0 0 120 120" class="w-full h-full -rotate-90" aria-hidden="true">
              <circle cx="60" cy="60" r={DIAL_R} fill="none" stroke-width="10" class="stroke-base-300" />
              {#if band}
                <circle
                  cx="60" cy="60" r={DIAL_R} fill="none" stroke-width="10"
                  stroke-dasharray={`${bandLen} ${dialC}`}
                  stroke-dashoffset={dialC - bandStart}
                  class="stroke-primary/40"
                />
              {/if}
              <line
                x1="60" y1="60" x2="60" y2="16"
                stroke-width="4" stroke-linecap="round"
                transform={`rotate(${goalAngle} 60 60)`}
                class="stroke-primary"
              />
            </svg>
            <div class="absolute inset-0 flex flex-col items-center justify-center">
              <span class="text-xl font-bold">{percentLabel(projection?.percent_of_goal)}</span>
              <span class="text-[10px] uppercase tracking-wide opacity-60">{$_("retirement.percentLine")}</span>
            </div>
          </div>

          <!-- Band headline -->
          <div class="text-center sm:text-left flex-1 min-w-0">
            {#if headlineBand}
              <h4 class="text-2xl sm:text-3xl font-bold leading-tight">
                {$_("retirement.bandRange", { values: { low: headlineBand.low, high: headlineBand.high } })}
              </h4>
            {:else}
              <h4 class="text-2xl sm:text-3xl font-bold leading-tight">—</h4>
            {/if}
            <p class="text-sm opacity-60 mt-1">{$_("retirement.headlineLabel")}</p>
            <p class="text-sm mt-1 opacity-80">
              {$_("retirement.medianLine", { values: { value: formatAmount(projection?.deterministic_monthly_income, "USD") } })}
            </p>
            {#if projection?.success_rate != null}
              <p class="text-sm mt-1 opacity-80">
                {$_("retirement.successRateLabel")}: {Math.round(projection.success_rate * 100)}%
              </p>
            {/if}
          </div>
        </div>

        <!-- Fixed hypothetical-illustration disclosure (constraint 3). -->
        <p class="text-[11px] text-base-content/50 mt-4 border-t border-base-200 pt-3">
          {$_(DISCLOSURE_KEY)}
        </p>
      </div>

      <!-- Depletion line -->
      <div class="card bg-base-100 shadow-sm p-4 mt-3">
        <h4 class="font-semibold flex items-center gap-2">
          <TrendingUp class="w-4 h-4 text-primary" aria-hidden="true" />
          {$_("retirement.depletionTitle")}
        </h4>
        {#if dep.depletes}
          <p class="text-sm mt-1">
            <span class="text-error font-medium">{$_("retirement.depletionAt", { values: { age: dep.age } })}</span>
          </p>
        {:else}
          <p class="text-sm mt-1 text-base-content/80">{$_("retirement.depletionNone")}</p>
        {/if}
      </div>

      <!-- Stacked income-source bar -->
      <div class="card bg-base-100 shadow-sm p-4 mt-3">
        <h4 class="font-semibold mb-3">{$_("retirement.stackedBarTitle")}</h4>
        {#if hasVisibleIncome}
          <div class="flex w-full h-4 rounded overflow-hidden bg-base-200">
            {#each segs as s}
              {#if s.pct > 0}
                <div class={SEGMENT_STYLES[s.key] ?? "bg-base-300"} style:width="{s.pct}%"></div>
              {/if}
            {/each}
          </div>
          <ul class="mt-3 space-y-1">
            {#each segs as s}
              {#if s.pct > 0}
                <li class="flex items-center gap-2 text-sm">
                  <span class={`w-3 h-3 rounded-sm ${SEGMENT_STYLES[s.key] ?? "bg-base-300"}`}></span>
                  <span class="flex-1">{$_(segmentLabelKey(s.key))}</span>
                  <span class="font-medium">{formatAmount(s.value, "USD")} {$_("retirement.perMonth")}</span>
                </li>
              {/if}
            {/each}
          </ul>
        {:else}
          <p class="text-sm text-base-content/70">{$_("retirement.noIncome")}</p>
        {/if}
        <!-- Social Security status line (absent-vs-zero, #466). When a figure
             was entered, BOTH the entered and the adjusted figures render —
             never the adjusted alone (#466 compliance: the user did not produce
             the adjusted number and cannot check it against their statement).
             The `ssAdjustmentNote` string interpolates entered→adjusted with
             the quoted and claiming ages, and the source line states WHERE the
             figure came from (SSA statement, user-entered) so the screen
             satisfies the AC's "figure and its source". -->
        <div class="mt-3 border-t border-base-200 pt-3 text-sm">
          <span class="font-medium">{$_("retirement.socialSecurityHeading")}:</span>{" "}
          {#if projection?.assumptions?.ss_adjusted_monthly_benefit != null}
            {@const a = projection.assumptions}
            <p class="mt-1">
              {$_("retirement.ssAdjustmentNote", { values: {
                entered: formatAmount(profile?.ss_monthly_benefit, "USD"),
                quotedAge: profile?.ss_benefit_at_age_months != null
                  ? Math.round(profile.ss_benefit_at_age_months / 12)
                  : "—",
                claimingAge: a.ss_claiming_age_used ?? "—",
                adjusted: formatAmount(a.ss_adjusted_monthly_benefit, "USD"),
              } })}
            </p>
            <p class="mt-1 text-xs opacity-70">{$_("retirement.ssSourceUserEntered")}</p>
          {:else}
            <span class="text-base-content/70">
              {$_("retirement.ssNotEntered")} — {$_("retirement.ssNotEnteredHelp")}
            </span>
          {/if}
        </div>
      </div>

      <!-- Assets card -->
      <div class="card bg-base-100 shadow-sm p-4 mt-3">
        <div class="flex items-center justify-between gap-2 mb-2">
          <h4 class="font-semibold">{$_("retirement.assetsTitle")}</h4>
          <button type="button" class="btn btn-sm btn-ghost gap-1.5" disabled={connecting} onclick={connect}>
            {#if connecting}
              <span class="loading loading-spinner loading-xs"></span>
            {:else}
              <Link2 class="w-4 h-4" />
            {/if}
            {connecting ? $_("retirement.connecting") : $_("retirement.connectAccounts")}
          </button>
        </div>
        {#if connectError}
          <p class="text-xs text-error mb-2">{connectError}</p>
        {/if}
        <div class="divide-y divide-base-200">
          {#each listedAssets as asset}
            <div class="flex items-center justify-between gap-2 py-2">
              <div class="min-w-0">
                <div class="truncate font-medium text-sm">{asset.name}</div>
                <div class="text-xs opacity-60">
                  {$_(assetTypeLabelKey(asset.asset_type))} · {$_(taxTreatmentLabelKey(asset.tax_treatment))}
                  {#if asset.balance_as_of}
                    {" "}· {$_("retirement.assetAsOf", { values: { date: formatDate(asset.balance_as_of) } })}
                  {/if}
                </div>
              </div>
              <div class="text-sm font-semibold whitespace-nowrap">
                {asset.current_balance != null
                  ? formatAmount(asset.current_balance, asset.currency)
                  : "—"}
              </div>
            </div>
          {/each}
        </div>
      </div>

      <!-- Assumptions (constraint 2: same screen, visible AND editable). -->
      <div class="card bg-base-100 shadow-sm p-4 mt-3">
        <h4 class="font-semibold mb-2">{$_("retirement.assumptionsTitle")}</h4>
        <dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
          {#each assumptionRows as row}
            {#if row.key === "socialSecurity"}
              <dt class="opacity-60">{$_("retirement.assumptionSocialSecurity")}</dt>
              <dd class="text-right font-medium">
                {#if row.ssAdjusted != null}
                  {formatAmount(row.ssAdjusted, "USD")}{" "}
                  {#if row.ssClaimingAge != null}
                    {$_("retirement.ssAdjustedLabel", { values: { age: row.ssClaimingAge } })}
                  {/if}
                {:else}
                  {$_("retirement.ssNotEntered")}
                {/if}
              </dd>
            {:else}
              <dt class="opacity-60">{$_("retirement." + row.labelKey)}</dt>
              <dd class="text-right font-medium">{row.value}</dd>
            {/if}
          {/each}
        </dl>

        <h4 class="font-semibold mt-4 mb-1">{$_("retirement.controlsTitle")}</h4>
        {#each SLIDERS as s}
          {#if s.field !== "ssClaimingAge" || hasCompleteSs}
            <div class="py-1">
              <div class="flex items-center justify-between text-sm">
                <label for={`ret-control-${s.field}`} class="opacity-80">
                  {$_("retirement." + s.key)}
                </label>
                <span class="font-medium tabular-nums">
                  {controls[s.field]}{s.suffix}
                </span>
              </div>
              <input
                id={`ret-control-${s.field}`}
                type="range"
                min={s.min}
                max={s.max}
                step={s.step}
                class="range range-xs w-full"
                bind:value={controls[s.field]}
                oninput={onControlInput}
              />
            </div>
          {/if}
        {/each}

        <p class="text-[11px] text-base-content/50 mt-1 border-t border-base-200 pt-2">
          {$_(SLIDER_DISCLOSURE_KEY)}
        </p>

        <div class="flex flex-wrap items-center gap-3 mt-3">
          <button type="button" class="btn btn-sm gap-1.5" disabled={recomputing} onclick={saveToProfile}>
            <Save class="w-4 h-4" /> {$_("retirement.saveToProfile")}
          </button>
          {#if saveMsg}
            <span class="text-xs text-success">{saveMsg}</span>
          {:else if saveFailed}
            <span class="text-xs text-error">{$_("retirement.saveFailed")}</span>
          {:else if recomputing}
            <span class="text-xs opacity-50 flex items-center gap-1">
              <span class="loading loading-spinner loading-xs"></span>
              {$_("retirement.recomputing")}
            </span>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>

<!-- Classification modal (#468): appears immediately after a successful link.
     Every newly linked account needs a user-chosen type + tax treatment before
     the `assets` row exists, so this is intentionally NOT dismissible — the
     only routes out are Save (both selects filled per account) or navigation
     away (which unmounts; the unclassified accounts are recoverable via a new
     link, since classify requires a completed session id). -->
<dialog
  bind:this={classifyEl}
  class="modal modal-bottom sm:modal-middle"
  aria-labelledby="ret-classify-title"
  onclose={resetClassify}
  oncancel={(e) => e.preventDefault()}
>
  <div class="modal-box">
    <h3 id="ret-classify-title" class="font-semibold text-lg mb-1">
      {$_("retirement.classifyTitle")}
    </h3>
    <p class="text-sm text-base-content/70 mb-3">{$_("retirement.classifyBody")}</p>

    {#each pending as a}
      <div class="border-t border-base-200 pt-2 mb-3">
        <div class="text-sm font-medium mb-2">{a.display_name || a.last4 || $_("retirement.classifyAccount")}</div>
        <div class="grid grid-cols-2 gap-2">
              <label class="block">
                <span class="text-xs opacity-70 block mb-1">{$_("retirement.assetTypeLabel")}</span>
                <select class="select select-sm select-bordered w-full" bind:value={classifyChoices[a.linked_account_id].asset_type}>
                  <option value="" disabled>…</option>
                  {#each ASSET_TYPES as at}
                    <option value={at}>{$_(assetTypeLabelKey(at))}</option>
                  {/each}
                </select>
              </label>
              <label class="block">
                <span class="text-xs opacity-70 block mb-1">{$_("retirement.taxTreatmentLabel")}</span>
                <select class="select select-sm select-bordered w-full" bind:value={classifyChoices[a.linked_account_id].tax_treatment}>
                  <option value="" disabled>…</option>
                  {#each TAX_TREATMENTS as tt}
                    <option value={tt}>{$_(taxTreatmentLabelKey(tt))}</option>
                  {/each}
                </select>
              </label>
        </div>
      </div>
    {/each}

    {#if classifyError}
      <p class="text-xs text-error mb-2">{classifyError}</p>
    {/if}

    <div class="modal-action">
      <button type="button" class="btn btn-primary gap-1.5" disabled={classifying} onclick={saveClassification}>
        {#if classifying}
          <span class="loading loading-spinner loading-xs"></span>
        {:else}
          <Save class="w-4 h-4" />
        {/if}
        {classifying ? "…" : $_("retirement.saveClassification")}
      </button>
    </div>
  </div>
</dialog>
