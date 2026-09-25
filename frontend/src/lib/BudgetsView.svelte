<script>
  // Router-outlet view for the budgets list/overview page (#241) — reached via
  // the /budgets-list command, the sidebar "Budgets" entry, or an assistant
  // OPEN_BUDGETS_LIST turn. Sibling to ./BudgetDetails.svelte and
  // ./CategoriesView.svelte: self-fetches via the shared `fetchApi` helper
  // rather than receiving budget data as a prop.
  import { _ } from "svelte-i18n";
  import { ArrowLeft, AlertTriangle } from "lucide-svelte";
  import {
    canSwitchTo,
    resolveRollupNames,
    ownershipLabel,
    UNKNOWN_ROLLUP_LABEL,
    isActivationKey,
    switchErrorKey,
  } from "./budgetsView.js";

  let { fetchApi, activeBudget, onSwitch, onBudgetUpdated, onBack, onOpenDetails } = $props();

  let budgets = $state([]);
  let loading = $state(false);
  let error = $state("");
  let switchingId = $state(null);
  let switchError = $state("");

  // Translates the pure-module's UNKNOWN_ROLLUP_LABEL sentinel (never itself user-facing —
  // see budgetsView.js) into the localized string at render time.
  function rollupName(name) {
    return name === UNKNOWN_ROLLUP_LABEL ? $_("budgetsList.unknownRollupBudget") : name;
  }

  // `afterSwitch: true` is the post-switch refresh handleSwitch issues below — the switch
  // already succeeded server-side, so a failure here must NOT nuke the page into the full
  // "failed to load" error state (that would make a successful switch look like it broke the
  // page). Instead it keeps showing the last-known-good `budgets` and surfaces a distinct,
  // non-blocking notice via `switchError` (mirrors BudgetDetails.svelte's own `afterSave`
  // guard on its `load()`).
  //
  // Every failure path here also console.error()s — this app has no telemetry/logging layer
  // (see BudgetDetails.svelte's lookupName for the same rationale), so this is the only place
  // a "why did loading/switching fail" question is ever answerable during debugging.
  async function load({ afterSwitch = false } = {}) {
    if (!afterSwitch) {
      loading = true;
      error = "";
    }
    try {
      budgets = await fetchApi("/budgets");
    } catch (e) {
      console.error(
        afterSwitch ? "BudgetsView: post-switch refresh failed" : "BudgetsView: load failed",
        e,
      );
      if (afterSwitch) {
        switchError = $_("budgetsList.switchSucceededRefreshFailed");
      } else {
        error = e?.message || $_("budgetsList.loadError");
        budgets = [];
      }
    } finally {
      if (!afterSwitch) loading = false;
    }
  }

  // Mounting IS the "open" signal (the outlet's {#if route === "budgetsList"}
  // owns mount/unmount) — this page has no reactive input parameter to react
  // to the way budgetId/activeBudget are for the sibling pages.
  $effect(() => {
    load();
  });

  async function handleSwitch(budget) {
    if (!canSwitchTo(budget, activeBudget) || switchingId) return;
    if (!onSwitch) {
      // A missing onSwitch handler means the switch can never actually happen server-side —
      // App.svelte always wires onSwitch={setActiveBudget} today, so this can't fire in
      // practice, but treat it as a genuine failure (not a silent no-op that falls through to
      // the "success" refresh path below) as defense-in-depth for a future caller/refactor.
      console.error("BudgetsView: handleSwitch called with no onSwitch handler wired");
      switchError = $_("budgetsList.switchErrorGeneric");
      return;
    }
    switchingId = budget.id;
    switchError = "";
    try {
      await onSwitch(budget.id);
    } catch (e) {
      // Only a genuine switch failure gets the "couldn't switch" message — see the
      // separate try/finally below for the post-switch refresh, which must never be
      // mislabeled as a failed switch. A 403/404 gets copy naming the specific cause
      // (#391); anything else keeps the interpolated-or-generic fallback.
      console.error(`BudgetsView: switch failed for budget ${budget.id}`, e);
      const specific = switchErrorKey(e?.status);
      if (specific) {
        switchError = $_(specific);
      } else {
        switchError = e?.message
          ? $_("budgetsList.switchError", { values: { error: e.message } })
          : $_("budgetsList.switchErrorGeneric");
      }
      switchingId = null;
      return;
    }
    // The switch itself succeeded past this point. load({afterSwitch: true}) keeps showing
    // the last-known-good list (with a non-blocking switchError notice) instead of the full
    // error state if the refresh itself fails. onBudgetUpdated (App.svelte's fetchBudgets)
    // swallows its own failures internally, so nothing here is expected to throw beyond that
    // — the try/catch/finally is defense-in-depth only, so switchingId can never get stuck
    // disabled, and an unexpected throw is still logged rather than silently dropped.
    try {
      await load({ afterSwitch: true });
      onBudgetUpdated?.();
    } catch (e) {
      console.error("BudgetsView: post-switch refresh failed unexpectedly", e);
    } finally {
      switchingId = null;
    }
  }

  function handleOpen(budget) {
    onOpenDetails?.(budget.id);
  }
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("budgetsList.title")}</h3>
    <button type="button" class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" />
      {$_("budgetsList.back")}
    </button>
  </div>

  {#if loading}
    <div
      class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
    >
      <span class="loading loading-spinner loading-lg text-primary"></span>
      <span class="text-sm">{$_("budgetsList.loading")}</span>
    </div>
  {:else if error}
    <div class="alert alert-error">
      <AlertTriangle class="w-5 h-5" />
      <span>{error}</span>
    </div>
  {:else if budgets.length === 0}
    <p class="text-sm text-base-content/50 py-4">{$_("budgetsList.empty")}</p>
  {:else}
    {#if switchError}
      <div class="alert alert-error mb-3">
        <AlertTriangle class="w-5 h-5" />
        <span>{switchError}</span>
      </div>
    {/if}
    <div class="flex flex-col gap-3">
      {#each budgets as budget (budget.id)}
        {@const isActive = budget.id === activeBudget?.id}
        {@const ownership = ownershipLabel(budget)}
        {@const rollup = resolveRollupNames(budget, budgets)}
        {@const switchable = canSwitchTo(budget, activeBudget)}
        <!--
          This card is itself a role="button" open-details activation target, but it also wraps
          a real <button> (the Switch action below) when `switchable`. Both handlers below guard
          with `e.target.closest("button")` so a click/keypress that originated on the nested
          Switch button doesn't ALSO trigger handleOpen via bubbling up to the card.

          For onclick this guard is belt-and-suspenders: the Switch button's own onclick already
          calls e.stopPropagation(), so the click never reaches this div's onclick in the first
          place. The closest("button") check here is a second, independent line of defense.

          For onkeydown this guard is load-bearing, not redundant: the Switch button has no
          onkeydown/stopPropagation of its own, so a native keydown (e.g. focusing Switch and
          pressing Enter/Space) bubbles straight up into this card's onkeydown. Without this
          check, activating Switch via keyboard would ALSO call handleOpen and pop open budget
          details behind/alongside the switch — this was the actual bug fixed in d45accc.
        -->
        <div
          class="rounded-xl bg-base-100 border border-base-300 p-3 cursor-pointer hover:border-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-primary"
          role="button"
          tabindex="0"
          aria-label={$_("budgetsList.openDetailsLabel", { values: { name: budget.name } })}
          onclick={(e) => {
            if (e.target.closest("button")) return;
            handleOpen(budget);
          }}
          onkeydown={(e) => {
            if (e.target.closest("button")) return;
            if (isActivationKey(e)) {
              e.preventDefault();
              handleOpen(budget);
            }
          }}
        >
          <div class="flex items-start justify-between gap-2">
            <div class="flex items-center gap-2 flex-wrap">
              <span class="font-semibold text-sm">{budget.name}</span>
              {#if isActive}
                <span class="badge badge-success badge-sm">{$_("budgetsList.activeBadge")}</span>
              {/if}
              {#if budget.archived_at}
                <span class="badge badge-warning badge-sm">{$_("budgetsList.archivedBadge")}</span>
              {/if}
            </div>
            {#if switchable}
              <button
                type="button"
                class="btn btn-sm btn-primary shrink-0"
                disabled={switchingId !== null}
                onclick={(e) => { e.stopPropagation(); handleSwitch(budget); }}
              >
                {switchingId === budget.id
                  ? $_("budgetsList.switching")
                  : $_("budgetsList.switchAction")}
              </button>
            {/if}
          </div>

          <div class="text-xs text-base-content/60 mt-1.5 flex flex-col gap-0.5">
            {#if ownership.kind === "owned"}
              <span>{$_("budgetsList.ownedLabel")}</span>
            {:else}
              <span>
                {ownership.ownerName
                  ? $_("budgetsList.sharedByLabel", { values: { name: ownership.ownerName } })
                  : $_("budgetsList.sharedByUnknown")}
              </span>
              <span>
                {budget.permission_level === "edit"
                  ? $_("budgetsList.permissionEdit")
                  : $_("budgetsList.permissionView")}
              </span>
            {/if}
            {#if rollup.parentName}
              <span>{$_("budgetsList.rollupParent", { values: { name: rollupName(rollup.parentName) } })}</span>
            {/if}
            {#if rollup.childNames.length > 0}
              <span
                >{$_("budgetsList.rollupChildren", {
                  values: { names: rollup.childNames.map(rollupName).join(", ") },
                })}</span
              >
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
