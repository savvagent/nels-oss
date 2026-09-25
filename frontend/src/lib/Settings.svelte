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
