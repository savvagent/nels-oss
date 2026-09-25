<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { howTo } from '$lib/components/schema.js';

  let { data } = $props();

  const steps = $derived([
    { name: $_('howItWorks.step1.name'), text: $_('howItWorks.step1.text') },
    { name: $_('howItWorks.step2.name'), text: $_('howItWorks.step2.text') },
    { name: $_('howItWorks.step3.name'), text: $_('howItWorks.step3.text') },
    { name: $_('howItWorks.step4.name'), text: $_('howItWorks.step4.text') },
  ]);
</script>

<Seo
  title={$_('howItWorks.seo.title')}
  path="/how-it-works"
  lang={data.lang}
  description={$_('howItWorks.seo.description')}
/>
<JsonLd data={howTo(steps)} />

<Section classes="text-center">
  <h1 class="text-4xl font-bold">{$_('howItWorks.title')}</h1>
  <p class="mx-auto mt-4 max-w-2xl opacity-80">{$_('howItWorks.subtitle')}</p>
</Section>

<Section>
  <ol class="space-y-6">
    {#each steps as s, i}
      <li class="flex gap-4">
        <span
          class="flex h-10 w-10 flex-none items-center justify-center rounded-full bg-primary font-bold text-primary-content"
          >{i + 1}</span
        >
        <div>
          <h2 class="text-xl font-semibold">{s.name}</h2>
          <p class="mt-1 opacity-80">{s.text}</p>
        </div>
      </li>
    {/each}
  </ol>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
