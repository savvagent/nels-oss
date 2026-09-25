<script>
  import { _, locale } from 'svelte-i18n';
  import { SITE, NAV } from '$lib/site.js';
  import Cta from './Cta.svelte';
  import LanguagePicker from './LanguagePicker.svelte';
  import ThemePicker from './ThemePicker.svelte';

  // Active locale drives every nav link's prefix.
  let lang = $derived(($locale || 'en').slice(0, 2));
</script>

<header class="navbar mx-auto max-w-5xl px-4">
  <div class="min-w-0 flex-1">
    <a href={`/${lang}`} class="flex items-center gap-2 text-xl font-bold text-primary">
      <img src="/nels-mark.png" alt="" class="h-7 w-7 shrink-0" width="28" height="28" />
      <span class="truncate">{SITE.name}</span>
    </a>
  </div>
  <nav class="hidden gap-2 md:flex">
    {#each NAV as item}
      <a href={`/${lang}${item.path}`} class="btn btn-ghost btn-sm">{$_(item.key)}</a>
    {/each}
  </nav>
  <div class="ml-2 flex shrink-0 items-center gap-1.5">
    <LanguagePicker />
    <ThemePicker />
    <a href={SITE.appUrl} class="btn btn-ghost btn-sm min-h-11 mouse:min-h-0">{$_('cta.signIn')}</a>
    <Cta label={$_('cta.startTrial')} classes="min-h-11 mouse:min-h-0 mouse:btn-sm" />
  </div>
</header>
