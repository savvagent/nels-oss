<script>
  // Router-outlet view for the active budget's categories (#233, rebuilt in
  // #426). Sibling to ./TransactionsView.svelte and deliberately built from the
  // same vocabulary: self-fetches via the shared `fetchApi` helper rather than
  // receiving data as a prop, so it behaves identically whether reached via the
  // `/categories-list` slash command or a chat-detected LIST_CATEGORIES intent
  // (both just navigate here).
  //
  // #426 replaces the server-rendered `{@html}` table with grouped cards, one
  // meter per category, search and filter chips, plus a bottom-sheet editor for
  // creating, editing and deleting categories.
  //
  // NOTE: App.svelte's `:global(.cat-table …)` block still styles the
  // CATEGORY_BALANCE chat bubble's `{@html}` table. It is NOT dead code and is
  // intentionally untouched by this rebuild.
  import { _ } from "svelte-i18n";
  import {
    ArrowLeft,
    AlertTriangle,
    AlertCircle,
    CheckCircle2,
    Search,
    PiggyBank,
    RefreshCw,
    Layers,
    Minus,
    Info,
    Pencil,
    Plus,
    Trash2,
  } from "lucide-svelte";
  import { formatAmount } from "./money.js";
  import {
    meterPercent,
    healthStatus,
    groupCategories,
    searchCategories,
    filterByChip,
    chipCounts,
    showsRolloverBadge,
    showsFundBalance,
    expenseSummary,
    isDuplicateName,
    formMessageFor,
    buildCategoryPatch,
    isValidCategoriesEnvelope,
    loadErrorKey,
    MALFORMED_ENVELOPE,
    DELETE_FAILED_KEY,
  } from "./categoriesView.js";

  let { fetchApi, activeBudget, onBack } = $props();

  // `data` is the whole /categories-view envelope
  // ({categories, currency, time_frame, closed_at, permission_level}), null
  // until the first response lands — every derived value below therefore reads
  // `data?.categories`, and every helper tolerates null/undefined.
  let data = $state(null);
  let loading = $state(false);
  let error = $state("");
  // A non-fatal page-level note: the list rendered fine, but something the user
  // was in the middle of no longer applies (today: the row they had open in the
  // editor turned out to be already deleted). Distinct from `error`, which
  // REPLACES the list.
  let notice = $state("");
  // Instant search + one exclusive filter chip, both client-side over the
  // already-loaded rows (mirrors TransactionsView's P4 filtering).
  let query = $state("");
  let chip = $state("all");

  // #47 rollover only actually DOES anything when the budget-level master switch
  // is on AND the budget is not a #48 `project` budget — `category_carried`
  // returns 0 in either other case. `categories.rollover_enabled` is DEFAULT
  // TRUE while `budgets.rollover_enabled` is DEFAULT FALSE, so gating on the
  // per-category flag alone would badge EVERY category on a default budget with
  // a mechanism that does nothing. The chat CATEGORIES context already renders
  // exactly this qualifier ("Rollover: off (budget rollover disabled)" /
  // "Rollover: n/a (project budget)"); this is the same rule for the page.
  // This is only the BUDGET half of the badge rule. The CATEGORY half lives in
  // `showsRolloverBadge` / `categoryCarryKind` in `categoriesView.js`: the
  // row's own `rollover_enabled`, AND a `categoryCarryKind` that mirrors
  // `budget::category_carry_for` arm for arm. A #228 fund, a #52 mirror and any
  // non-expense row all fail `showsRolloverBadge`, because none of them takes
  // the #49 one-period carry.
  const rolloverActive = $derived(
    data?.rollover_enabled === true && data?.budget_type !== "project",
  );

  const CHIPS = [
    { key: "all", labelKey: "categories.all", icon: null },
    { key: "over", labelKey: "categories.overLimit", icon: AlertCircle },
    { key: "near", labelKey: "categories.nearLimit", icon: AlertTriangle },
    { key: "nolimit", labelKey: "categories.noLimitChip", icon: Minus },
    { key: "funds", labelKey: "categories.funds", icon: PiggyBank },
    { key: "rollover", labelKey: "categories.rollover", icon: RefreshCw },
  ];

  // Counts come from the FULL loaded set, not `visible`, so each chip always
  // reports how many rows IT would match rather than how many currently remain.
  const counts = $derived(chipCounts(data?.categories));
  // Drop the rollover chip entirely when it cannot do anything: rollover is
  // inert for this budget, or no loaded row would match it. A filter that can
  // only ever select nothing is noise, and after #474 narrowed the predicate to
  // the one arm of `category_carry_for` that actually carries, a budget whose
  // only rollover-enabled categories are funds, mirrors or non-expense rows
  // would otherwise render a permanent `Rollover 0`. `chipCounts` /
  // `filterByChip` keep
  // counting and filtering every key — only the RENDERED strip narrows, so the
  // helpers stay chip-agnostic.
  const visibleChips = $derived(
    rolloverActive && counts.rollover > 0
      ? CHIPS
      : CHIPS.filter((c) => c.key !== "rollover"),
  );
  // A budget switch can leave `chip` pointing at a chip that is no longer
  // rendered, which would silently filter the list by an invisible control.
  // Resolved as a DERIVATION rather than an $effect that writes back to `chip`:
  // an effect reading and writing the same state is a loop waiting to happen,
  // and falling back here needs no write at all.
  const activeChip = $derived(
    visibleChips.some((c) => c.key === chip) ? chip : "all",
  );

  // filter → group → render. Search narrows first so the chip counts a row only
  // once, then grouping runs over what actually survives. Filtering is by
  // `activeChip`, never the raw `chip`.
  const visible = $derived(
    filterByChip(searchCategories(data?.categories, query), activeChip),
  );
  const groups = $derived(groupCategories(visible));
  const summary = $derived(expenseSummary(data?.categories));

  // The single client-side mirror of the server's write guards: `update_category`
  // / `create_category` / `delete_category` all require owner-or-edit and refuse
  // a closed budget. Requires `data` so nothing editable paints before the first
  // envelope lands (undefined would otherwise read as "not view, not closed").
  const canEdit = $derived(
    !!data && data.permission_level !== "view" && data.closed_at == null,
  );

  const currencyIsMixed = $derived(data?.currency_is_mixed === true);
  const mixedCurrencyNote = $derived(
    currencyIsMixed
      ? $_("categories.currencyMixed", {
          values: { currency: data?.currency ?? "USD" },
        })
      : "",
  );

  const fmt = (n) => formatAmount(n, data?.currency);

  // Meter fill per health status. Color is DECORATIVE reinforcement only — the
  // same status is always spelled out with an icon and a text label beside the
  // bar, so nothing is conveyed by color alone.
  const FILL_CLASS = {
    healthy: "bg-success",
    near: "bg-warning",
    over: "bg-error",
    none: "bg-base-300",
  };
  // The status LABEL stays on `text-base-content` rather than text-success /
  // text-warning / text-error: the daisyUI accent tokens are tuned for use as
  // backgrounds and don't reliably clear 4.5:1 as small text in both the light
  // and dark themes. The icon carries the color instead (non-text, 3:1).
  const ICON_CLASS = {
    healthy: "text-success",
    near: "text-warning",
    over: "text-error",
    none: "text-base-content/50",
  };
  const statusLabel = (status) =>
    status === "over"
      ? $_("categories.statusOver")
      : status === "near"
        ? $_("categories.statusNear")
        : $_("categories.statusHealthy");

  const groupLabel = (type) =>
    type === "income"
      ? $_("categories.groupIncome")
      : type === "savings"
        ? $_("categories.groupSavings")
        : $_("categories.groupExpense");

  // Monotonic request token. NOT a budget-id comparison: `activeBudget` is
  // reassigned to a NEW object with the SAME id on every `fetchBudgets()` in
  // App.svelte (which runs after chat turns), so the $effect below re-fires and
  // starts a second concurrent `load()` for the same budget. An id-only guard
  // cannot tell two in-flight requests for one budget apart, and a slow FAILING
  // load landing after a fast SUCCEEDING one would wipe a correctly-rendered
  // page into an error screen. Only the newest request may write state.
  let reqSeq = 0;

  async function load() {
    if (!activeBudget) return;
    const seq = ++reqSeq;
    // Still captured: the id is what the URL is built from, and it is the
    // useful half of the log line when a load fails.
    const budgetId = activeBudget.id;
    loading = true;
    error = "";
    notice = "";
    try {
      const res = await fetchApi(`/budgets/${budgetId}/categories-view`);
      // Validate the envelope AT THE BOUNDARY. `parseApiResponse` yields null
      // for a 204 or an empty body and a missing `categories` coerces to `[]`
      // downstream, so an unchecked `res` renders a broken response as "this
      // budget has no categories yet".
      if (!isValidCategoriesEnvelope(res)) {
        const err = new Error("malformed categories-view response");
        err[MALFORMED_ENVELOPE] = true;
        throw err;
      }
      if (seq !== reqSeq) return;
      data = res;
    } catch (e) {
      // ALWAYS log before discarding. The old order returned on the staleness
      // guard first, so during a backend outage with the user switching budgets
      // there was zero trace of the real failure.
      console.error("categories load failed", { budgetId, seq, e });
      if (seq !== reqSeq) return;
      // `e.message` is never user-facing: fetchApi always builds
      // `new Error(errText || "API error")`, so showing it means showing the
      // raw ENGLISH server body to every locale. Map the status instead.
      error = $_("categories." + loadErrorKey(e));
      data = null;
    } finally {
      if (seq === reqSeq) loading = false;
    }
  }

  // Mounting IS the "open" signal now (the outlet's {#if route === "categories"}
  // owns mount/unmount), so fetch unconditionally at init. Re-fetch if the
  // active budget changes while this view stays mounted (e.g. a chat-driven
  // budget switch while categories are showing).
  $effect(() => {
    activeBudget;
    load();
  });

  // --- Editor (create / edit / delete) ---------------------------------------
  //
  // One native <dialog> serves both modes: the browser gives us the focus trap,
  // Escape-to-close and focus restoration to the trigger for free. `editing` is
  // null in create mode and holds the row's LAST SERVER IMAGE in edit mode — the
  // PUT diff is computed against it, so it is re-pointed at the fresh row after
  // every reload rather than mutated in place. Nothing is ever written
  // optimistically: the list only changes after a successful response is
  // followed by a re-`load()`, so a failed save can leave no stranded state.
  let editorEl = $state(null);
  let nameInputEl = $state(null);
  let mode = $state("edit"); // "edit" | "create"
  let editing = $state(null);
  let confirmingDelete = $state(false);
  let saving = $state(false);
  // Field-level messages (rendered under their input) vs. a form-level one.
  let nameError = $state("");
  let limitError = $state("");
  let formError = $state("");
  // `formLimit` is kept as a STRING so an empty box stays distinguishable from
  // 0. Empty means "don't send category_limit at all" — the server's COALESCE
  // treats a null as "unchanged", so a limit cannot be cleared once set.
  // `validateLimit` therefore REFUSES an emptied box when the category already
  // has a limit, rather than letting the save silently drop the field.
  let formName = $state("");
  let formLimit = $state("");
  let formType = $state("expense");
  let formRollover = $state(true);
  let formFund = $state(false);

  // The fund flag is expense-only server-side (a non-expense + is_fund pair 400s
  // and is unrepresentable in the DB), and `create_category` doesn't accept it
  // at all — a new fund is create-then-toggle. So the checkbox appears only when
  // editing a category that is already an expense.
  const showFund = $derived(
    mode === "edit" && editing?.category_type === "expense",
  );

  // Which hint, if any, goes under the rollover checkbox. #433: a fund
  // supersedes the #49 carry, so the checkbox is inert for a fund even when the
  // budget-level switch is ON. Two different reasons, two different hints — a
  // fund user told "rollover is off for this budget" would go turn the budget
  // switch on and still see nothing change. Derived once rather than inlined,
  // because the `aria-describedby` and the `{#if}` that renders the hint must
  // agree. `showFund` is part of the fund arm deliberately: a non-expense
  // category has no fund control and cannot become a fund through this dialog.
  //
  // PRECEDENCE: the FUND reason wins when both apply. Being a fund is intrinsic
  // to the category and permanent until the user untoggles it; the budget switch
  // is transient and one click away. Naming the transient reason while a
  // permanent one also holds is precisely the "flip the switch, see nothing
  // change" trap above — so the more durable reason is the one worth showing.
  const rolloverHintKey = $derived(
    showFund && formFund
      ? "categories.rolloverSupersededByFund"
      : !rolloverActive
        ? "categories.rolloverInactive"
        : null,
  );

  const TYPES = [
    { value: "expense", labelKey: "categories.groupExpense" },
    { value: "income", labelKey: "categories.groupIncome" },
    { value: "savings", labelKey: "categories.groupSavings" },
  ];

  function resetEditorState() {
    confirmingDelete = false;
    saving = false;
    nameError = "";
    limitError = "";
    formError = "";
  }

  function openCreate() {
    if (!canEdit) return;
    // Whatever the last editor session ended up saying about a vanished row no
    // longer applies once a new one is opened.
    notice = "";
    mode = "create";
    editing = null;
    formName = "";
    formLimit = "";
    formType = "expense";
    formRollover = true;
    formFund = false;
    resetEditorState();
    editorEl?.showModal();
    nameInputEl?.focus();
  }

  function openEdit(cat) {
    if (!canEdit) return;
    notice = "";
    mode = "edit";
    editing = cat;
    formName = cat.name ?? "";
    // Prefill from the raw `category_limit`, never `effective_limit` — the
    // latter folds in the #49 carry, so writing it back would bake the carry
    // into the base limit.
    formLimit = cat.category_limit == null ? "" : String(cat.category_limit);
    formType = cat.category_type ?? "expense";
    formRollover = !!cat.rollover_enabled;
    formFund = !!cat.is_fund;
    resetEditorState();
    // A mirror row is never given an edit affordance, but if one is somehow
    // reached, say why instead of letting the server 409 read as a generic
    // failure.
    if (cat.is_mirror) formError = $_("categories.mirrorNotEditable");
    editorEl?.showModal();
    nameInputEl?.focus();
  }

  function closeEditor() {
    editorEl?.close();
  }

  // Validation runs on blur (and again on submit), so a bad value is called out
  // while the user is still on the field rather than only after they commit.
  function validateName() {
    nameError = formName.trim() ? "" : $_("categories.nameRequired");
    return !nameError;
  }

  function validateLimit() {
    const raw = formLimit.trim();
    if (raw === "") {
      // The server CANNOT clear a limit: `update_category` does
      // `category_limit = COALESCE($3, category_limit)`, so a null reads as
      // "unchanged". Silently dropping the field would close the sheet as
      // though it saved — worse, a simultaneous rename would land while the
      // cleared limit did not. So refuse the empty box in edit mode on a
      // category that already HAS a limit, and say what to do instead. In
      // create mode `editing` is null, where empty legitimately means
      // "no limit".
      limitError =
        editing?.category_limit != null
          ? $_("categories.limitNotClearable")
          : "";
      return !limitError;
    }
    const n = Number(raw);
    limitError =
      Number.isFinite(n) && n >= 0 ? "" : $_("categories.limitInvalid");
    return !limitError;
  }

  // `isDuplicateName` / `formMessageFor` now live in ./categoriesView.js as
  // pure functions returning KEY strings — this repo has no component-test
  // harness, and their precedence order is exactly the sort of thing that needs
  // pinning. Translation stays here.
  const formMessage = (e, fallbackKey) =>
    $_("categories." + formMessageFor(e, fallbackKey));

  // Re-point `editing` at the row as the server now has it, so the next PUT
  // diffs against truth rather than against a pre-failure image.
  function resyncEditing() {
    if (!editing) return;
    const fresh = (data?.categories ?? []).find((c) => c.id === editing.id);
    if (fresh) {
      editing = fresh;
      return;
    }
    // The row is GONE. Reachable when a DELETE succeeded server-side but its
    // response was lost: the catch showed an error, the reload found nothing,
    // and leaving `editing` pointed at a deleted category means the next Save
    // PUTs a dead id, 404s, and strands the user with no correct action. Close
    // the sheet and say what happened, on the page where it stays visible —
    // `onclose` clears every in-sheet message.
    editing = null;
    closeEditor();
    notice = $_("categories.categoryGone");
  }

  async function save() {
    if (saving) return;
    // Both validators always run — `&&` would short-circuit and leave a bad
    // limit uncalled-out whenever the name is also empty.
    const okName = validateName();
    const okLimit = validateLimit();
    if (!okName || !okLimit) return;

    const budgetId = activeBudget?.id;
    if (!budgetId) {
      // Reachable if the active budget is cleared while the sheet is open. A
      // bare `return` here meant pressing Save did literally nothing — no
      // spinner, no close, no message.
      console.error("category save skipped: no active budget");
      formError = $_("commands.noActiveBudget");
      return;
    }

    formError = "";
    saving = true;
    try {
      const limit = formLimit.trim() === "" ? null : Number(formLimit);
      if (mode === "create") {
        // No `is_fund` here: POST /categories doesn't accept it.
        await fetchApi(`/budgets/${budgetId}/categories`, {
          method: "POST",
          body: JSON.stringify({
            name: formName.trim(),
            category_type: formType,
            category_limit: limit,
            rollover_enabled: formRollover,
          }),
        });
      } else if (editing) {
        // Send only what actually changed — see buildCategoryPatch for the four
        // rules it encodes and the tests that pin them.
        const patch = buildCategoryPatch(
          {
            name: formName,
            limit: formLimit,
            rollover: formRollover,
            fund: formFund,
          },
          editing,
          { showFund },
        );
        if (Object.keys(patch).length === 0) {
          closeEditor();
          return;
        }
        await fetchApi(`/budgets/${budgetId}/categories/${editing.id}`, {
          method: "PUT",
          body: JSON.stringify(patch),
        });
      }
      closeEditor();
      await load();
    } catch (e) {
      console.error("category save failed", e);
      if (isDuplicateName(e)) {
        nameError = $_("categories.nameTaken");
      } else {
        formError = formMessage(e);
      }
      // Re-read so the list behind the sheet shows server state, never a
      // half-applied edit, then diff future saves against that same state.
      await load();
      resyncEditing();
    } finally {
      saving = false;
    }
  }

  async function removeCategory() {
    if (saving || !editing) return;
    const budgetId = activeBudget?.id;
    if (!budgetId) {
      // Same silent no-op as `save()` had: the Delete button did nothing at all.
      console.error("category delete skipped: no active budget");
      confirmingDelete = false;
      formError = $_("commands.noActiveBudget");
      return;
    }

    formError = "";
    saving = true;
    try {
      await fetchApi(`/budgets/${budgetId}/categories/${editing.id}`, {
        method: "DELETE",
      });
      closeEditor();
      await load();
    } catch (e) {
      console.error("category delete failed", e);
      confirmingDelete = false;
      // The DELETE-specific fallback: reusing `saveFailed` reported "Couldn't
      // save that change" for an operation the user never asked for.
      // On a 404 the row is already gone, so skip the in-sheet copy entirely:
      // `resyncEditing` below closes the sheet (wiping `formError` via onclose)
      // and surfaces the page-level `categoryGone` notice instead, which is the
      // more accurate, longer-lived message. Computing the copy here would be
      // dead work on exactly the path that matters (#494).
      if (e?.status !== 404) {
        formError = formMessage(e, DELETE_FAILED_KEY);
      }
      await load();
      resyncEditing();
    } finally {
      saving = false;
    }
  }
