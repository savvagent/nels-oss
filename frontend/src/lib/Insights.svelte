<script>
  import { _ } from "svelte-i18n";
  import {
    X,
    TrendingUp,
    Wallet,
    PiggyBank,
    AlertTriangle,
    Lightbulb,
    BarChart3,
  } from "lucide-svelte";
  import { fmtMoney } from "./money.js";

  // `fetchApi` is the shared authenticated request helper from App.svelte; it
  // attaches the bearer token and handles 401 by logging out. Reusing it keeps
  // owner-scoping enforced server-side (the endpoint reads the user from the
  // session token, never a client-supplied id).
  let { fetchApi, onClose } = $props();

  // Period presets mirror the backend report/insights presets.
  const PERIODS = [
    { value: "this_month", key: "insights.periodThisMonth" },
    { value: "last_month", key: "insights.periodLastMonth" },
    { value: "this_year", key: "insights.periodThisYear" },
    { value: "all_time", key: "insights.periodAllTime" },
  ];

  let period = $state("this_month");
  let data = $state(null);
  let loading = $state(false);
  let error = $state("");
  let closeButtonEl = $state(null);

  function fmtPct(p) {
    if (p == null) return "—";
    return `${Math.round(p * 100)}%`;
  }

  async function load() {
    loading = true;
    error = "";
    try {
      data = await fetchApi(`/insights?period=${encodeURIComponent(period)}`);
    } catch (e) {
      error = e?.message || "Failed to load insights";
      data = null;
    } finally {
      loading = false;
    }
  }

  // Mounting IS the "open" signal now — the outlet's {#if route === "insights"}
  // in App.svelte owns mount/unmount (this component no longer gates its own
  // visibility). Re-load whenever `period` changes; run once unconditionally
  // at mount for the initial load, and move focus into the view for
  // keyboard/a11y users.
  //
  // These two effects are DELIBERATELY separate, not a stylistic choice: the
  // focus effect reads `closeButtonEl` only inside a setTimeout callback, so
  // Svelte 5 never tracks it as a dependency and the effect runs exactly once,
  // at mount. If this were merged into the effect above (which tracks
  // `period`), every period change would re-steal focus to the close button,
  // yanking it away from whatever the user just interacted with (e.g. the
  // period selector itself).
  $effect(() => {
    period;
    load();
  });

  $effect(() => {
    setTimeout(() => closeButtonEl?.focus(), 50);
  });

  // Largest bucket spend, for scaling the trend bars. Guard against an empty or
  // all-zero series (avoid divide-by-zero -> NaN widths).
  let trendMax = $derived(
    Math.max(0, ...((data?.trend ?? []).map((t) => t.spent))) || 1,
  );
  let topCatMax = $derived(
    Math.max(0, ...((data?.top_categories ?? []).map((c) => c.spent))) || 1,
  );

  // True when the user genuinely has nothing to show (no budgets at all).
  let isEmpty = $derived(
    !!data &&
      (data.kpis?.budget_count ?? 0) === 0 &&
      (data.trend?.length ?? 0) === 0 &&
      (data.top_categories?.length ?? 0) === 0,
  );
</script>

