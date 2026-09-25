<script>
  // In-app notification feed (#55): a topbar bell with an unread-count badge and
  // a dropdown panel listing the user's notifications (newest first), with mark
  // read (single + all) and dismiss. Reuses the existing /api/notifications
  // surface (#45) and honors the #54 per-category preferences client-side: the
  // visible list AND the badge count are filtered by enabled categories, so the
  // badge never disagrees with what the user can see.
  import { onMount, onDestroy } from "svelte";
  import { Bell, Check, CheckCheck, X, Trash2 } from "lucide-svelte";
  import { _, locale } from "svelte-i18n";
  import { get } from "svelte/store";
  import {
    filterByPrefs,
    STORAGE_KEY,
    PREFS_CHANGED_EVENT,
  } from "./notificationPrefs.js";
  import { setAppBadge } from "./pwa.svelte.js";

  // `fetchApi` is the app's authenticated fetch helper (handles base URL, token,
  // 401 -> logout, JSON/204). Passing it in keeps auth handling in one place.
  let { fetchApi, budgets = [] } = $props();

  let open = $state(false);
  let notifications = $state([]); // raw rows from the server (newest first)
  // #54 prefs live in localStorage. `prefsTick` is a reactive nonce we bump to
  // force re-reading the stored prefs (on open, on storage event) so the feed
  // reacts to a Settings change in the same session. filterByPrefs reads the
  // stored prefs by default, so we don't hold a snapshot.
  let prefsTick = $state(0);
  let panelEl = $state(null);
  let bellEl = $state(null);
  let pollTimer = null;

  // Prefs-filtered, newest-first view used for BOTH the list and the counts.
  // Depend on prefsTick so a prefs change re-runs the filter.
  let visible = $derived((prefsTick, filterByPrefs(notifications)));
  let unreadVisible = $derived(visible.filter((n) => !n.is_read));
  let unreadCount = $derived(unreadVisible.length);

  // Keep the PWA app-icon badge in sync with the prefs-filtered unread count.
  $effect(() => {
    setAppBadge(unreadCount);
  });

  async function load() {
    if (!fetchApi) return;
    try {
      const rows = await fetchApi("/notifications");
      notifications = Array.isArray(rows) ? rows : [];
    } catch {
      // Non-fatal: an empty feed is acceptable (mirrors conversation loading).
    }
  }

  // Public refresh hook so the parent can re-poll after a notable action.
  export function refresh() {
    load();
  }

  async function markRead(n) {
    if (n.is_read) return;
    // Optimistic: flip locally, then persist.
    notifications = notifications.map((x) =>
      x.id === n.id ? { ...x, is_read: true } : x,
    );
    try {
      await fetchApi(`/notifications/${n.id}/read`, { method: "POST" });
    } catch {
      load(); // reconcile on failure
    }
  }

  async function markAllRead() {
    if (unreadCount === 0) return;
    const ids = new Set(unreadVisible.map((n) => n.id));
    notifications = notifications.map((x) =>
      ids.has(x.id) ? { ...x, is_read: true } : x,
    );
    try {
      await fetchApi("/notifications/read_all", { method: "POST" });
    } catch {
      load();
    }
  }

  async function dismiss(n) {
    notifications = notifications.filter((x) => x.id !== n.id);
    try {
      await fetchApi(`/notifications/${n.id}`, { method: "DELETE" });
    } catch {
      load();
    }
  }

  function budgetName(budgetId) {
    if (!budgetId) return null;
    const b = budgets.find((x) => x.id === budgetId);
    return b ? b.name : null;
  }

  function relativeTime(iso) {
    const then = new Date(iso).getTime();
    const mins = Math.floor((Date.now() - then) / 60000);
    if (mins < 1) return $_("notifications.justNow");
    if (mins < 60) return `${mins}m`;
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return `${hrs}h`;
    const days = Math.floor(hrs / 24);
    if (days < 7) return `${days}d`;
    return new Date(iso).toLocaleDateString(get(locale) || "en", {
      month: "short",
      day: "numeric",
    });
  }

  function toggle() {
    open = !open;
    if (open) {
      prefsTick++; // re-read prefs from storage on open
      load();
    }
  }

  function close() {
    open = false;
  }

  // Close on outside click / Escape while open.
  function onWindowClick(e) {
    if (!open) return;
    if (panelEl?.contains(e.target) || bellEl?.contains(e.target)) return;
    close();
  }
  function onWindowKeydown(e) {
    if (e.key === "Escape" && open) close();
  }

  // Re-read prefs when ANOTHER tab changes them (native `storage` event).
  function onStorage(e) {
    if (e.key === STORAGE_KEY) prefsTick++;
  }
  // Re-read prefs when the Settings panel changes them in THIS tab (the
  // `storage` event does not fire same-tab), so the badge updates immediately.
  function onPrefsChanged() {
    prefsTick++;
  }

  onMount(() => {
    load();
    pollTimer = setInterval(load, 60000);
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("storage", onStorage);
    window.addEventListener(PREFS_CHANGED_EVENT, onPrefsChanged);
  });

  function onVisibility() {
    if (document.visibilityState === "visible") load();
  }

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("storage", onStorage);
    window.removeEventListener(PREFS_CHANGED_EVENT, onPrefsChanged);
  });
