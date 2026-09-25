<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { faqPage } from '$lib/components/schema.js';
  import { SITE } from '$lib/site.js';

  let { data } = $props();

  const vals = { days: SITE.trialDays, monthly: SITE.price.monthly, annual: SITE.price.annual };

  const faqs = $derived([
    { q: $_('faq.q1'), a: $_('faq.a1') },
    { q: $_('faq.q2'), a: $_('faq.a2') },
    { q: $_('faq.q3'), a: $_('faq.a3') },
    { q: $_('faq.q4'), a: $_('faq.a4') },
    { q: $_('faq.q5'), a: $_('faq.a5', { values: vals }) },
  ]);
</script>

<Seo title={$_('faq.seo.title')} path="/faq" lang={data.lang} description={$_('faq.seo.description')} />
<JsonLd data={faqPage(faqs)} />

<Section classes="max-w-2xl">
  <h1 class="text-center text-4xl font-bold">{$_('faq.title')}</h1>
  <div class="mt-8 space-y-3">
    {#each faqs as item}
      <div class="collapse-arrow collapse bg-base-200">
        <input type="checkbox" aria-label={item.q} />
        <div class="collapse-title font-medium">{item.q}</div>
        <div class="collapse-content text-sm opacity-80"><p>{item.a}</p></div>
      </div>
    {/each}
  </div>
</Section>