<div class="flex flex-col h-full">
  <div
    class="rounded-xl bg-base-200 border border-base-300 max-w-3xl w-full mx-auto p-4"
    aria-labelledby="insights-dialog-title"
  >
      <!-- Header -->
      <div class="flex items-center justify-between gap-2 sticky top-0 bg-base-200 z-10 pb-2">
        <h3
          id="insights-dialog-title"
          class="font-bold text-lg flex items-center gap-2"
        >
          <BarChart3 class="w-5 h-5 text-primary" />
          {$_("insights.title")}
        </h3>
        <button
          type="button"
          bind:this={closeButtonEl}
          class="btn btn-sm btn-ghost btn-circle"
          aria-label={$_("insights.dismiss")}
          onclick={() => onClose?.()}
        >
          <X class="w-5 h-5" />
        </button>
      </div>

      <!-- Period selector -->
      <div
        class="join mt-2 flex-wrap"
        role="group"
        aria-label={$_("insights.periodLabel")}
      >
        {#each PERIODS as p}
          <button
            type="button"
            class="join-item btn btn-sm {period === p.value
              ? 'btn-primary'
              : 'btn-ghost'}"
            aria-pressed={period === p.value}
            onclick={() => (period = p.value)}
          >
            {$_(p.key)}
          </button>
        {/each}
      </div>

      {#if loading}
        <!-- Loading state -->
        <div
          class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
        >
          <span class="loading loading-spinner loading-lg text-primary"></span>
          <span class="text-sm">{$_("insights.loading")}</span>
        </div>
      {:else if error}
        <div class="alert alert-error mt-4">
          <AlertTriangle class="w-5 h-5" />
          <span>{error}</span>
        </div>
      {:else if isEmpty}
        <!-- Empty state -->
        <div
          class="flex flex-col items-center justify-center gap-3 py-16 text-center text-base-content/60"
        >
          <BarChart3 class="w-12 h-12 text-base-content/30" />
          <p class="text-sm max-w-xs">{$_("insights.empty")}</p>
        </div>
      {:else if data}
        <!-- KPI cards -->
        <div class="grid grid-cols-2 lg:grid-cols-4 gap-3 mt-4">
          <div class="rounded-xl bg-base-100 border border-base-300 p-3">
            <div
              class="flex items-center gap-1.5 text-xs text-base-content/60 mb-1"
            >
              <Wallet class="w-3.5 h-3.5" />
              {$_("insights.kpiBudgeted")}
            </div>
            <div class="text-lg font-bold text-base-content">
              {fmtMoney(data.kpis.total_budgeted)}
            </div>
          </div>
          <div class="rounded-xl bg-base-100 border border-base-300 p-3">
            <div
              class="flex items-center gap-1.5 text-xs text-base-content/60 mb-1"
            >
              <TrendingUp class="w-3.5 h-3.5" />
              {$_("insights.kpiSpent")}
            </div>
            <div class="text-lg font-bold text-base-content">
              {fmtMoney(data.kpis.total_spent)}
            </div>
          </div>
          <div class="rounded-xl bg-base-100 border border-base-300 p-3">
            <div
              class="flex items-center gap-1.5 text-xs text-base-content/60 mb-1"
            >
              <PiggyBank class="w-3.5 h-3.5" />
              {$_("insights.kpiRemaining")}
            </div>
            <div
              class="text-lg font-bold {data.kpis.total_remaining < 0
                ? 'text-error'
                : 'text-success'}"
            >
              {fmtMoney(data.kpis.total_remaining)}
            </div>
          </div>
          <div class="rounded-xl bg-base-100 border border-base-300 p-3">
            <div
              class="flex items-center gap-1.5 text-xs text-base-content/60 mb-1"
            >
              <AlertTriangle class="w-3.5 h-3.5" />
              {$_("insights.kpiOverspent")}
            </div>
            <div
              class="text-lg font-bold {data.kpis.overspent_count > 0
                ? 'text-error'
                : 'text-base-content'}"
            >
              {data.kpis.overspent_count}
            </div>
          </div>
        </div>

        <!-- Recommendations -->
        {#if data.recommendations?.length}
          <div
            class="mt-4 rounded-xl bg-base-100 border border-base-300 p-3"
          >
            <div
              class="flex items-center gap-1.5 text-sm font-semibold text-base-content mb-2"
            >
              <Lightbulb class="w-4 h-4 text-warning" />
              {$_("insights.recommendationsTitle")}
            </div>
            <ul class="space-y-1.5">
              {#each data.recommendations as rec}
                <li
                  class="text-sm text-base-content/80 flex items-start gap-2"
                >
                  <span class="text-warning mt-0.5 shrink-0">•</span>
                  <span>{rec}</span>
                </li>
              {/each}
            </ul>
            <p class="text-[11px] text-base-content/40 mt-2">
              {$_("insights.recommendationsDisclaimer")}
            </p>
          </div>
        {/if}

        <!-- Spend over time (trend) -->
        <div class="mt-4">
          <h4 class="text-sm font-semibold text-base-content mb-2">
            {$_("insights.trendTitle")}
          </h4>
          {#if data.trend?.length}
            <div
              class="rounded-xl bg-base-100 border border-base-300 p-3 flex items-end gap-1 h-40"
              role="img"
              aria-label={$_("insights.trendTitle")}
            >
              {#each data.trend as point}
                <div
                  class="flex-1 flex flex-col items-center justify-end h-full min-w-0"
                  title={`${point.bucket}: ${fmtMoney(point.spent)}`}
                >
                  <div
                    class="w-full bg-primary rounded-t transition-all"
                    style={`height: ${(point.spent / trendMax) * 100}%; min-height: 2px;`}
                  ></div>
                </div>
              {/each}
            </div>
            <div class="flex justify-between text-[10px] text-base-content/40 mt-1">
              <span>{data.trend[0]?.bucket}</span>
              <span>{data.trend[data.trend.length - 1]?.bucket}</span>
            </div>
          {:else}
            <p class="text-sm text-base-content/50 py-4">
              {$_("insights.noTrend")}
            </p>
          {/if}
        </div>

        <!-- Top spending categories -->
        <div class="mt-4">
          <h4 class="text-sm font-semibold text-base-content mb-2">
            {$_("insights.topCategoriesTitle")}
          </h4>
          {#if data.top_categories?.length}
            <div class="space-y-2">
              {#each data.top_categories as c}
                <div>
                  <div
                    class="flex justify-between text-xs text-base-content/70 mb-0.5"
                  >
                    <span class="truncate pr-2">{c.category}</span>
                    <span class="shrink-0 font-medium">{fmtMoney(c.spent)}</span>
                  </div>
                  <div class="h-2.5 rounded-full bg-base-300 overflow-hidden">
                    <div
                      class="h-full bg-secondary rounded-full"
                      style={`width: ${(c.spent / topCatMax) * 100}%;`}
                    ></div>
                  </div>
                </div>
              {/each}
            </div>
          {:else}
            <p class="text-sm text-base-content/50 py-4">
              {$_("insights.noCategories")}
            </p>
          {/if}
        </div>

        <!-- Spend vs budget, per budget -->
        <div class="mt-4">
          <h4 class="text-sm font-semibold text-base-content mb-2">
            {$_("insights.budgetsTitle")}
          </h4>
          {#if data.budgets?.length}
            <div class="space-y-2.5">
              {#each data.budgets as b}
                <div>
                  <div
                    class="flex justify-between text-xs text-base-content/70 mb-0.5"
                  >
                    <span class="truncate pr-2 font-medium">{b.name}</span>
                    <span class="shrink-0">
                      {fmtMoney(b.spent)} / {fmtMoney(b.budgeted)}
                      <span
                        class={b.overspent ? "text-error" : "text-base-content/50"}
                        >({fmtPct(b.pct)})</span
                      >
                    </span>
                  </div>
                  <div class="h-3 rounded-full bg-base-300 overflow-hidden">
                    <div
                      class="h-full rounded-full {b.overspent
                        ? 'bg-error'
                        : 'bg-success'}"
                      style={`width: ${Math.min(100, (b.pct ?? 0) * 100)}%;`}
                    ></div>
                  </div>
                </div>
              {/each}
            </div>
          {:else}
            <p class="text-sm text-base-content/50 py-4">
              {$_("insights.noBudgets")}
            </p>
          {/if}
        </div>
      {/if}

      <div class="flex justify-end mt-4">
        <button class="btn" onclick={() => onClose?.()}>
          {$_("insights.close")}
        </button>
      </div>
    </div>
  </div>
