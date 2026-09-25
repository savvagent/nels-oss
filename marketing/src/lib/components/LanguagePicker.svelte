<script>
  import { Globe, Check } from 'lucide-svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { _, locale } from 'svelte-i18n';
  import { LOCALES, isLocale, persistLocale } from '$lib/i18n/index.js';

  // Active base code (region stripped) for highlighting the current choice.
  let active = $derived(($locale || 'en').slice(0, 2));

  // Same page under the chosen locale's prefix. From a non-localized page
  // (e.g. /privacy) fall back to that locale's home.
  /** @param {string} code */
  function targetPath(code) {
    const segs = page.url.pathname.split('/');
    if (isLocale(segs[1])) {
      segs[1] = code;
      return segs.join('/');
    }
    return `/${code}`;
  }

  /** @param {string} code */
  function choose(code) {
    persistLocale(code);
    // Close the daisyUI dropdown by blurring the focused menu item.
    if (typeof document !== 'undefined') {
      /** @type {HTMLElement | null} */ (document.activeElement)?.blur();
    }
    goto(targetPath(code));
  }
</script>

<div class="dropdown dropdown-end">
  <button
    tabindex="0"
    type="button"
    class="btn btn-ghost btn-sm min-h-11 min-w-11 mouse:min-h-0 mouse:min-w-0 gap-1 px-2 text-base-content/70"
    aria-label={$_('language.label')}
    title={$_('language.label')}
  >
    <Globe class="h-4 w-4" />
    <span class="text-[11px] font-semibold uppercase">{active}</span>
  </button>
  <ul
    class="dropdown-content menu z-50 mt-2 w-44 max-w-[calc(100vw-2rem)] rounded-xl border border-base-300 bg-base-200 p-1 shadow-2xl"
  >
    {#each LOCALES as opt}
      <li>
        <button
          type="button"
          class="min-h-11 flex items-center justify-between gap-2 text-sm {active === opt.code
            ? 'font-semibold text-primary'
            : 'text-base-content/80'}"
          aria-current={active === opt.code}
          onclick={() => choose(opt.code)}
        >
          {opt.label}
          {#if active === opt.code}
            <Check class="h-4 w-4 shrink-0" />
          {/if}
        </button>
      </li>
    {/each}
  </ul>
</div>
