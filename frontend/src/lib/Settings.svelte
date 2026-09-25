<script>
  import { _, locale } from "svelte-i18n";
  import { Sun, Monitor, Moon, ArrowLeft } from "lucide-svelte";
  import { LOCALES, setLocale } from "./i18n/index.js";
  import { getStoredTheme, applyTheme } from "./theme.js";
  import { SIZES, getStoredTextSize, applyTextSize } from "./textsize.js";
  import {
    CATEGORIES,
    getStoredNotifPrefs,
    setNotifPref,
  } from "./notificationPrefs.js";
  import {
    hasEntitledPlan,
    planLabelKey,
    needsPaymentUpdate,
  } from "./subscriptionDisplay.js";
  import {
    PROVIDER_TERMS_URLS,
    providerLabelKey,
    statusLineKey,
    canSave,
    embeddingsNoteVisible,
    providerRetired,
  } from "./aiProviderView.js";

  let {
    fetchApi,
    subscription = null,
    billingFinalizing = false,
    onUpgrade,
    onManage,
    onBack,
  } = $props();

  let currentTheme = $state(getStoredTheme());
  let currentSize = $state(getStoredTextSize());
  let notifPrefs = $state(getStoredNotifPrefs());
  let backButtonEl = $state(null);
  let tokenStats = $state(null);
  let tokenError = $state(false);
  // AI provider card (nels-oss#3). `aiView` is the GET/PUT /user/ai-provider
  // response; `aiKey` is only ever held in memory until it is submitted.
  let aiView = $state(null);
  let aiChoice = $state("nels");
  let aiProvider = $state("");
  let aiKey = $state("");
  let aiSaving = $state(false);
  let aiError = $state("");

  // Active locale base (region stripped) for the language <select> value.
  let activeLang = $derived(($locale || "en").slice(0, 2));

  // Mounting this route IS the "opened" signal now (same convention
  // CategoriesView.svelte uses) — re-read the theme/size/notification prefs
  // from storage on mount to reflect any changes made elsewhere, and move
  // focus onto the back button for keyboard/a11y users.
  $effect(() => {
    currentTheme = getStoredTheme();
    currentSize = getStoredTextSize();
    notifPrefs = getStoredNotifPrefs();
    backButtonEl?.focus();
  });

  // Fetch the user's cumulative token usage on mount so the numbers are
  // fresh. Resets to a loading/zero state first; on failure we show an
  // unavailable message instead of the rows.
  $effect(() => {
    if (!fetchApi) return;
    tokenError = false;
    tokenStats = null;
    let cancelled = false;
    fetchApi("/user/token-stats")
      .then((s) => {
        if (!cancelled) tokenStats = s;
      })
      .catch(() => {
        if (!cancelled) tokenError = true;
      });
    return () => {
      cancelled = true;
    };
  });

  // Point the form at a freshly loaded provider view. The <select> must always
  // hold an OFFERED provider, so a retired saved provider falls back to the
  // first available one (the retired key keeps working until replaced).
  function applyAiView(view) {
    aiView = view;
    aiChoice = view?.mode === "byo" ? "own" : "nels";
    const available = view?.available_providers ?? [];
    aiProvider = available.includes(view?.provider) ? view.provider : (available[0] ?? "");
  }

  // Load the AI provider view on mount, same cancellation pattern as the
  // token-stats effect above.
  $effect(() => {
    if (!fetchApi) return;
    let cancelled = false;
    fetchApi("/user/ai-provider")
      .then((view) => {
        if (!cancelled) applyAiView(view);
      })
      .catch(() => {
        if (!cancelled) aiError = $_("aiProvider.loadError");
      });
    return () => {
      cancelled = true;
    };
  });

  async function saveAiKey() {
    aiSaving = true;
    aiError = "";
    try {
      const view = await fetchApi("/user/ai-provider", {
        method: "PUT",
        body: JSON.stringify({ provider: aiProvider, api_key: aiKey.trim() }),
      });
      applyAiView(view);
      aiKey = "";
    } catch (e) {
      aiError = $_("aiProvider.saveError", { values: { message: e?.message ?? "" } });
    } finally {
      aiSaving = false;
    }
  }

  async function removeAiKey() {
    aiSaving = true;
    aiError = "";
    try {
      await fetchApi("/user/ai-provider", { method: "DELETE" });
      applyAiView(await fetchApi("/user/ai-provider"));
      aiKey = "";
    } catch (e) {
      aiError = $_("aiProvider.saveError", { values: { message: e?.message ?? "" } });
    } finally {
      aiSaving = false;
    }
  }

  // Theme options in order: light=Sun, system=Monitor, dark=Moon.
  const THEME_OPTIONS = [
    { value: "light", key: "theme.light", Icon: Sun },
    { value: "system", key: "theme.system", Icon: Monitor },
    { value: "dark", key: "theme.dark", Icon: Moon },
  ];

  // Text-size option -> i18n key. Order follows SIZES (small, normal, large).
  const SIZE_KEYS = {
    small: "settings.sizeSmall",
    normal: "settings.sizeNormal",
    large: "settings.sizeLarge",
  };

  // Static category -> i18n key map so strings stay translatable.
  const NOTIF_KEYS = {
    limitAlerts: "settings.notifLimitAlerts",
    reminders: "settings.notifReminders",
  };
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("settings.title")}</h3>
    <button type="button" bind:this={backButtonEl} class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" /> {$_("history.back")}
    </button>
  </div>

  <div class="flex-grow overflow-y-auto">
    <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
      <!-- Appearance: language, theme, text size -->
      <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-4">
        <h4 class="font-semibold text-sm text-base-content/70 uppercase">{$_("settings.appearanceTitle")}</h4>

        <div>
          <label for="settings-language" class="label-text text-base-content/80 mb-2 block">
            {$_("settings.language")}
          </label>
          <select
            id="settings-language"
            class="select select-bordered select-sm w-full max-w-xs"
            value={activeLang}
            onchange={(e) => setLocale(e.currentTarget.value)}
          >
            {#each LOCALES as opt}
              <option value={opt.code}>{opt.label}</option>
            {/each}
          </select>
        </div>

        <div>
          <div class="label-text text-base-content/80 mb-2">
            {$_("settings.theme")}
          </div>
          <div class="join" role="group" aria-label={$_("settings.theme")}>
            {#each THEME_OPTIONS as opt}
              <button
                type="button"
                class="join-item btn btn-sm gap-1 {currentTheme === opt.value
                  ? 'btn-primary'
                  : 'btn-ghost'}"
                aria-pressed={currentTheme === opt.value}
                onclick={() => {
                  currentTheme = applyTheme(opt.value);
                }}
              >
                <opt.Icon class="w-4 h-4" />
                <span>{$_(opt.key)}</span>
              </button>
            {/each}
          </div>
        </div>

        <div>
          <div class="label-text text-base-content/80 mb-2">
            {$_("settings.textSize")}
          </div>
          <div class="join" role="group" aria-label={$_("settings.textSize")}>
            {#each SIZES as size}
              <button
                type="button"
                class="join-item btn btn-sm {currentSize === size
                  ? 'btn-primary'
                  : 'btn-ghost'}"
                aria-pressed={currentSize === size}
                onclick={() => {
                  currentSize = applyTextSize(size);
                }}
              >
                {$_(SIZE_KEYS[size])}
              </button>
            {/each}
          </div>
        </div>
      </div>

      <!-- Notifications -->
      <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-3">
        <h4 class="font-semibold text-sm text-base-content/70 uppercase">{$_("settings.notifications")}</h4>
        <div class="space-y-2">
          {#each CATEGORIES as cat}
            <div class="flex items-center justify-between gap-3">
              <label for={"notif-" + cat} class="text-sm text-base-content/80">
                {$_(NOTIF_KEYS[cat])}
              </label>
              <input
                id={"notif-" + cat}
                type="checkbox"
                class="toggle toggle-primary"
                checked={notifPrefs[cat]}
                onchange={(e) => {
                  notifPrefs = setNotifPref(cat, e.currentTarget.checked);
                }}
              />
            </div>
          {/each}
        </div>
      </div>

      <!-- Subscription / plan (#25) -->
      <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-3">
        <h4 class="font-semibold text-sm text-base-content/70 uppercase">{$_("settings.subscriptionTitle")}</h4>
        {#if hasEntitledPlan(subscription)}
          <div class="flex items-center justify-between gap-3">
            <span class="text-sm text-base-content/80">
              {$_(planLabelKey(subscription))}
            </span>
            <button class="btn btn-sm" onclick={() => onManage?.()}>
              {$_("settings.manageSubscription")}
            </button>
          </div>
          {#if needsPaymentUpdate(subscription)}
            <p class="text-xs text-warning mt-2">
              {$_("settings.paymentWarning")}
            </p>
          {/if}
          {#if subscription.cancel_at_period_end}
            <p class="text-xs text-base-content/60 mt-2">
              {$_("settings.cancelsAtPeriodEnd")}
            </p>
          {/if}
        {:else if billingFinalizing}
          <p class="text-sm text-base-content/70">{$_("settings.finalizing")}</p>
        {:else}
          <div class="flex items-center justify-between gap-3">
            <span class="text-sm text-base-content/80">{$_("settings.planFree")}</span>
            <div class="flex gap-2">
              <button class="btn btn-sm btn-primary" onclick={() => onUpgrade?.("monthly")}>
                {$_("settings.upgradeMonthly")}
              </button>
              <button class="btn btn-sm btn-primary" onclick={() => onUpgrade?.("annual")}>
                {$_("settings.upgradeAnnual")}
              </button>
            </div>
          </div>
        {/if}
      </div>

      <!-- AI provider (nels-oss#3) -->
      <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-3 lg:col-span-2">
        <h4 class="font-semibold text-sm text-base-content/70 uppercase">{$_("aiProvider.title")}</h4>
        <p class="text-sm text-base-content/80">{$_("aiProvider.intro")}</p>
        <p class="text-sm" role="status">
          {$_(statusLineKey(aiView), {
            values: {
              provider: aiView?.provider ? $_(providerLabelKey(aiView.provider)) : "",
              last4: aiView?.key_last4 ?? "",
              date: aiView?.last_verified_at
                ? new Date(aiView.last_verified_at).toLocaleDateString($locale)
                : "",
            },
          })}
        </p>
        <fieldset class="space-y-2">
          <legend class="sr-only">{$_("aiProvider.title")}</legend>
          <label class="flex items-center gap-2 text-sm">
            <input type="radio" class="radio radio-sm" name="ai-provider-choice" bind:group={aiChoice} value="nels" />
            {$_("aiProvider.optionNels")}
          </label>
          <label class="flex items-center gap-2 text-sm">
            <input
              type="radio"
              class="radio radio-sm"
              name="ai-provider-choice"
              bind:group={aiChoice}
              value="own"
              disabled={!aiView?.available_providers?.length}
            />
            {$_("aiProvider.optionOwn")}
          </label>
        </fieldset>
        {#if aiChoice === "own"}
          <div class="grid gap-2 sm:grid-cols-2">
            <label class="form-control">
              <span class="label-text text-sm">{$_("aiProvider.providerLabel")}</span>
              <select class="select select-sm select-bordered" bind:value={aiProvider}>
                {#each aiView?.available_providers ?? [] as p (p)}
                  <option value={p}>{$_(providerLabelKey(p))}</option>
                {/each}
              </select>
            </label>
            <label class="form-control">
              <span class="label-text text-sm">{$_("aiProvider.keyLabel")}</span>
              <input
                type="password"
                autocomplete="off"
                spellcheck="false"
                class="input input-sm input-bordered"
                bind:value={aiKey}
                maxlength="512"
              />
            </label>
          </div>
          <p class="text-xs text-base-content/60">{$_("aiProvider.keyHint")}</p>
          <p class="text-sm">
            {$_("aiProvider.dataUseOwn", { values: { provider: $_(providerLabelKey(aiProvider)) } })}
            {#if PROVIDER_TERMS_URLS[aiProvider]}
              <a class="link" href={PROVIDER_TERMS_URLS[aiProvider]} target="_blank" rel="noopener noreferrer">
                {$_("aiProvider.termsLink", { values: { provider: $_(providerLabelKey(aiProvider)) } })}
              </a>
            {/if}
          </p>
          <div class="flex gap-2">
            <button
              type="button"
              class="btn btn-sm btn-primary"
              disabled={aiSaving || !canSave(aiProvider, aiKey, aiView?.available_providers)}
              onclick={saveAiKey}
            >
              {aiSaving ? $_("aiProvider.saving") : $_("aiProvider.save")}
            </button>
            {#if aiView?.mode === "byo"}
              <button type="button" class="btn btn-sm btn-ghost" disabled={aiSaving} onclick={removeAiKey}>
                {$_("aiProvider.remove")}
              </button>
            {/if}
          </div>
        {:else}
          <p class="text-sm">
            {$_("aiProvider.dataUseNels")}
            <a class="link" href={PROVIDER_TERMS_URLS.gemini} target="_blank" rel="noopener noreferrer">
              {$_("aiProvider.termsLink", { values: { provider: $_("aiProvider.providerGemini") } })}
            </a>
          </p>
          {#if aiView?.mode === "byo"}
            <button type="button" class="btn btn-sm btn-ghost" disabled={aiSaving} onclick={removeAiKey}>
              {$_("aiProvider.remove")}
            </button>
          {/if}
        {/if}
        {#if providerRetired(aiView)}
          <p class="text-xs text-warning">
            {$_("aiProvider.noLongerOffered", { values: { provider: $_(providerLabelKey(aiView.provider)) } })}
          </p>
        {/if}
        {#if embeddingsNoteVisible(aiView)}
          <p class="text-xs text-base-content/70">
            {$_("aiProvider.embeddingsOff", { values: { provider: $_(providerLabelKey(aiView.provider)) } })}
          </p>
        {/if}
        <p class="text-xs text-base-content/60">
          {$_("aiProvider.dataUsePassesThrough")} {$_("aiProvider.dataUseShared")}
        </p>
        {#if aiError}<p class="text-sm text-error" role="alert">{aiError}</p>{/if}
      </div>

      <!-- Token usage -->
      <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-3 lg:col-span-2">
        <h4 class="font-semibold text-sm text-base-content/70 uppercase">{$_("settings.tokenUsage")}</h4>
        {#if tokenError}
          <p class="text-sm text-base-content/60">
            {$_("settings.tokensUnavailable")}
          </p>
        {:else}
          <div class="space-y-2">
            <div class="flex items-center justify-between gap-3">
              <span class="text-sm text-base-content/80"
                >{$_("settings.tokensInput")}</span
              >
              <span class="text-sm tabular-nums"
                >{new Intl.NumberFormat($locale).format(
                  tokenStats?.input_tokens ?? 0,
                )}</span
              >
            </div>
            <div class="flex items-center justify-between gap-3">
              <span class="text-sm text-base-content/80"
                >{$_("settings.tokensThinking")}</span
              >
              <span class="text-sm tabular-nums"
                >{new Intl.NumberFormat($locale).format(
                  tokenStats?.thinking_tokens ?? 0,
                )}</span
              >
            </div>
            <div class="flex items-center justify-between gap-3">
              <span class="text-sm text-base-content/80"
                >{$_("settings.tokensOutput")}</span
              >
              <span class="text-sm tabular-nums"
                >{new Intl.NumberFormat($locale).format(
                  tokenStats?.output_tokens ?? 0,
                )}</span
              >
            </div>
            <div
              class="flex items-center justify-between gap-3 font-medium border-t border-base-300 pt-2 mt-1"
            >
              <span class="text-sm">{$_("settings.tokensTotal")}</span>
              <span class="text-sm tabular-nums"
                >{new Intl.NumberFormat($locale).format(
                  tokenStats?.total_tokens ?? 0,
                )}</span
              >
            </div>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>
