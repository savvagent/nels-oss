<script>
  // Router-outlet view for the active budget's transactions (#376). Sibling to
  // ./CategoriesView.svelte: self-fetches via the shared `fetchApi` helper and
  // behaves identically whether reached via the `/transactions-list` slash
  // command or a chat-detected LIST_TRANSACTIONS intent (both just navigate
  // here). Scope is REASSIGN ONLY — a per-row category dropdown that PUTs the
  // new category; amount/description/date are intentionally not editable here.
  import { _ } from "svelte-i18n";
  import {
    ArrowLeft,
    AlertTriangle,
    Sparkles,
    Link2,
    Ban,
    Clock,
    Check,
    Copy,
    Search,
    Tag,
  } from "lucide-svelte";
  import {
    assignableCategories,
    formatAmount,
    amountTone,
    provenanceKind,
    needsReview,
    markReviewed,
    possibleDuplicate,
    resolveMatch,
    findTwin,
    groupTransactions,
    applyTransactionFilters,
    filterCounts,
    splitAccountLabel,
  } from "./transactionsView.js";

  // Map provenance → daisyUI accent classes for the left rail + avatar tile
  // (#403 P5). Keeps the mockup's "indigo=Nels / teal=bank / neutral=you" idea
  // on theme tokens so light/dark still work.
  const railClass = (tx) =>
    provenanceKind(tx) === "ai"
      ? "border-l-primary"
      : provenanceKind(tx) === "imported"
        ? "border-l-secondary"
        : "border-l-base-300";
  const tileClass = (tx) =>
    provenanceKind(tx) === "ai"
      ? "bg-primary/15 text-primary"
      : provenanceKind(tx) === "imported"
        ? "bg-secondary/15 text-secondary"
        : "bg-base-200 text-base-content/60";

  let { fetchApi, activeBudget, onBack } = $props();

  // One expanded detail panel at a time (#403 P5).
  let openId = $state(null);
  const toggleDetails = (id) => (openId = openId === id ? null : id);

  let transactions = $state([]);
  let categories = $state([]);
  let loading = $state(false);
  let error = $state("");
  // Instant search + filter chips (#403 P4): all client-side over the
  // already-loaded rows (no new list query), composing with each other.
  // `showNeedsReviewOnly` shipped in P2; Pending + Uncategorized are P4.
  let search = $state("");
  let showNeedsReviewOnly = $state(false);
  let showPendingOnly = $state(false);
  let showUncategorizedOnly = $state(false);

  // The rows actually rendered, after search + the active chips (#403 P4):
  // filter → group → render. Each stage is identity when inactive.
  const visibleTransactions = $derived(
    applyTransactionFilters(transactions, {
      query: search,
      needsReviewOnly: showNeedsReviewOnly,
      pendingOnly: showPendingOnly,
      uncategorizedOnly: showUncategorizedOnly,
    }),
  );

  // Relative-date groups with per-group running totals (#403 P4). Grouped from
  // the already-filtered rows so search/chips narrow BEFORE grouping.
  const groups = $derived(groupTransactions(visibleTransactions));

  // Ids of Nels rows currently displayed inside a duplicate stitch (#403 P5) —
  // hidden from their normal date-group position so the pair reads together.
  const stitchedTwinIds = $derived(
    new Set(
      (visibleTransactions || [])
        .filter(possibleDuplicate)
        .map((tx) => tx.matched_transaction_id)
        .filter(Boolean),
    ),
  );

  // Live per-chip counts + alert-strip source (#403 P5), over the FULL loaded
  // set (not `visibleTransactions`) so a chip always shows how many rows IT
  // would match, not how many remain under the currently active filters.
  const counts = $derived(filterCounts(transactions));
  const anyActiveFilter = $derived(
    showNeedsReviewOnly || showPendingOnly || showUncategorizedOnly || search.trim() !== "",
  );
  function clearFilters() {
    showNeedsReviewOnly = false;
    showPendingOnly = false;
    showUncategorizedOnly = false;
    search = "";
  }

  // Currency-aware amount (#403): render each row in its own `currency`
  // (Nels-logged rows are NULL → USD fallback via formatAmount). Income (an
  // income-type category) is shown positive/success-toned with a leading `+`;
  // expenses are muted; savings/uncategorized are neutral. Tone keys off the
  // category TYPE, never the amount's sign (amounts are unsigned magnitudes).
  const displayAmount = (tx) => {
    const s = formatAmount(tx.amount, tx.currency);
    return amountTone(tx.category_type) === "income" ? `+${s}` : s;
  };
  const amountClass = (tx) => {
    const tone = amountTone(tx.category_type);
    const base =
      tone === "income"
        ? "text-success font-medium"
        : tone === "expense"
          ? "text-base-content/70"
          : "text-base-content";
    // The row itself already dims (class:opacity-60) when excluded, so the
    // amount only needs the strike-through to read as "not counted".
    return tx.excluded_from_budget ? `${base} line-through` : base;
  };

  const fmtDate = (d) => {
    try {
      return new Date(d).toLocaleDateString();
    } catch {
      return "";
    }
  };

  async function load() {
    if (!activeBudget) return;
    // Capture the budget id being fetched FOR so a stale response from a
    // previous budget can't overwrite state after a switch (mirrors
    // CategoriesView's race guard).
    const budgetId = activeBudget.id;
    loading = true;
    error = "";
    try {
      const [txs, cats] = await Promise.all([
        fetchApi(`/budgets/${budgetId}/transactions`),
        fetchApi(`/budgets/${budgetId}/categories`),
      ]);
      if (activeBudget?.id !== budgetId) return;
      transactions = txs ?? [];
      categories = assignableCategories(cats ?? []);
    } catch (e) {
      if (activeBudget?.id !== budgetId) return;
      console.error("transactions load failed", e);
      error = e?.message || $_("transactions.loadError");
      transactions = [];
    } finally {
      if (activeBudget?.id === budgetId) loading = false;
    }
  }

  async function reassign(tx, categoryId) {
    if (!categoryId || categoryId === tx.category_id) return;
    error = "";
    try {
      await fetchApi(`/budgets/${activeBudget.id}/transactions/${tx.id}`, {
        method: "PUT",
        body: JSON.stringify({ category_id: categoryId }),
      });
      // Reload so an emptied auto-created category disappears from the dropdown
      // options (the backend cleanup deletes it) and the row shows its new home.
      await load();
    } catch (e) {
      console.error("transaction reassign failed", e);
      error = e?.message || $_("transactions.reassignError");
      // Reload so the <select> snaps back to the persisted category rather than
      // showing the unsaved pick after a failed PUT.
      await load();
    }
  }

  // Approve a needs-review row (#403 P2): POST the approve endpoint, then
  // optimistically flip the row to `reviewed` so its indicator + de-emphasis
  // clear without a full reload. On failure, surface the error and reload to
  // re-sync the row's true state (mirrors `reassign`'s failure handling).
  async function approve(tx) {
    if (!needsReview(tx)) return;
    error = "";
    try {
      await fetchApi(
        `/budgets/${activeBudget.id}/transactions/${tx.id}/approve`,
        { method: "POST" },
      );
      transactions = markReviewed(transactions, tx.id);
    } catch (e) {
      console.error("transaction approve failed", e);
      error = e?.message || $_("transactions.approveError");
      await load();
    }
  }

  // Resolve a possible duplicate (#403 P3): POST the resolve-match endpoint with
  // the chosen action, then optimistically apply the same non-destructive change
  // the server made (both actions clear the banner; merge also strikes the
  // excluded Nels twin). On failure, surface the error and reload to re-sync
  // (mirrors `approve`'s failure handling). Nothing is ever deleted.
  async function resolveDuplicate(tx, action) {
    if (!possibleDuplicate(tx)) return;
    error = "";
    try {
      await fetchApi(
        `/budgets/${activeBudget.id}/transactions/${tx.id}/resolve-match`,
        { method: "POST", body: JSON.stringify({ action }) },
      );
      transactions = resolveMatch(transactions, tx.id, action);
    } catch (e) {
      console.error("transaction resolve-match failed", e);
      error = e?.message || $_("transactions.resolveError");
      await load();
    }
  }

  $effect(() => {
    activeBudget;
    load();
  });
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("transactions.title")}</h3>
    <button
      type="button"
      class="btn btn-sm btn-ghost gap-1.5"
      onclick={() => onBack?.()}
    >
      <ArrowLeft class="w-4 h-4" />
      {$_("transactions.back")}
    </button>
  </div>

  {#if loading}
    <div
      class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
    >
      <span class="loading loading-spinner loading-lg text-primary"></span>
      <span class="text-sm">{$_("transactions.loading")}</span>
    </div>
  {:else if error}
    <div class="alert alert-error">
      <AlertTriangle class="w-5 h-5" />
      <span>{error}</span>
    </div>
  {:else if !activeBudget}
    <p class="text-sm text-base-content/50 py-4">
      {$_("commands.noActiveBudget")}
    </p>
  {:else if transactions.length === 0}
    <p class="text-sm text-base-content/50 py-4">{$_("transactions.empty")}</p>
  {:else}
    <!-- Instant search + sticky filter chips (#403 P4): all client-side over the
         already-loaded rows, composing with each other and with grouping
         (filter → group → render). Always shown when there are rows so search
         stays available even after a chip empties the visible set. -->
    <div class="flex flex-col gap-2 mb-2">
      <label class="input input-sm input-bordered flex items-center gap-2">
        <Search class="w-4 h-4 opacity-60" />
        <input
          type="text"
          class="grow"
          placeholder={$_("transactions.search")}
          bind:value={search}
          aria-label={$_("transactions.search")}
        />
      </label>
      <!-- Compact scrollable chip row (#403 P5): a single non-wrapping row
           (horizontal scroll on narrow viewports) so it never pushes the list
           down. "All" resets every filter + search; each chip carries its live
           count from `counts` (full-set, not the already-filtered view). -->
      <div class="flex items-center gap-2 overflow-x-auto pb-1 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        <button type="button"
          class="btn btn-xs rounded-full shrink-0"
          class:btn-neutral={!anyActiveFilter}
          class:btn-ghost={anyActiveFilter}
          aria-pressed={!anyActiveFilter}
          onclick={clearFilters}>
          {$_("transactions.all")} <span class="opacity-70">{counts.all}</span>
        </button>
        <!-- Needs Review chip (#403 P2): keeps its live count of pending rows. -->
        <button type="button"
          class="btn btn-xs rounded-full shrink-0 gap-1"
          class:btn-warning={showNeedsReviewOnly}
          class:btn-ghost={!showNeedsReviewOnly}
          aria-pressed={showNeedsReviewOnly}
          onclick={() => (showNeedsReviewOnly = !showNeedsReviewOnly)}>
          <Clock class="w-3 h-3" />{$_("transactions.needsReview")} <span class="opacity-70">{counts.needsReview}</span>
        </button>
        <!-- Pending chip (#403 P4): the data model has no distinct
             uncleared/hold signal, so Pending is scoped to the only
             not-yet-settled state — unapproved bank-synced rows — and thus
             currently coincides with Needs Review (see `isPending`). Included
             because the epic AC enumerates it; it diverges once a provider-level
             pending flag is modeled (out of scope). -->
        <button type="button"
          class="btn btn-xs rounded-full shrink-0 gap-1"
          class:btn-warning={showPendingOnly}
          class:btn-ghost={!showPendingOnly}
          aria-pressed={showPendingOnly}
          onclick={() => (showPendingOnly = !showPendingOnly)}>
          <Clock class="w-3 h-3" />{$_("transactions.pending")} <span class="opacity-70">{counts.pending}</span>
        </button>
        <!-- Uncategorized chip (#403 P4): rows with no category assigned. -->
        <button type="button"
          class="btn btn-xs rounded-full shrink-0 gap-1"
          class:btn-warning={showUncategorizedOnly}
          class:btn-ghost={!showUncategorizedOnly}
          aria-pressed={showUncategorizedOnly}
          onclick={() => (showUncategorizedOnly = !showUncategorizedOnly)}>
          <Tag class="w-3 h-3" />{$_("transactions.uncategorized")} <span class="opacity-70">{counts.uncategorized}</span>
        </button>
      </div>
    </div>

    {#if counts.needsReview > 0 || counts.duplicates > 0}
      <!-- Review/duplicate alert strip (#403 P5): a persistent nudge summarizing
           what the chips would otherwise hide until clicked. Clicking it jumps
           straight into the Needs Review filter (duplicates surface inline per
           row via the possible-duplicate banner below, not a separate filter). -->
      <button type="button"
        class="alert alert-warning py-2 mb-2 text-sm text-left w-full"
        onclick={() => (showNeedsReviewOnly = true)}>
        <AlertTriangle class="w-4 h-4 shrink-0" />
        <span>
          {$_("transactions.reviewSummary", { values: { count: counts.needsReview } })}{#if counts.duplicates > 0}
            &nbsp;·&nbsp;{$_("transactions.duplicateSummary", { values: { count: counts.duplicates } })}{/if}
        </span>
      </button>
    {/if}

    {#if groups.length === 0}
      <!-- Zero-result state (#403 P4): search/chips matched nothing — distinct
           from the "no transactions yet" empty state above. -->
      <p class="text-sm text-base-content/50 py-4">
        {$_("transactions.noMatches")}
      </p>
    {:else}
      <div class="flex flex-col gap-3 overflow-y-auto">
        {#each groups as g (g.key)}
          <div class="flex flex-col gap-2">
            <!-- Relative-date group header with the per-group running total
                 (#403 P4), e.g. "Yesterday · $42.18". The total sums only
                 budget-affecting (non-excluded) rows. -->
            <div
              class="sticky top-0 z-10 flex items-center justify-between gap-2 bg-base-200/95 backdrop-blur px-1 py-1 rounded"
            >
              <span class="text-xs font-semibold text-base-content/70">
                {g.key === "today"
                  ? $_("transactions.today")
                  : g.key === "yesterday"
                    ? $_("transactions.yesterday")
                    : g.key === "thisWeek"
                      ? $_("transactions.thisWeek")
                      : g.label}
              </span>
              <span class="text-xs font-semibold text-base-content/60">
                {formatAmount(g.total, g.currency)}
              </span>
            </div>
            {#each g.rows as tx (tx.id)}
              {#if !stitchedTwinIds.has(tx.id)}
                {#if possibleDuplicate(tx)}
                  <!-- Stitched duplicate pair (#403 P5): bracket the imported
                       row together with its Nels-logged twin (found via
                       matched_transaction_id) so the pair reads as one
                       decision instead of two separate rows. Replaces the
                       old in-row banner (Task 7) — the twin is suppressed
                       from its normal standalone slot via stitchedTwinIds. -->
                  {@const twin = findTwin(transactions, tx)}
                  <div class="rounded-xl border border-warning/50 bg-warning/5 overflow-hidden">
                    <div class="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold text-warning">
                      <Copy class="w-3.5 h-3.5" />
                      {$_("transactions.duplicateStitchHeader")}
                    </div>
                    {#if twin}
                      <!-- The Nels-logged twin, rendered as a compact row -->
                      <div class="flex items-center gap-3 px-3 py-2 border-t border-warning/30">
                        <span class="shrink-0 w-7 h-7 rounded-lg grid place-items-center bg-primary/15 text-primary" aria-hidden="true"><Sparkles class="w-3.5 h-3.5" /></span>
                        <div class="min-w-0 flex-1 truncate text-sm">{twin.description}</div>
                        <div class="font-mono tabular-nums text-sm">{displayAmount(twin)}</div>
                      </div>
                    {/if}
                    <!-- The imported row itself -->
                    <div class="flex items-center gap-3 px-3 py-2 border-t border-warning/30">
                      <span class="shrink-0 w-7 h-7 rounded-lg grid place-items-center bg-secondary/15 text-secondary" aria-hidden="true"><Link2 class="w-3.5 h-3.5" /></span>
                      <div class="min-w-0 flex-1">
                        <div class="truncate text-sm">{tx.description}</div>
                        {#if tx.account_label}<div class="text-xs text-base-content/60 truncate">{tx.account_label}</div>{/if}
                      </div>
                      <div class="font-mono tabular-nums text-sm">{displayAmount(tx)}</div>
                    </div>
                    <div class="flex items-center gap-2 px-3 py-2 border-t border-warning/30">
                      <button type="button" class="btn btn-xs btn-warning gap-1" onclick={() => resolveDuplicate(tx, "merge")}>
                        {$_("transactions.mergeKeepImported")}
                      </button>
                      <button type="button" class="btn btn-xs btn-ghost" onclick={() => resolveDuplicate(tx, "dismiss")}>
                        {$_("transactions.keepBoth")}
                      </button>
                    </div>
                  </div>
                {:else}
                  <div
                    class={`rounded-xl bg-base-100 border border-base-300 border-l-4 ${railClass(tx)} p-3`}
                    class:opacity-60={tx.excluded_from_budget}
                    class:opacity-70={needsReview(tx) && !tx.excluded_from_budget}
                  >
                    <div class="flex items-center gap-3">
                      <!-- Source-tinted avatar tile (decorative) -->
                      <span class={`shrink-0 w-8 h-8 rounded-lg grid place-items-center ${tileClass(tx)}`} aria-hidden="true">
                        {#if provenanceKind(tx) === "ai"}
                          <Sparkles class="w-4 h-4" />
                        {:else if provenanceKind(tx) === "imported"}
                          <Link2 class="w-4 h-4" />
                        {:else}
                          <Tag class="w-4 h-4" />
                        {/if}
                      </span>

                      <div class="min-w-0 flex-1">
                        <div class="font-medium truncate">{tx.description}</div>
                        <!-- Provenance + state sub-line (unchanged badges) -->
                        <div class="flex flex-wrap items-center gap-1.5 mt-0.5">
                          <!-- Provenance indicator (#403), driven by the real `source` field. -->
                          {#if provenanceKind(tx) === "ai"}
                            <span
                              class="badge badge-sm badge-primary badge-outline gap-1"
                              title={$_("transactions.loggedByNels")}
                            >
                              <Sparkles class="w-3 h-3" />
                              {$_("transactions.loggedByNels")}
                            </span>
                          {:else if provenanceKind(tx) === "imported"}
                            <!-- #425: the badge is nowrap with no max-width, so as a
                                 flex item it refused to shrink and overflowed the card
                                 on phones. min-w-0 + max-w-full let it shrink and wrap
                                 its siblings; only the name span ellipsizes, while the
                                 icon and the ••last4 mask stay pinned with shrink-0. -->
                            {@const acct = splitAccountLabel(tx.account_label)}
                            <span
                              class="badge badge-sm badge-secondary badge-outline gap-1 min-w-0 max-w-full sm:max-w-[30ch]"
                              title={tx.account_label ?? $_("transactions.linkedAccount")}
                            >
                              <Link2 class="w-3 h-3 shrink-0" aria-hidden="true" />
                              <span class="sr-only">{$_("transactions.detailAccount")}: </span>
                              <span class="truncate min-w-0">{acct.name ?? $_("transactions.linkedAccount")}</span>
                              {#if acct.mask}<span class="shrink-0 tabular-nums">••{acct.mask}</span>{/if}
                            </span>
                          {/if}
                          {#if needsReview(tx)}
                            <!-- Needs-Review indicator (#403 P2): imported rows awaiting
                                 the user's approval. Paired with the inline Approve action. -->
                            <span
                              class="badge badge-sm badge-warning badge-outline gap-1"
                              title={$_("transactions.needsReview")}
                            >
                              <Clock class="w-3 h-3" />
                              {$_("transactions.needsReview")}
                            </span>
                          {/if}
                          {#if tx.excluded_from_budget}
                            <span class="badge badge-sm badge-ghost gap-1">
                              <Ban class="w-3 h-3" />
                              {$_("transactions.excluded")}
                            </span>
                          {/if}
                        </div>
                      </div>

                      <!-- Ledger amount: right-aligned, tabular monospace -->
                      <div class="shrink-0 text-right">
                        <div class={`font-mono tabular-nums ${amountClass(tx)}`}>{displayAmount(tx)}</div>
                        <div class="text-xs text-base-content/50">{fmtDate(tx.transaction_date)}</div>
                      </div>
                    </div>

                    <!-- Row controls (approve + category select) move onto their own line -->
                    <div class="flex items-center gap-2 flex-wrap mt-2 pl-11">
                      {#if needsReview(tx)}
                        <!-- Inline approve (#403 P2): full-contrast so it stays reachable
                             even though the row itself is de-emphasized as pending. -->
                        <button
                          type="button"
                          class="btn btn-sm btn-success gap-1"
                          onclick={() => approve(tx)}
                        >
                          <Check class="w-3.5 h-3.5" />
                          {$_("transactions.approve")}
                        </button>
                      {/if}
                      <button type="button" class="btn btn-ghost btn-xs gap-1" onclick={() => toggleDetails(tx.id)}
                        aria-expanded={openId === tx.id}>
                        <Search class="w-3.5 h-3.5" />
                        {openId === tx.id ? $_("transactions.hideDetails") : $_("transactions.showDetails")}
                      </button>
                      <label class="flex items-center gap-1.5 text-xs">
                        <span class="text-base-content/60"
                          >{$_("transactions.category")}</span
                        >
                        <select
                          class="select select-sm select-bordered"
                          value={tx.category_id ?? ""}
                          onchange={(e) => reassign(tx, e.currentTarget.value)}
                        >
                          <!-- Uncategorized transaction (its category was deleted →
                               category_id NULL): show a disabled placeholder so the browser
                               doesn't silently display the first real option as if selected. -->
                          {#if !tx.category_id}
                            <option value="" disabled
                              >{$_("transactions.uncategorized")}</option
                            >
                          {/if}
                          <!-- If the current category isn't assignable (e.g. income or a
                               mirror), still show it as the current selection so the row
                               isn't blank; it just can't be re-selected. -->
                          {#if tx.category_id && !categories.some((c) => c.id === tx.category_id)}
                            <option value={tx.category_id}>{tx.category_name ?? "—"}</option>
                          {/if}
                          {#each categories as c (c.id)}
                            <option value={c.id}>{c.name}</option>
                          {/each}
                        </select>
                      </label>
                    </div>

                    <!-- Inline detail panel (#403 P5): full untruncated description,
                         account, exact date/time, amount, category, source, and the
                         bank reference id when present. Scoped to this normal-row
                         branch only — the stitched duplicate card above renders its
                         own compact summaries for both twins and isn't a per-tx row
                         with its own controls line. -->
                    {#if openId === tx.id}
                      <dl class="mt-2 ml-11 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs bg-base-200/60 rounded-lg p-3">
                        <dt class="text-base-content/60">{$_("transactions.detailRawDescription")}</dt>
                        <dd class="break-words">{tx.description}</dd>
                        {#if tx.account_label}
                          <dt class="text-base-content/60">{$_("transactions.detailAccount")}</dt>
                          <dd>{tx.account_label}</dd>
                        {/if}
                        <dt class="text-base-content/60">{$_("transactions.detailDate")}</dt>
                        <dd>{new Date(tx.transaction_date).toLocaleString()}</dd>
                        <dt class="text-base-content/60">{$_("transactions.detailAmount")}</dt>
                        <dd class="font-mono tabular-nums">{formatAmount(tx.amount, tx.currency)}</dd>
                        <dt class="text-base-content/60">{$_("transactions.detailCategory")}</dt>
                        <dd>{tx.category_name ?? $_("transactions.uncategorized")}</dd>
                        <dt class="text-base-content/60">{$_("transactions.detailSource")}</dt>
                        <dd>{provenanceKind(tx) === "ai" ? $_("transactions.loggedByNels") : provenanceKind(tx) === "imported" ? (tx.account_label ?? $_("transactions.linkedAccount")) : "—"}</dd>
                        {#if tx.provider_transaction_id}
                          <dt class="text-base-content/60">{$_("transactions.detailProviderId")}</dt>
                          <dd class="font-mono break-all">{tx.provider_transaction_id}</dd>
                        {/if}
                      </dl>
                    {/if}

                  </div>
                {/if}
              {/if}
            {/each}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>
