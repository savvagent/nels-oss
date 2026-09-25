<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import { SITE } from '$lib/site.js';

  let { data } = $props();

  const pillars = $derived([
    { sub: $_('features.pillar1.sub'), h: $_('features.pillar1.h'), body: $_('features.pillar1.body') },
    { sub: $_('features.pillar2.sub'), h: $_('features.pillar2.h'), body: $_('features.pillar2.body') },
    { sub: $_('features.pillar3.sub'), h: $_('features.pillar3.h'), body: $_('features.pillar3.body') },
    { sub: $_('features.pillar4.sub'), h: $_('features.pillar4.h'), body: $_('features.pillar4.body') },
    { sub: $_('features.pillar5.sub'), h: $_('features.pillar5.h'), body: $_('features.pillar5.body', { values: { monthly: SITE.plans.pro.monthly } }) },
  ]);
</script>

<Seo
  title={$_('features.seo.title')}
  path="/features"
  lang={data.lang}
  description={$_('features.seo.description')}
/>

<Section classes="text-center">
  <h1 class="text-4xl font-bold">{$_('features.title')}</h1>
  <p class="mx-auto mt-4 max-w-2xl opacity-80">{$_('features.subtitle')}</p>
</Section>

<Section>
  <div class="space-y-6">
    {#each pillars as p}
      <div class="card bg-base-200 p-8">
        <p class="text-sm uppercase tracking-wide text-primary">{p.sub}</p>
        <h2 class="mt-1 text-2xl font-semibold">{p.h}</h2>
        <p class="mt-3 opacity-80">{p.body}</p>
      </div>
    {/each}
  </div>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
