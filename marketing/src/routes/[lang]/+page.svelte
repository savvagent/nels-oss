<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { faqPage } from '$lib/components/schema.js';
  import { SITE } from '$lib/site.js';

  let { data } = $props();

  // Interpolation values shared across the page copy.
  const vals = { days: SITE.trialDays, monthly: SITE.price.monthly, annual: SITE.price.annual };

  const features = $derived([
    { title: $_('home.features.item1.title'), body: $_('home.features.item1.body') },
    { title: $_('home.features.item2.title'), body: $_('home.features.item2.body') },
    { title: $_('home.features.item3.title'), body: $_('home.features.item3.body') },
    { title: $_('home.features.item4.title'), body: $_('home.features.item4.body') },
  ]);

  const faqs = $derived([
    { q: $_('home.faq.q1'), a: $_('home.faq.a1') },
    { q: $_('home.faq.q2'), a: $_('home.faq.a2') },
    { q: $_('home.faq.q3'), a: $_('home.faq.a3') },
    { q: $_('home.faq.q4'), a: $_('home.faq.a4', { values: vals }) },
  ]);
</script>

<Seo title={$_('home.seo.title')} path="" lang={data.lang} description={$_('home.seo.description')} />
<JsonLd data={faqPage(faqs)} />

<Section classes="text-center">
  <h1 class="text-5xl font-bold tracking-tight">{$_('home.hero.title')}</h1>
  <p class="mx-auto mt-4 max-w-2xl text-lg opacity-80">{$_('home.hero.subtitle')}</p>
  <div class="mt-8 flex justify-center gap-3">
    <Cta label={$_('cta.tryFree', { values: vals })} />
    <a href={`/${data.lang}/how-it-works`} class="btn btn-ghost">{$_('home.hero.ctaSecondary')}</a>
  </div>
  <p class="mt-4 text-sm opacity-70">{$_('home.hero.trustLine', { values: vals })}</p>
</Section>

<Section id="why" classes="text-center">
  <h2 class="text-3xl font-bold">{$_('home.why.title')}</h2>
  <div class="mt-8 grid gap-6 md:grid-cols-3">
    <div class="card bg-base-200 p-6">
      <h3 class="font-semibold">{$_('home.why.card1.title')}</h3>
      <p class="mt-2 text-sm opacity-80">{$_('home.why.card1.body')}</p>
    </div>
    <div class="card bg-base-200 p-6">
      <h3 class="font-semibold">{$_('home.why.card2.title')}</h3>
      <p class="mt-2 text-sm opacity-80">{$_('home.why.card2.body')}</p>
    </div>
    <div class="card bg-base-200 p-6">
      <h3 class="font-semibold">{$_('home.why.card3.title')}</h3>
      <p class="mt-2 text-sm opacity-80">{$_('home.why.card3.body')}</p>
    </div>
  </div>
</Section>

<Section id="features">
  <h2 class="text-center text-3xl font-bold">{$_('home.features.title')}</h2>
  <div class="mt-8 grid gap-6 md:grid-cols-2">
    {#each features as f}
      <div class="card bg-base-200 p-6">
        <h3 class="text-xl font-semibold text-primary">{f.title}</h3>
        <p class="mt-2 opacity-80">{f.body}</p>
      </div>
    {/each}
  </div>
</Section>

<Section id="security" classes="text-center">
  <h2 class="text-3xl font-bold">{$_('home.security.title')}</h2>
  <p class="mx-auto mt-4 max-w-2xl opacity-80">{$_('home.security.body')}</p>
  <a href={`/${data.lang}/security`} class="btn btn-ghost mt-6">{$_('home.security.link')}</a>
</Section>

<Section id="pricing" classes="text-center">
  <h2 class="text-3xl font-bold">{$_('home.pricing.title')}</h2>
  <p class="mx-auto mt-3 max-w-xl opacity-80">{$_('home.pricing.body', { values: vals })}</p>
  <a href={`/${data.lang}/pricing`} class="btn btn-ghost mt-4">{$_('home.pricing.link')}</a>
</Section>

<Section id="faq">
  <h2 class="text-center text-3xl font-bold">{$_('home.faq.title')}</h2>
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

<Section classes="text-center">
  <h2 class="text-3xl font-bold">{$_('home.finalCta.title')}</h2>
  <p class="mt-3 opacity-80">{$_('home.finalCta.body', { values: vals })}</p>
  <div class="mt-6"><Cta /></div>
</Section>
