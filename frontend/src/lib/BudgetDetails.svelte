<script>
  // Router-outlet view for a single budget's details (#240) — reached by
  // clicking the header's budget name. Sibling to ./CategoriesView.svelte and
  // ./Insights.svelte: self-fetches via the shared `fetchApi` helper rather
  // than receiving budget data as a prop, so it works for ANY budget id in
  // the hash, not just the currently active budget.
  import { _ } from "svelte-i18n";
  import { ArrowLeft, AlertTriangle, Pencil } from "lucide-svelte";
  import {
    TIME_FRAMES,
    BUDGET_STRATEGIES,
    buildUpdatePayload,
    buildEditPatch,
    rollupSummary,
    isReadOnly,
    isRollupLinked,
  } from "./budgetDetails.js";

  let { fetchApi, budgetId, onBack, onBudgetUpdated } = $props();

  let budget = $state(null);
  let loading = $state(false);
  let error = $state("");

  let parentName = $state(null);
  let childNames = $state([]);

  let editingField = $state(null); // null | "name" | "time_frame" | "budget_type" | "budget_strategy"
  let draftValue = $state("");
  let saving = $state(false);
  let saveError = $state("");

  // Best-effort name lookup for one related budget id. A failure resolves to
  // a placeholder rather than rejecting — one bad lookup must never block the
  // page or any other successfully-resolved name (see design doc) — but the
  // failure is still logged: a 404 (deleted/unshared budget) is expected and
  // uninteresting, while a 401/500/network failure silently collapsing into
  // "unknown budget" with no trace anywhere would hide a real problem (this
  // app has no logging/telemetry layer, so this console.error is the only
  // place such a failure is ever recorded).
  async function lookupName(id) {
    try {
      const b = await fetchApi(`/budgets/${id}`);
      return b?.name ?? $_("budgetDetails.unknownBudget");
    } catch (e) {
      console.error(`BudgetDetails: name lookup failed for budget ${id}`, e);
      return $_("budgetDetails.unknownBudget");
    }
  }

  async function loadRollupNames(loadedBudget, forId) {
    const parentId = loadedBudget?.rollup_parent_id ?? null;
    const childIds = loadedBudget?.rollup_child_ids ?? [];

    const [parentResult, childResults] = await Promise.all([
      parentId ? lookupName(parentId) : Promise.resolve(null),
      Promise.all(childIds.map((id) => lookupName(id))),
    ]);

    // Discard a stale response if budgetId changed while these lookups were
    // in flight (mirrors CategoriesView's stale-response guard).
    if (budgetId !== forId) return;
    parentName = parentResult;
    childNames = childResults;
  }

  // `afterSave: true` is the post-PUT refresh commitEdit() issues (see
  // below) — a fundamentally different situation from the initial/
  // budget-switch load: the edit already succeeded server-side, so a failure
  // here must NOT nuke the page into the full "budget not found" error state
  // (that would make a successful save look like data loss). Instead it
  // keeps showing the last-known-good `budget` and surfaces a distinct,
  // non-blocking notice via `saveError`.
  async function load({ afterSave = false } = {}) {
    if (!budgetId) return;
    const forId = budgetId;
    error = "";
    saveError = "";
    if (!afterSave) {
      loading = true;
      editingField = null;
      // A genuine navigation to a (possibly different) budget always clears
      // any stale `saving` flag left behind by an abandoned in-flight edit on
      // the PREVIOUSLY displayed budget (see commitEdit's forId guard below)
      // — otherwise the newly-displayed budget's edit affordances could stay
      // stuck disabled until that unrelated stale save eventually settles.
      saving = false;
    }
    try {
      const res = await fetchApi(`/budgets/${forId}`);
      if (budgetId !== forId) return;
      budget = res;
      parentName = null;
      childNames = [];
      // Awaited (not fire-and-forget) so the details panel — including the
      // rollup section — never renders until parent/child names are resolved.
      // This deliberately avoids a separate "names still loading" substate: a
      // brief extra wait before the whole page appears is preferable to a
      // rollup section that flashes an "unknown budget" placeholder for an
      // entry that's actually still loading (not actually a failed lookup).
      await loadRollupNames(res, forId);
    } catch (e) {
      if (budgetId !== forId) return;
      console.error(`BudgetDetails: load failed for budget ${forId}`, e);
      if (afterSave) {
        saveError = $_("budgetDetails.saveSucceededRefreshFailed");
      } else {
        error = e?.message || $_("budgetDetails.loadError");
        budget = null;
      }
    } finally {
      if (budgetId === forId) loading = false;
    }
  }

  // Mounting IS the "open" signal (the outlet's {#if route === "budgetDetails"}
  // owns mount/unmount). Re-fetch if budgetId changes while mounted (e.g. the
  // user clicks a different budget's name without leaving the details page).
  $effect(() => {
    budgetId;
    load();
  });

  function startEdit(field, currentValue) {
    if (!budget || isReadOnly(budget) || saving) return;
    if ((field === "budget_type" || field === "budget_strategy") && isRollupLinked(budget)) return;
    editingField = field;
    draftValue = currentValue;
    saveError = "";
  }

  // `saving` re-entrancy guard: setting `disabled={saving}` on the input/
  // select that currently has focus (see the template below) makes the
  // browser fire a native `blur` event on it — which is wired to call
  // commitEdit() (the name field) or cancelEdit() (the select fields). Without
  // this guard, that synthetic blur re-enters commitEdit() a second time (a
  // duplicate PUT) or fires cancelEdit() mid-save (prematurely resetting
  // editingField/draftValue while the first commit's PUT+refetch is still in
  // flight, flashing the UI back to the stale value). Both functions bail
  // immediately once `saving` is true; the in-flight commitEdit() call is the
  // only one that will actually run to completion.
  function cancelEdit() {
    if (saving) return;
    editingField = null;
    draftValue = "";
    saveError = "";
  }

  async function commitEdit() {
    if (!budget || !editingField || saving) return;
    // Captured up front: if the user navigates to a DIFFERENT budget's
    // details page while this save is still in flight (the component stays
    // mounted across a budgetId change), the eventual PUT response belongs to
    // a budget that's no longer displayed. That fresh budget's own load()
    // call already reset `saving`/`editingField` for itself; this stale
    // continuation must not overwrite that with the old budget's outcome.
    const forId = budgetId;

    const patch = buildEditPatch(editingField, draftValue, budget);
    if (!patch) {
      // Empty/whitespace-only name, or the value is unchanged — nothing to
      // persist (see buildEditPatch's contract in budgetDetails.js).
      cancelEdit();
      return;
    }

    saving = true;
    saveError = "";
    try {
      const payload = buildUpdatePayload(budget, patch);
      await fetchApi(`/budgets/${forId}`, {
        method: "PUT",
        body: JSON.stringify(payload),
      });
      if (budgetId !== forId) return;
      // The edit itself succeeded — close the editor now regardless of
      // whether the follow-up refresh below succeeds.
      editingField = null;
      draftValue = "";
      // The PUT response's rollup_child_ids/aggregated_* fields are not
      // reliable (update_budget never calls the server's .with_rollup() —
      // see design doc), so re-fetch via GET rather than trusting it.
      // `afterSave: true` keeps the last-known-good `budget` on screen (with
      // a non-blocking notice) instead of the full error state if THIS
      // refresh fails — a refresh failure must not look like the save itself
      // failed or destroyed data.
      await load({ afterSave: true });
      if (budgetId === forId) onBudgetUpdated?.();
    } catch (e) {
      if (budgetId === forId) {
        console.error(`BudgetDetails: save failed for budget ${forId}`, e);
        saveError = e?.message || $_("budgetDetails.updateError");
      }
    } finally {
      if (budgetId === forId) saving = false;
    }
  }

  function handleEditKeydown(e) {
    if (e.key === "Enter") commitEdit();
    if (e.key === "Escape") {
      // Stop the keydown from bubbling to App.svelte's window-level handler,
      // which treats any Escape while route !== "chat" as "leave the whole
      // outlet page" (navigate("chat")). Without this, cancelling an
      // in-progress field edit would also evict the user from the entire
      // details page — Escape should only cancel the field, matching
      // Sidebar.svelte's rename Escape behavior (cancels the rename, stays on
      // the conversation list).
      e.stopPropagation();
      cancelEdit();
    }
  }

  let readOnly = $derived(isReadOnly(budget));
  let rollupLocked = $derived(isRollupLinked(budget));
  let rollup = $derived(rollupSummary(budget, parentName, childNames));

  function fmtRenewalDate(iso) {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) {
      console.error("BudgetDetails: invalid next_renewal_at from backend", iso);
      return "";
    }
    return d.toLocaleDateString(undefined, { timeZone: "UTC" });
  }

  // Resolves a TIME_FRAMES value ("monthly"/"quarterly"/"yearly") to its
  // translated label. `time_frame` is a free-form VARCHAR(50) server-side
  // (not a DB enum) — `previous_period_window`'s "any unrecognized time_frame
  // falls back to monthly" only governs THAT function's own rollover-period
  // math, it does not rewrite the stored column, so GET can still return a
  // legacy/manually-set value outside TIME_FRAMES. Building an i18n key from
  // an unrecognized value would resolve to nothing and render the raw,
  // untranslated key string (e.g. "budgetDetails.periodWeekly") — guard by
  // only building the key for a value actually in TIME_FRAMES, and fall back
  // to the raw value otherwise so the page shows *something* legible instead
  // of an internal-looking key.
  function timeFrameLabel(tf) {
    if (!tf) return "";
    if (!TIME_FRAMES.includes(tf)) return tf;
    const key = `budgetDetails.period${tf[0].toUpperCase()}${tf.slice(1)}`;
    return $_(key);
  }
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">
      {budget ? budget.name : $_("budgetDetails.loading")}
    </h3>
    <button
      type="button"
      class="btn btn-sm btn-ghost gap-1.5"
      onclick={() => onBack?.()}
    >
      <ArrowLeft class="w-4 h-4" />
      {$_("budgetDetails.back")}
    </button>
  </div>

  {#if loading}
    <div
      class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
    >
      <span class="loading loading-spinner loading-lg text-primary"></span>
      <span class="text-sm">{$_("budgetDetails.loading")}</span>
    </div>
  {:else if error}
    <div class="alert alert-error">
      <AlertTriangle class="w-5 h-5" />
      <span>{error}</span>
    </div>
  {:else if !budget}
    <p class="text-sm text-base-content/50 py-4">{$_("budgetDetails.notFound")}</p>
  {:else}
    <div class="flex flex-col gap-3">
      {#if budget.closed_at}
        <div class="alert alert-warning text-sm">{$_("budgetDetails.closedNotice")}</div>
      {/if}
      {#if budget.archived_at}
        <div class="alert alert-warning text-sm">{$_("budgetDetails.archivedNotice")}</div>
      {/if}

      <!-- Name -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.nameLabel")}
        </div>
        {#if editingField === "name"}
          <input
            type="text"
            bind:value={draftValue}
            onblur={commitEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          />
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("name", budget.name)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>{budget.name}</span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "name" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Type -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.typeLabel")}
        </div>
        {#if editingField === "budget_type"}
          <select
            bind:value={draftValue}
            onchange={commitEdit}
            onblur={cancelEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          >
            <option value="time_based">{$_("budgetDetails.typePeriodic")}</option>
            <option value="project">{$_("budgetDetails.typeProject")}</option>
          </select>
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default{rollupLocked
              ? ' cursor-default opacity-60'
              : ''}"
            disabled={readOnly}
            aria-disabled={rollupLocked}
            onclick={() => startEdit("budget_type", budget.budget_type)}
            title={readOnly
              ? undefined
              : rollupLocked
                ? $_("budgetDetails.rollupLockedHint")
                : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_type === "project"
                ? $_("budgetDetails.typeProject")
                : $_("budgetDetails.typePeriodic")}
            </span>
            {#if !readOnly && !rollupLocked}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "budget_type" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Strategy (#300) -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.strategyLabel")}
        </div>
        {#if editingField === "budget_strategy"}
          <select
            bind:value={draftValue}
            onchange={commitEdit}
            onblur={cancelEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          >
            {#each BUDGET_STRATEGIES as bs (bs)}
              <option value={bs}>
                {bs === "zero_based"
                  ? $_("budgetDetails.strategyZeroBased")
                  : $_("budgetDetails.strategyLimitSpentRemaining")}
              </option>
            {/each}
          </select>
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default{rollupLocked
              ? ' cursor-default opacity-60'
              : ''}"
            disabled={readOnly}
            aria-disabled={rollupLocked}
            onclick={() => startEdit("budget_strategy", budget.budget_strategy)}
            title={readOnly
              ? undefined
              : rollupLocked
                ? $_("budgetDetails.rollupLockedHint")
                : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_strategy === "zero_based"
                ? $_("budgetDetails.strategyZeroBased")
                : $_("budgetDetails.strategyLimitSpentRemaining")}
            </span>
            {#if !readOnly && !rollupLocked}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "budget_strategy" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Period (hidden for project budgets — they track lifetime spend, not a rotating period) -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.periodLabel")}
        </div>
        {#if budget.budget_type === "project"}
          <p class="text-sm text-base-content/60">{$_("budgetDetails.projectNote")}</p>
        {:else if editingField === "time_frame"}
          <select
            bind:value={draftValue}
            onchange={commitEdit}
            onblur={cancelEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          >
            {#each TIME_FRAMES as tf (tf)}
              <option value={tf}>{timeFrameLabel(tf)}</option>
            {/each}
          </select>
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("time_frame", budget.time_frame)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>
              {timeFrameLabel(budget.time_frame)}
            </span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "time_frame" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Rollover (display only; excluded for project budgets, matching budgetDisplay.js) -->
      {#if budget.budget_type !== "project"}
        <div class="rounded-xl bg-base-100 border border-base-300 p-3">
          <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
            {$_("budgetDetails.rolloverLabel")}
          </div>
          <p class="text-sm">
            {#if !budget.rollover_enabled}
              {$_("budgetDetails.rolloverOff")}
            {:else if budget.has_partial_category_rollover}
              {$_("budgetDetails.rolloverOnSome")}
            {:else}
              {$_("budgetDetails.rolloverOnAll")}
            {/if}
          </p>
        </div>

        <!-- Auto-renew (display only; excluded for project budgets) -->
        <div class="rounded-xl bg-base-100 border border-base-300 p-3">
          <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
            {$_("budgetDetails.autoRenewLabel")}
          </div>
          <p class="text-sm">
            {#if !budget.auto_renew}
              {$_("budgetDetails.autoRenewOff")}
            {:else if budget.next_renewal_at}
              {$_("budgetDetails.autoRenewOnNext", {
                values: { date: fmtRenewalDate(budget.next_renewal_at) },
              })}
            {:else}
              {$_("budgetDetails.autoRenewOn")}
            {/if}
          </p>
        </div>
      {/if}

      <!-- Rollup relationships (read-only list, not navigable in v1) -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.rollupLabel")}
        </div>
        {#if !rollup.hasParent && !rollup.hasChildren}
          <p class="text-sm text-base-content/60">{$_("budgetDetails.rollupNone")}</p>
        {:else}
          <div class="flex flex-col gap-1 text-sm">
            {#if rollup.hasParent}
              <p>
                {$_("budgetDetails.rollupParent", {
                  values: { name: rollup.parentName ?? $_("budgetDetails.unknownBudget") },
                })}
              </p>
            {/if}
            {#if rollup.hasChildren}
              <p>
                {$_("budgetDetails.rollupChildren", { values: { names: rollup.childNames.join(", ") } })}
              </p>
            {/if}
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>
