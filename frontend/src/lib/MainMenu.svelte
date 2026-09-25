<script>
  import {
    History as HistoryIcon,
    Tags,
    Receipt,
    Wallet,
    BarChart3,
    Landmark,
    Copy,
    Check,
    AlertTriangle,
    Download,
    Settings as SettingsIcon,
    LogOut,
    Trash2,
    RefreshCw,
    PiggyBank,
  } from "lucide-svelte";
  import { _ } from "svelte-i18n";
  import { pwa } from "./pwa.svelte.js";

  // Build identity, injected at build time (see vite.config.js) — ported
  // verbatim from Sidebar.svelte's own footer, which this replaces.
  const appVersion = __APP_VERSION__;
  const buildSha = __BUILD_SHA__;
  // Public AGPL-3.0 source. AGPL §13 requires offering network users the
  // source, so the menu footer links here from every screen.
  const SOURCE_URL = "https://github.com/savvagent/nels-oss";

  let {
    open = false,
    user = null,
    token = "",
    retirementPlannerEnabled = false,
    onClose,
    onNavigate, // (routeName: "history"|"categories"|"insights"|"accounts"|"retirement"|"settings") => void — NOT used for Budgets, see onOpenBudgets
    onOpenBudgets, // distinct callback: "budgetsList" isn't in the flat ROUTES array (#241 registered it as a separate static hash segment), so it can't go through onNavigate/navigate() like the others
    onExportData,
    onDeleteAccount,
    onLogout,
  } = $props();

  let copiedPasscode = $state(false);
  let copyFailed = $state(false);
  let panelEl = $state(null);

  $effect(() => {
    if (open) panelEl?.querySelector("button")?.focus();
  });

  // Manual focus trap: the top bar (hamburger, "+ New chat", notification
  // bell) sits in a sibling DOM subtree that `inert` does NOT cover (only
  // <main> is made inert while the menu is open — see App.svelte), so
  // without this handler Tab/Shift+Tab would escape the popover into those
  // controls. Cycle within this panel's own buttons instead.
  function trapFocus(e) {
    if (e.key !== "Tab") return;
    const rows = Array.from(panelEl?.querySelectorAll("button, a[href]") ?? []);
    if (rows.length === 0) return;
    const first = rows[0];
    const last = rows[rows.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  // Ported from Sidebar.svelte's own copyPasscode, but awaited/caught here:
  // a denied clipboard permission or an insecure (non-HTTPS) context rejects
  // navigator.clipboard.writeText, and the unawaited original always flipped
  // to "Copied!" regardless of outcome. This component has no error-surface
  // prop wired in from App.svelte (unlike onLogout/onDeleteAccount, which are
  // followed by a visible screen transition on success), so a rejection
  // flips a local `copyFailed` flag instead and the row renders its own
  // inline failure state — self-contained, no new cross-cutting prop.
  async function copyPasscode() {
    try {
      await navigator.clipboard.writeText(token);
      copyFailed = false;
      copiedPasscode = true;
      setTimeout(() => (copiedPasscode = false), 2000);
    } catch {
      copiedPasscode = false;
      copyFailed = true;
      setTimeout(() => (copyFailed = false), 3000);
    }
  }

  function navigateAndClose(route) {
    onNavigate?.(route);
    onClose?.();
  }
</script>

{#if open}
  <div
    bind:this={panelEl}
    id="main-menu"
    role="dialog"
    aria-modal="true"
    aria-label={$_("menu.panelAria")}
    tabindex="-1"
    class="fixed top-[52px] left-2 z-50 w-72 md:w-64 bg-base-200 border border-base-300 rounded-xl shadow-2xl p-0"
    onkeydown={trapFocus}
  >
    {#if user}
      <!-- Identity header: which account is signed in, ported from
           Sidebar.svelte's own footer trigger (avatar initial + email) —
           there's otherwise no surface left in the app that shows this. Not
           one of the 10 action rows below, just a display. -->
      <div class="flex items-center gap-2 px-4 py-3 border-b border-base-300 rounded-t-xl">
        <span class="w-7 h-7 rounded-full bg-gradient-to-tr from-primary to-secondary flex items-center justify-center font-bold text-primary-content text-xs shrink-0">{user.email[0].toUpperCase()}</span>
        <span class="text-xs text-base-content/80 truncate">{user.email}</span>
      </div>
    {/if}
    <ul class="menu p-0">
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none {user ? '' : 'rounded-t-xl'}"
          onclick={() => navigateAndClose("history")}
        >
          <HistoryIcon class="w-4 h-4 shrink-0" /> <span>{$_("menu.history")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("categories")}
        >
          <Tags class="w-4 h-4 shrink-0" /> <span>{$_("menu.categories")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("transactions")}
        >
          <Receipt class="w-4 h-4 shrink-0" /> <span>{$_("menu.transactions")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => { onOpenBudgets?.(); onClose?.(); }}
        >
          <Wallet class="w-4 h-4 shrink-0" /> <span>{$_("budgetsList.openLabel")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("insights")}
        >
          <BarChart3 class="w-4 h-4 shrink-0" /> <span>{$_("insights.openLabel")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("accounts")}
        >
          <Landmark class="w-4 h-4 shrink-0" /> <span>{$_("accounts.openLabel")}</span>
        </button>
      </li>
      {#if retirementPlannerEnabled || user?.retirement_planner_enabled === true}
        <li>
          <button
            type="button"
            class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
            onclick={() => navigateAndClose("retirement")}
          >
            <PiggyBank class="w-4 h-4 shrink-0" /> <span>{$_("retirement.openLabel")}</span>
          </button>
        </li>
      {/if}
      <li><hr class="border-base-300 mx-0" /></li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm text-primary hover:bg-base-100 rounded-none"
          onclick={copyPasscode}
        >
          {#if copiedPasscode}
            <Check class="w-4 h-4 text-success shrink-0" />
            <span class="text-success" aria-live="polite">{$_("sidebar.copied")}</span>
          {:else if copyFailed}
            <AlertTriangle class="w-4 h-4 text-error shrink-0" />
            <span class="text-error" aria-live="polite">{$_("sidebar.copyFailed")}</span>
          {:else}
            <Copy class="w-4 h-4 shrink-0" />
            <span>{$_("sidebar.copyPasscode")}</span>
          {/if}
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => { onExportData?.(); onClose?.(); }}
        >
          <Download class="w-4 h-4 shrink-0" /> <span>{$_("account.exportData")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("settings")}
        >
          <SettingsIcon class="w-4 h-4 shrink-0" /> <span>{$_("settings.openLabel")}</span>
        </button>
      </li>
      <li><hr class="border-base-300 mx-0" /></li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm text-error hover:bg-base-100 rounded-none"
          onclick={() => { onLogout?.(); onClose?.(); }}
        >
          <LogOut class="w-4 h-4 shrink-0" /> <span>{$_("sidebar.signOut")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm text-error hover:bg-base-100 rounded-none {pwa.updateAvailable ? '' : 'rounded-b-xl'}"
          onclick={() => { onDeleteAccount?.(); onClose?.(); }}
        >
          <Trash2 class="w-4 h-4 shrink-0" /> <span>{$_("account.deleteAccount")}</span>
        </button>
      </li>
    </ul>

    <!-- Version + in-app update, ported verbatim from Sidebar.svelte's own
         footer (not part of the spec's 9-item list, but dropping it would
         silently remove the only surface that lets a user pick up a
         waiting service-worker update — see pwa.svelte.js). -->
    <div class="border-t border-base-300 px-3 py-2 flex items-center justify-between gap-2 rounded-b-xl">
      <span class="text-xs text-base-content/40 select-text">
        v{appVersion} · {buildSha} ·
        <a href={SOURCE_URL} target="_blank" rel="noopener" class="link link-hover text-base-content/70">{$_("menu.sourceCode")}</a>
      </span>
      {#if pwa.updateAvailable}
        <button type="button" onclick={() => pwa.applyUpdate()} class="btn btn-xs btn-primary gap-1 normal-case">
          <RefreshCw class="w-3 h-3" />
          {$_("sidebar.updateAvailable")}
        </button>
      {/if}
    </div>
  </div>
{/if}
