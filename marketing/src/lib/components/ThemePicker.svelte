<script>
  import { Sun, Monitor, Moon, Check } from 'lucide-svelte';
  import { _ } from 'svelte-i18n';
  import { getStoredTheme, applyTheme } from '$lib/theme.js';

  // Highlight reflects the persisted choice; the pre-paint script in app.html
  // already applied it to <html>, so we only re-apply on user interaction.
  // Initialise to "system" for the prerendered/SSR pass (no localStorage there),
  // then sync to the stored choice once mounted on the client.
  let current = $state('system');

  $effect(() => {
    current = getStoredTheme();
  });

  const OPTIONS = [
    { value: 'light', key: 'theme.light', Icon: Sun },
    { value: 'system', key: 'theme.system', Icon: Monitor },
    { value: 'dark', key: 'theme.dark', Icon: Moon },
  ];

  // Drives the trigger icon; falls back to "system" before the client syncs.
  let active = $derived(OPTIONS.find((o) => o.value === current) ?? OPTIONS[1]);

  /** @param {string} value */
  function choose(value) {
    current = applyTheme(value);
    // Close the daisyUI dropdown by blurring the focused menu item.
    if (typeof document !== 'undefined') {
      /** @type {HTMLElement | null} */ (document.activeElement)?.blur();
    }
  }
</script>

<div class="dropdown dropdown-end">
  <button
    tabindex="0"
    type="button"
    class="btn btn-ghost btn-sm min-h-11 min-w-11 mouse:min-h-0 mouse:min-w-0 px-2 text-base-content/70"
    aria-label={$_('theme.groupLabel')}
    title={$_('theme.groupLabel')}
  >
    <active.Icon class="h-4 w-4" />
  </button>
  <ul
    class="dropdown-content menu z-50 mt-2 w-40 max-w-[calc(100vw-2rem)] rounded-xl border border-base-300 bg-base-200 p-1 shadow-2xl"
  >
    {#each OPTIONS as opt}
      <li>
        <button
          type="button"
          class="min-h-11 flex items-center justify-between gap-2 text-sm {current === opt.value
            ? 'font-semibold text-primary'
            : 'text-base-content/80'}"
          aria-current={current === opt.value}
          onclick={() => choose(opt.value)}
        >
          <span class="flex items-center gap-2">
            <opt.Icon class="h-4 w-4 shrink-0" />
            {$_(opt.key)}
          </span>
          {#if current === opt.value}
            <Check class="h-4 w-4 shrink-0" />
          {/if}
        </button>
      </li>
    {/each}
  </ul>
</div>
