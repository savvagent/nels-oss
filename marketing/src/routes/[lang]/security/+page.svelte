<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';

  let { data } = $props();
</script>

<Seo
  title={$_('security.seo.title')}
  path="/security"
  lang={data.lang}
  description={$_('security.seo.description')}
/>

<Section classes="max-w-2xl">
  <h1 class="text-4xl font-bold">{$_('security.title')}</h1>
  <div class="prose mt-6 max-w-none opacity-90">
    <h2>{$_('security.h1')}</h2>
    <p>{$_('security.p1')}</p>
    <h2>{$_('security.h2')}</h2>
    <!-- p2 contains an inline link to the English-only Privacy Policy. -->
    <p>{@html $_('security.p2')}</p>
    <h2>{$_('security.h3')}</h2>
    <p>{$_('security.p3')}</p>
  </div>
  <div class="mt-8"><Cta /></div>
</Section>