</script>

<!-- One category card. `editable` drives the tap-to-edit affordance only; the
     read-path markup below it is identical either way.

     WHY an absolutely-positioned overlay button rather than wrapping the whole
     card in a <button>: a `button` has presentational children, so an ancestor
     button would strip the meter's `role="progressbar"` and its aria-label from
     the accessibility tree. Keeping the hit target as a SIBLING preserves every
     read-path semantic while still making the entire card tappable. The content
     layer paints above the button and passes pointer events through to it. -->
{#snippet categoryCard(cat, editable)}
  {@const status = healthStatus(cat.spent, cat.effective_limit)}
  {@const pct = Math.round(meterPercent(cat.spent, cat.effective_limit))}
  {@const remaining = (cat.effective_limit ?? 0) - (cat.spent ?? 0)}
  {@const StatusIcon = status === "over" ? AlertCircle : status === "near" ? AlertTriangle : CheckCircle2}
  <div class="relative rounded-xl border border-base-300 bg-base-100 p-3">
    {#if editable}
      <button
        type="button"
        class="cat-row-hit absolute inset-0 rounded-xl"
        aria-label={`${$_("categories.edit")}: ${cat.name}`}
        onclick={() => openEdit(cat)}
      ></button>
    {/if}
    <div class={editable ? "relative pointer-events-none" : ""}>
      <div class="flex items-baseline justify-between gap-3">
        <div class="min-w-0 flex-1 font-medium truncate text-base-content">
          {cat.name}
        </div>
        {#if cat.effective_limit != null}
          <span class="shrink-0 text-sm font-mono tabular-nums text-base-content">
            {$_("categories.spentOf", {
              values: { spent: fmt(cat.spent), limit: fmt(cat.effective_limit) },
            })}
          </span>
        {:else}
          <span class="shrink-0 text-sm font-mono tabular-nums text-base-content">
            {fmt(cat.spent)}
          </span>
        {/if}
        {#if editable}
          <!-- Purely a visual hint that the card is tappable; the overlay button
               above already carries the accessible name. -->
          <Pencil class="w-4 h-4 shrink-0 text-base-content/60" aria-hidden="true" />
        {/if}
      </div>

      <!-- `showsRolloverBadge` owns the rollover-badge VISIBILITY rule: the
           budget-level master switch from #47 / PR #427 AND, via
           `categoryCarryKind`, every shape `budget::category_carry_for` gives a
           zero #49 carry — a #228 fund, a #52 mirror, any non-expense row —
           plus the row's own `rollover_enabled`. The
           wrapper below and the badge itself call that ONE helper, so they
           cannot drift by construction. It does not own the whole block — the
           carried-amount line has its own independent `{#if cat.carried_amount}`
           nested inside the badge, and the fund balance has `showsFundBalance`. -->
      {#if cat.is_fund || showsRolloverBadge(cat, rolloverActive) || cat.is_mirror}
        <div class="flex flex-wrap items-center gap-1.5 mt-1.5">
          {#if cat.is_fund}
            <span class="badge badge-sm badge-outline gap-1">
              <PiggyBank class="w-3 h-3" aria-hidden="true" />
              {$_("categories.fundBadge")}
            </span>
            <!-- The Fund BADGE follows the `is_fund` flag, which is simply
                 true. The BALANCE follows `showsFundBalance`, because a fund
                 that is also a #52 mirror keeps a fund_balance column that
                 `category_carry_for` never returns for it — printing it here
                 would present a number nothing carries. -->
            {#if showsFundBalance(cat)}
              <span class="text-xs text-base-content/70">
                {$_("categories.fundBalance", {
                  values: { amount: fmt(cat.fund_balance) },
                })}
              </span>
            {/if}
          {/if}
          {#if showsRolloverBadge(cat, rolloverActive)}
            <span class="badge badge-sm badge-outline gap-1">
              <RefreshCw class="w-3 h-3" aria-hidden="true" />
              {$_("categories.rolloverBadge")}
            </span>
            {#if cat.carried_amount}
              <span class="text-xs text-base-content/70">
                {$_("categories.carriedAmount", {
                  values: { amount: fmt(cat.carried_amount) },
                })}
              </span>
            {/if}
          {/if}
          {#if cat.is_mirror}
            <span class="badge badge-sm badge-outline gap-1">
              <Layers class="w-3 h-3" aria-hidden="true" />
              {$_("categories.rollupBadge")}
            </span>
            {#if cat.linked_budget_name}
              <span class="text-xs text-base-content/70">
                {$_("categories.rollupSource", {
                  values: { name: cat.linked_budget_name },
                })}
              </span>
            {/if}
          {/if}
        </div>
      {/if}

      {#if cat.effective_limit == null}
        <!-- Tracking-only category: no denominator, so no meter. -->
        <p class="mt-2 text-xs text-base-content/60">
          {$_("categories.noLimit")}
        </p>
      {:else}
        <div
          class="mt-2 h-2 w-full rounded-full bg-base-300 overflow-hidden"
          role="progressbar"
          aria-valuenow={pct}
          aria-valuemin="0"
          aria-valuemax="100"
          aria-label={$_("categories.meterLabel", {
            values: {
              name: cat.name,
              spent: fmt(cat.spent),
              limit: fmt(cat.effective_limit),
            },
          })}
        >
          <div class={`meter-fill h-full rounded-full ${FILL_CLASS[status]}`} style={`width:${pct}%`}></div>
        </div>
        <!-- Status is always icon + words, never color alone. -->
        <div class="flex items-center justify-between gap-2 mt-1.5">
          <span class="inline-flex items-center gap-1 text-xs font-medium text-base-content">
            <StatusIcon class={`w-3.5 h-3.5 shrink-0 ${ICON_CLASS[status]}`} aria-hidden="true" />
            {statusLabel(status)}
          </span>
          <span class="text-xs font-mono tabular-nums text-base-content">
            {remaining < 0
              ? $_("categories.overBy", { values: { amount: fmt(-remaining) } })
              : $_("categories.remaining", { values: { amount: fmt(remaining) } })}
          </span>
        </div>
      {/if}

      {#if canEdit && cat.is_mirror}
        <!-- The one place `mirrorNotEditable` surfaces on the read path: the row
             deliberately has no edit affordance, so say why rather than leave it
             looking like an oversight. -->
        <p class="mt-2 text-xs text-base-content/60">
          {$_("categories.mirrorNotEditable")}
        </p>
      {/if}
    </div>
  </div>
{/snippet}

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("categories.title")}</h3>
    <div class="flex items-center gap-2">
      {#if canEdit}
        <button
          type="button"
          class="btn btn-sm btn-primary gap-1.5 min-h-11"
          onclick={openCreate}
        >
          <Plus class="w-4 h-4" aria-hidden="true" />
          {$_("categories.addCategory")}
        </button>
      {/if}
      <button
        type="button"
        class="btn btn-sm btn-ghost gap-1.5 min-h-11"
        onclick={() => onBack?.()}
      >
        <ArrowLeft class="w-4 h-4" aria-hidden="true" />
        {$_("categories.back")}
      </button>
    </div>
  </div>

  <!-- Why the write controls are absent, stated rather than left to be guessed.
       Mirrors the server: view permission and a closed budget both reject every
       category write. -->
  {#if data && data.permission_level === "view"}
    <div role="note" class="alert alert-info mb-3 py-2">
      <Info class="w-4 h-4 shrink-0" aria-hidden="true" />
      <span class="text-sm">{$_("categories.readOnly")}</span>
    </div>
  {:else if data && data.closed_at != null}
    <div role="note" class="alert alert-info mb-3 py-2">
      <Info class="w-4 h-4 shrink-0" aria-hidden="true" />
      <span class="text-sm">{$_("categories.closedBudget")}</span>
    </div>
  {/if}

  <!-- Non-fatal: the list below is current, but something the user had open no
       longer exists. Rendered outside the loading/error chain so it survives
       the reload that discovered it. -->
  {#if notice}
    <div role="status" class="alert alert-warning mb-3 py-2">
      <AlertTriangle class="w-4 h-4 shrink-0" aria-hidden="true" />
      <span class="text-sm">{notice}</span>
    </div>
  {/if}

  {#if currencyIsMixed}
    <div role="status" class="alert alert-info mb-3 py-2">
      <Info class="w-4 h-4 shrink-0" aria-hidden="true" />
      <span class="text-sm">{mixedCurrencyNote}</span>
    </div>
  {/if}

  {#if loading}
    <div
      class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
    >
      <span class="loading loading-spinner loading-lg text-primary"></span>
      <span class="text-sm">{$_("categories.loading")}</span>
    </div>
  {:else if error}
    <div class="alert alert-error">
      <AlertTriangle class="w-5 h-5" aria-hidden="true" />
      <span>{error}</span>
    </div>
  {:else if !activeBudget}
    <p class="text-sm text-base-content/50 py-4">
      {$_("commands.noActiveBudget")}
    </p>
  {:else if !data || (data.categories ?? []).length === 0}
    <div class="flex flex-col items-start gap-3 py-4">
      <p class="text-sm text-base-content/50">
        {$_("commands.categoriesNone", { values: { name: activeBudget.name } })}
      </p>
      {#if canEdit}
        <button
          type="button"
          class="btn btn-primary btn-sm gap-1.5 min-h-11"
          onclick={openCreate}
        >
          <Plus class="w-4 h-4" aria-hidden="true" />
          {$_("categories.emptyCta")}
        </button>
      {/if}
    </div>
  {:else}
    <!-- Expense rollup for the whole period. Income and savings are excluded by
         `expenseSummary` because "remaining" isn't meaningful for them. -->
    {#if summary.totalLimit > 0 || summary.totalSpent > 0}
      {@const sStatus = healthStatus(summary.totalSpent, summary.totalLimit > 0 ? summary.totalLimit : null)}
      {@const sPct = Math.round(meterPercent(summary.totalSpent, summary.totalLimit))}
      {@const SIcon = sStatus === "over" ? AlertCircle : sStatus === "near" ? AlertTriangle : CheckCircle2}
      <div class="rounded-xl border border-base-300 bg-base-100 p-3 mb-3">
        <div class="flex items-baseline justify-between gap-2">
          <h4 class="text-sm font-semibold text-base-content">
            {$_("categories.summaryTitle")}
          </h4>
          <!-- The summary header carried the same "of $0.00" defect as the
               group headers when every expense row is tracking-only; the guard
               below only ever gated the METER. -->
          <span class="text-sm font-mono tabular-nums text-base-content">
            {#if summary.totalLimit > 0}
              {$_("categories.spentOf", {
                values: { spent: fmt(summary.totalSpent), limit: fmt(summary.totalLimit) },
              })}
            {:else}
              {fmt(summary.totalSpent)}
            {/if}
          </span>
        </div>
        {#if summary.totalLimit > 0}
          <div
            class="mt-2 h-2 w-full rounded-full bg-base-300 overflow-hidden"
            role="progressbar"
            aria-valuenow={sPct}
            aria-valuemin="0"
            aria-valuemax="100"
            aria-label={$_("categories.meterLabel", {
              values: {
                name: $_("categories.summaryTitle"),
                spent: fmt(summary.totalSpent),
                limit: fmt(summary.totalLimit),
              },
            })}
          >
            <div class={`meter-fill h-full rounded-full ${FILL_CLASS[sStatus]}`} style={`width:${sPct}%`}></div>
          </div>
          <div class="flex items-center justify-between gap-2 mt-1.5">
            <span class="inline-flex items-center gap-1 text-xs font-medium text-base-content">
              <SIcon class={`w-3.5 h-3.5 shrink-0 ${ICON_CLASS[sStatus]}`} aria-hidden="true" />
              {statusLabel(sStatus)}
            </span>
            <span class="text-xs font-mono tabular-nums text-base-content">
              {summary.remaining < 0
                ? $_("categories.overBy", { values: { amount: fmt(-summary.remaining) } })
                : $_("categories.remaining", { values: { amount: fmt(summary.remaining) } })}
            </span>
          </div>
        {:else}
          <p class="mt-1.5 text-xs text-base-content/60">{$_("categories.noLimit")}</p>
        {/if}
      </div>
    {/if}

    <!-- Instant search + a scrollable chip strip, both client-side over the
         already-loaded rows. Always rendered when the budget has categories so
         search stays reachable after a chip empties the visible set. -->
    <div class="flex flex-col gap-2 mb-2">
      <label class="input input-sm input-bordered flex items-center gap-2 min-h-11">
        <Search class="w-4 h-4 opacity-60" aria-hidden="true" />
        <input
          type="text"
          class="grow"
          placeholder={$_("categories.searchPlaceholder")}
          bind:value={query}
          aria-label={$_("categories.searchPlaceholder")}
        />
      </label>
      <!-- One non-wrapping row that scrolls inside its own container, so a long
           chip set never forces the page body to scroll horizontally. -->
      <div
        class="flex items-center gap-2 overflow-x-auto pb-1 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
      >
        {#each visibleChips as c (c.key)}
          <button
            type="button"
            class="btn btn-xs rounded-full shrink-0 gap-1 px-3 min-h-11"
            class:btn-neutral={activeChip === c.key}
            class:btn-ghost={activeChip !== c.key}
            aria-pressed={activeChip === c.key}
            onclick={() => (chip = c.key)}
          >
            {#if c.icon}
              {@const Icon = c.icon}
              <Icon class="w-3 h-3" aria-hidden="true" />
            {/if}
            {$_(c.labelKey)}
            <span class="opacity-70">{counts[c.key]}</span>
          </button>
        {/each}
      </div>
    </div>

    {#if groups.length === 0}
      <!-- Zero-result state: search/chips matched nothing — distinct from the
           "budget has no categories yet" empty state above. -->
      <p class="text-sm text-base-content/50 py-4">
        {$_("categories.noneFound")}
      </p>
    {:else}
      <div class="flex flex-col gap-3 overflow-y-auto">
        {#each groups as g (g.type)}
          <div class="flex flex-col gap-2">
            <div
              class="sticky top-0 z-10 flex items-center justify-between gap-2 bg-base-200/95 backdrop-blur px-1 py-1 rounded"
            >
              <span class="text-xs font-semibold text-base-content/70">
                {groupLabel(g.type)}
              </span>
              <!-- Same guard the page summary applies: a group whose rows are
                   all tracking-only totals to a limit of 0, and "$120.00 of
                   $0.00" reads as infinitely overspent rather than as "no
                   limit". -->
              <span class="text-xs font-semibold font-mono tabular-nums text-base-content/70">
                {#if g.totalLimit > 0}
                  {$_("categories.spentOf", {
                    values: { spent: fmt(g.totalSpent), limit: fmt(g.totalLimit) },
                  })}
                {:else}
                  {fmt(g.totalSpent)} · {$_("categories.noLimit")}
                {/if}
              </span>
            </div>

            {#each g.rows as cat (cat.id)}
              {@render categoryCard(cat, canEdit && !cat.is_mirror)}
            {/each}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<!-- Category editor (#426): ONE native <dialog> serves create, edit and the
     delete confirmation. daisyUI's `modal-bottom sm:modal-middle` reads as a
     bottom sheet on phones and a centered dialog from `sm` up. Native <dialog>
     is what gives us the focus trap, Escape-to-close and focus restoration to
     the row that opened it — none of that is hand-rolled here. `onclose` fires
     for every route out, including Escape and the backdrop, so it is the single
     place transient editor state is cleared. -->
<dialog
  bind:this={editorEl}
  class="modal modal-bottom sm:modal-middle"
  aria-labelledby="cat-editor-title"
  onclose={resetEditorState}
>
  <div class="modal-box">
    {#if confirmingDelete}
      <h3 id="cat-editor-title" class="font-semibold text-lg mb-2">
        {$_("categories.confirmDeleteTitle")}
      </h3>
      <p class="text-sm text-base-content/70">
        {$_("categories.confirmDeleteBody", {
          values: { name: editing?.name ?? "" },
        })}
      </p>
      <div class="modal-action gap-2">
        <button
          type="button"
          class="btn btn-ghost min-h-11"
          disabled={saving}
          onclick={() => (confirmingDelete = false)}
        >
          {$_("categories.cancel")}
        </button>
        <button
          type="button"
          class="btn btn-error min-h-11 gap-1.5"
          disabled={saving}
          onclick={removeCategory}
        >
          {#if saving}
            <span class="loading loading-spinner loading-xs"></span>
          {:else}
            <Trash2 class="w-4 h-4" aria-hidden="true" />
          {/if}
          {saving ? $_("categories.saving") : $_("categories.delete")}
        </button>
      </div>
    {:else}
      <h3 id="cat-editor-title" class="font-semibold text-lg mb-3">
        {mode === "create"
          ? $_("categories.addTitle")
          : $_("categories.editTitle")}
      </h3>

      <!-- Form-level failures land INSIDE the sheet: the main view's error alert
           paints behind the backdrop where it would be invisible. -->
      {#if formError}
        <div role="alert" class="alert alert-error mb-3 py-2">
          <AlertTriangle class="w-4 h-4 shrink-0" aria-hidden="true" />
          <span class="text-sm">{formError}</span>
        </div>
      {/if}

      <form
        class="flex flex-col gap-3"
        onsubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <div class="form-control">
          <label class="label py-1" for="cat-editor-name">
            <span class="label-text text-base-content/80">
              {$_("categories.nameLabel")}
            </span>
          </label>
          <input
            bind:this={nameInputEl}
            id="cat-editor-name"
            type="text"
            autocomplete="off"
            class="input input-bordered w-full min-h-11"
            class:input-error={!!nameError}
            bind:value={formName}
            onblur={validateName}
            aria-invalid={nameError ? "true" : undefined}
            aria-describedby={nameError ? "cat-editor-name-error" : undefined}
          />
          {#if nameError}
            <!-- Error text stays on `text-base-content` for the same reason the
                 read path's status labels do: the daisyUI accent tokens are
                 tuned as backgrounds and don't reliably clear 4.5:1 as small
                 text in both themes. The icon carries the color. -->
            <span
              id="cat-editor-name-error"
              class="inline-flex items-center gap-1 text-xs text-base-content mt-1"
            >
              <AlertCircle class="w-3.5 h-3.5 shrink-0 text-error" aria-hidden="true" />
              {nameError}
            </span>
          {/if}
        </div>

        {#if mode === "create"}
          <!-- Type is create-only: changing an existing category's type would
               also have to reconcile the fund flag, and nothing in this UI
               needs it. -->
          <div class="form-control">
            <label class="label py-1" for="cat-editor-type">
              <span class="label-text text-base-content/80">
                {$_("categories.typeLabel")}
              </span>
            </label>
            <select
              id="cat-editor-type"
              class="select select-bordered w-full min-h-11"
              bind:value={formType}
            >
              {#each TYPES as t (t.value)}
                <option value={t.value}>{$_(t.labelKey)}</option>
              {/each}
            </select>
          </div>
        {/if}

        <div class="form-control">
          <label class="label py-1" for="cat-editor-limit">
            <span class="label-text text-base-content/80">
              {$_("categories.limitLabel")}
            </span>
          </label>
          <!-- `type="text"` with a decimal inputmode, NOT `type="number"`: a
               number input both coerces the binding away from a string (so an
               empty box stops being distinguishable from 0) and silently drops
               non-numeric text, which would make `limitInvalid` unreachable.
               The keypad is what mobile users actually need here, and
               `validateLimit` does the rest. -->
          <input
            id="cat-editor-limit"
            type="text"
            inputmode="decimal"
            autocomplete="off"
            class="input input-bordered w-full min-h-11"
            class:input-error={!!limitError}
            bind:value={formLimit}
            onblur={validateLimit}
            aria-invalid={limitError ? "true" : undefined}
            aria-describedby={limitError ? "cat-editor-limit-error" : undefined}
          />
          {#if limitError}
            <span
              id="cat-editor-limit-error"
              class="inline-flex items-center gap-1 text-xs text-base-content mt-1"
            >
              <AlertCircle class="w-3.5 h-3.5 shrink-0 text-error" aria-hidden="true" />
              {limitError}
            </span>
          {/if}
        </div>

        <!-- The checkbox stays FUNCTIONAL when rollover is inert for this budget
             — the flag is still persisted and will take effect the moment the
             budget-level switch is turned on — but say so rather than let it
             look like it does something now. #433 adds a SECOND inert case that
             turning the budget switch on does NOT fix: on a fund the #228
             balance supersedes the #49 carry, so the flag persists and still
             never takes effect. `rolloverHintKey` picks which of the two to
             say. -->
        <div class="form-control">
          <label
            class="label cursor-pointer justify-start gap-3 min-h-11 py-2"
            for="cat-editor-rollover"
          >
            <input
              id="cat-editor-rollover"
              type="checkbox"
              class="checkbox checkbox-primary"
              bind:checked={formRollover}
              aria-describedby={rolloverHintKey
                ? "cat-editor-rollover-hint"
                : undefined}
            />
            <span class="label-text text-base-content/80">
              {$_("categories.rolloverLabel")}
            </span>
          </label>
          {#if rolloverHintKey}
            <p
              id="cat-editor-rollover-hint"
              class="inline-flex items-center gap-1 text-xs text-base-content/70"
            >
              <Info class="w-3.5 h-3.5 shrink-0" aria-hidden="true" />
              {$_(rolloverHintKey)}
            </p>
          {/if}
        </div>

        {#if showFund}
          <label
            class="label cursor-pointer justify-start gap-3 min-h-11 py-2"
            for="cat-editor-fund"
          >
            <input
              id="cat-editor-fund"
              type="checkbox"
              class="checkbox checkbox-primary"
              bind:checked={formFund}
            />
            <span class="label-text text-base-content/80 inline-flex items-center gap-1.5">
              <PiggyBank class="w-4 h-4 shrink-0" aria-hidden="true" />
              {$_("categories.fundLabel")}
            </span>
          </label>
        {/if}

        <div class="modal-action flex-wrap gap-2">
          {#if mode === "edit"}
            <button
              type="button"
              class="btn btn-ghost min-h-11 gap-1.5 mr-auto"
              disabled={saving}
              onclick={() => (confirmingDelete = true)}
            >
              <Trash2 class="w-4 h-4 text-error" aria-hidden="true" />
              {$_("categories.delete")}
            </button>
          {/if}
          <button
            type="button"
            class="btn btn-ghost min-h-11"
            disabled={saving}
            onclick={closeEditor}
          >
            {$_("categories.cancel")}
          </button>
          <button type="submit" class="btn btn-primary min-h-11 gap-1.5" disabled={saving}>
            {#if saving}
              <span class="loading loading-spinner loading-xs"></span>
            {/if}
            {saving ? $_("categories.saving") : $_("categories.save")}
          </button>
        </div>
      </form>
    {/if}
  </div>
  <form method="dialog" class="modal-backdrop">
    <button aria-label={$_("categories.cancel")}>close</button>
  </form>
</dialog>

<style>
  /* Meter growth is animated, but never for users who asked for less motion. */
  .meter-fill {
    transition: width 200ms ease-out;
  }

  /* The invisible full-card hit target for tap-to-edit. Tint and focus ring are
     both derived from `currentColor` (the inherited base-content token) so they
     track the daisyUI light and dark themes instead of pinning a hex. */
  .cat-row-hit {
    background-color: transparent;
    transition: background-color 180ms ease-out;
  }
  .cat-row-hit:hover {
    background-color: color-mix(in oklab, currentColor 8%, transparent);
  }
  .cat-row-hit:focus-visible {
    outline: 2px solid currentColor;
    outline-offset: 2px;
  }

  @media (prefers-reduced-motion: reduce) {
    .meter-fill,
    .cat-row-hit {
      transition: none;
    }
  }
</style>
