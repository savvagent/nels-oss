<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { productOffers } from '$lib/components/schema.js';
  import { SITE } from '$lib/site.js';

  let { data } = $props();

  // Billing-period toggle — default annual (the better value).
  let annual = $state(true);
  const cadence = $derived(annual ? 'annual' : 'monthly');
  const per = $derived(annual ? $_('pricing.perYear') : $_('pricing.perMonth'));
  const basicPrice = $derived(annual ? SITE.plans.basic.annual : SITE.plans.basic.monthly);
  const proPrice = $derived(annual ? SITE.plans.pro.annual : SITE.plans.pro.monthly);

  // Basic (entry) price feeds the SEO/subtitle "from" copy via i18n interpolation.
  const vals = { days: SITE.trialDays, monthly: SITE.plans.basic.monthly, annual: SITE.plans.basic.annual };

  // Basic bundles the six non-sync capabilities; bank sync is the Pro upsell.
  const basicFeatures = $derived([
    $_('pricing.included1'),
    $_('pricing.included2'),
    $_('pricing.included3'),
    $_('pricing.included4'),
    $_('pricing.included5'),
    $_('pricing.included6'),
  ]);
</script>

<Seo
  title={$_('pricing.seo.title')}
  path="/pricing"
  lang={data.lang}
  description={$_('pricing.seo.description', { values: vals })}
/>
<JsonLd data={productOffers()} />

<Section classes="text-center">
  <h1 class="text-4xl font-bold">{$_('pricing.title')}</h1>
  <p class="mx-auto mt-4 max-w-xl opacity-80">{$_('pricing.subtitle', { values: vals })}</p>

  <!-- Billing-period toggle -->
  <div class="mt-8 flex justify-center">
    <div class="join" role="group" aria-label={$_('pricing.billingPeriod')}>
      <button
        type="button"
        class="btn join-item {annual ? 'btn-ghost' : 'btn-primary'}"
        aria-pressed={!annual}
        onclick={() => (annual = false)}
      >
        {$_('pricing.monthly')}
      </button>
      <button
        type="button"
        class="btn join-item {annual ? 'btn-primary' : 'btn-ghost'}"
        aria-pressed={annual}
        onclick={() => (annual = true)}
      >
        {$_('pricing.annual')}
        <span class="badge badge-sm ml-2">{$_('pricing.saveBadge')}</span>
      </button>
    </div>
  </div>

  <!-- Tier cards: stack on mobile, side by side from sm up -->
  <div class="mx-auto mt-10 grid max-w-4xl gap-6 sm:grid-cols-2">
    <!-- Basic -->
    <div class="card border border-base-300 bg-base-100 p-8 text-left">
      <h2 class="text-xl font-semibold">{$_('pricing.tierBasicName')}</h2>
      <p class="mt-1 text-sm opacity-70">{$_('pricing.tierBasicTagline')}</p>
      <p class="mt-4 text-4xl font-bold">
        {basicPrice}<span class="text-base font-normal opacity-70">{per}</span>
      </p>
      <ul class="mt-6 space-y-2 text-sm">
        {#each basicFeatures as f}
          <li class="flex gap-2"><span class="text-primary">✓</span><span class="opacity-80">{f}</span></li>
        {/each}
        <li class="flex gap-2 opacity-50">
          <span aria-hidden="true">✗</span><span>{$_('pricing.bankSyncExcluded')}</span>
        </li>
      </ul>
      <Cta plan="basic" {cadence} label={$_('cta.startTrial')} classes="mt-8 w-full" />
    </div>

    <!-- Pro (highlighted) -->
    <div class="card relative border-2 border-primary bg-base-100 p-8 text-left">
      <div class="badge badge-primary absolute -top-3 right-6">{$_('pricing.mostPopular')}</div>
      <h2 class="text-xl font-semibold">{$_('pricing.tierProName')}</h2>
      <p class="mt-1 text-sm opacity-70">{$_('pricing.tierProTagline')}</p>
      <p class="mt-4 text-4xl font-bold">
        {proPrice}<span class="text-base font-normal opacity-70">{per}</span>
      </p>
      <ul class="mt-6 space-y-2 text-sm">
        <li class="flex gap-2">
          <span class="text-primary">✓</span><span class="font-medium opacity-90">{$_('pricing.everythingInBasic')}</span>
        </li>
        <li class="flex gap-2"><span class="text-primary">✓</span><span class="opacity-80">{$_('pricing.included7')}</span></li>
      </ul>
      <Cta plan="pro" {cadence} label={$_('cta.startTrial')} classes="mt-8 w-full" />
    </div>
  </div>

  <p class="mx-auto mt-8 max-w-xl text-sm opacity-70">{$_('pricing.trustLine', { values: vals })}</p>
</Section>