</script>

<svelte:window onclick={onWindowClick} onkeydown={onWindowKeydown} />

<div class="relative">
  <button
    bind:this={bellEl}
    type="button"
    onclick={toggle}
    aria-haspopup="true"
    aria-expanded={open}
    aria-label={unreadCount > 0
      ? $_("notifications.bellAriaUnread", { values: { count: unreadCount } })
      : $_("notifications.bellAria")}
    class="relative p-1.5 rounded-lg text-base-content/70 hover:text-base-content hover:bg-base-100 transition-colors"
  >
    <Bell class="w-5 h-5" />
    {#if unreadCount > 0}
      <span
        class="absolute -top-0.5 -right-0.5 min-w-[1.05rem] h-[1.05rem] px-1
               rounded-full bg-error text-error-content text-[10px] font-bold
               leading-none flex items-center justify-center"
        aria-hidden="true"
      >
        {unreadCount > 99 ? "99+" : unreadCount}
      </span>
    {/if}
  </button>

  {#if open}
    <div
      bind:this={panelEl}
      role="dialog"
      aria-label={$_("notifications.title")}
      class="absolute right-0 top-full mt-2 z-50 w-80 max-w-[calc(100vw-1.5rem)]
             rounded-xl bg-base-200 border border-base-300 shadow-2xl
             flex flex-col max-h-[70vh]"
    >
      <!-- Header -->
      <div
        class="shrink-0 flex items-center justify-between gap-2 px-3 py-2.5 border-b border-base-300"
      >
        <span class="font-semibold text-sm text-base-content">
          {$_("notifications.title")}
        </span>
        <div class="flex items-center gap-1">
          <button
            type="button"
            onclick={markAllRead}
            disabled={unreadCount === 0}
            class="btn btn-ghost btn-xs gap-1 normal-case disabled:opacity-40"
            title={$_("notifications.markAllRead")}
          >
            <CheckCheck class="w-3.5 h-3.5" />
            <span class="hidden sm:inline">{$_("notifications.markAllRead")}</span>
          </button>
          <button
            type="button"
            onclick={close}
            aria-label={$_("notifications.close")}
            class="btn btn-ghost btn-xs btn-circle"
          >
            <X class="w-4 h-4" />
          </button>
        </div>
      </div>

      <!-- List -->
      <div class="overflow-y-auto flex-grow">
        {#if visible.length === 0}
          <div
            class="flex flex-col items-center justify-center text-center py-10 px-4 gap-2"
          >
            <Bell class="w-8 h-8 text-base-content/25" />
            <span class="text-xs text-base-content/50"
              >{$_("notifications.empty")}</span
            >
          </div>
        {:else}
          <ul class="divide-y divide-base-300">
            {#each visible as n (n.id)}
              <li
                class="flex items-start gap-2 px-3 py-2.5 {n.is_read
                  ? 'opacity-60'
                  : 'bg-base-100/40'}"
              >
                <!-- Unread dot -->
                <span
                  class="mt-1.5 shrink-0 w-2 h-2 rounded-full {n.is_read
                    ? 'bg-transparent'
                    : 'bg-primary'}"
                  aria-hidden="true"
                ></span>
                <div class="flex-grow min-w-0">
                  <p
                    class="text-xs leading-snug text-base-content/90 whitespace-pre-line break-words"
                  >
                    {n.message}
                  </p>
                  <div
                    class="flex items-center gap-2 mt-1 text-[10px] text-base-content/45"
                  >
                    <span>{relativeTime(n.created_at)}</span>
                    {#if budgetName(n.budget_id)}
                      <span class="truncate"
                        >· {$_("notifications.contextBudget", {
                          values: { name: budgetName(n.budget_id) },
                        })}</span
                      >
                    {/if}
                  </div>
                </div>
                <div class="flex items-center gap-0.5 shrink-0">
                  {#if !n.is_read}
                    <button
                      type="button"
                      onclick={() => markRead(n)}
                      aria-label={$_("notifications.markRead")}
                      title={$_("notifications.markRead")}
                      class="p-1 rounded text-base-content/50 hover:text-success hover:bg-base-300"
                    >
                      <Check class="w-3.5 h-3.5" />
                    </button>
                  {/if}
                  <button
                    type="button"
                    onclick={() => dismiss(n)}
                    aria-label={$_("notifications.dismiss")}
                    title={$_("notifications.dismiss")}
                    class="p-1 rounded text-base-content/50 hover:text-error hover:bg-base-300"
                  >
                    <Trash2 class="w-3.5 h-3.5" />
                  </button>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>
  {/if}
</div>
