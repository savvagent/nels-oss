<script>
  import { _ } from 'svelte-i18n';
  import { FEATURE_KEYS } from '$lib/compare/competitors.js';
  import { pricingSummary } from '$lib/compare/pricing.js';
  import { NELS_FEATURES, NELS_PRICING_SUMMARY, mark, markLabel } from '$lib/compare/nels.js';
  import PriceCell from '$lib/components/PriceCell.svelte';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { faqPage } from '$lib/components/schema.js';

  let { data } = $props();
  const c = $derived(data.competitor);

  // Rendered visibly below (the FAQ section) AND fed into the FAQPage JSON-LD
  // below, from the same i18n keys, so the two can never diverge.
  const faqItems = $derived([
    { q: $_(`compare.${c.slug}.faq.q1`), a: $_(`compare.${c.slug}.faq.a1`) },
    { q: $_(`compare.${c.slug}.faq.q2`), a: $_(`compare.${c.slug}.faq.a2`) },
    { q: $_(`compare.${c.slug}.faq.q3`), a: $_(`compare.${c.slug}.faq.a3`) },
  ]);
  const faq = $derived(faqPage(faqItems));
</script>

<Seo
  title={$_(`compare.${c.slug}.seo.title`)}
  path={`/compare/${c.slug}`}
  lang={data.lang}
  description={$_(`compare.${c.slug}.seo.description`)}
/>
<JsonLd data={faq} />

<Section>
  <h1 class="text-center text-4xl font-bold">{$_(`compare.${c.slug}.headline`)}</h1>
  <p class="mx-auto mt-4 max-w-2xl text-center opacity-80">{$_(`compare.${c.slug}.intro`)}</p>

  <!-- tabindex+role=region is the WCAG SC 2.1.1 technique (SCR29) for making
       a horizontally-scrollable region keyboard-operable; the linter can't
       see that this div scrolls, so it flags a non-interactive tabindex. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
      class="mt-8 overflow-x-auto"
      tabindex="0"
      role="region"
      aria-label={$_('compare.scrollRegion', { values: { name: c.name } })}
    >
      <table class="table">
        <caption class="sr-only">
          {$_('compare.tableScrollLabel', { values: { name: c.name } })}
        </caption>
        <thead>
        <tr>
          <th scope="col">{$_('compare.colFeature')}</th>
          <th scope="col">{$_('compare.colNels')}</th>
          <th scope="col">{c.name}</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row" class="font-normal text-start">{$_('compare.pricing.label')}</th>
          <td class="font-semibold text-primary"><PriceCell summary={NELS_PRICING_SUMMARY} /></td>
          <td class="opacity-70"><PriceCell summary={pricingSummary(c.pricing)} /></td>
        </tr>
        {#each FEATURE_KEYS as key}
          <tr>
            <th scope="row" class="font-normal text-start">{$_(`compare.feature.${key}`)}</th>
            <td class="font-semibold text-primary">
              <span aria-hidden="true">{mark(NELS_FEATURES[key])}</span>
              <span class="sr-only">{$_(markLabel(NELS_FEATURES[key]))}</span>
            </td>
            <td class="opacity-70">
              <span aria-hidden="true">{mark(c.features[key])}</span>
              <span class="sr-only">{$_(markLabel(c.features[key]))}</span>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  <p class="mt-2 text-xs opacity-60">
    <span aria-hidden="true">✓</span> {$_('compare.mark.yes')} ·
    <span aria-hidden="true">~</span> {$_('compare.mark.partial')} ·
    <span aria-hidden="true">—</span> {$_('compare.mark.no')}
  </p>
  <p class="mt-2 text-xs opacity-60">
    {$_('compare.verifiedOn', { values: { date: c.verifiedOn } })}
    <a class="link" href={c.sourceUrl} rel="nofollow noopener" target="_blank">{c.name}</a>
  </p>

  <h2 class="mt-12 text-2xl font-bold">{$_(`compare.${c.slug}.whoForTitle`)}</h2>
  <p class="mt-2 opacity-80">{$_(`compare.${c.slug}.whoFor`)}</p>

  <h2 class="mt-8 text-2xl font-bold">{$_(`compare.${c.slug}.strengthTitle`)}</h2>
  <p class="mt-2 opacity-80">{$_(c.strength)}</p>

  <h2 class="mt-8 text-2xl font-bold">{$_(`compare.${c.slug}.verdictTitle`)}</h2>
  <p class="mt-2 opacity-80">{$_(`compare.${c.slug}.verdict`)}</p>

  <h2 class="mt-8 text-2xl font-bold">{$_('faq.title')}</h2>
  <div class="mt-4 space-y-3">
    {#each faqItems as item}
      <details class="collapse-arrow collapse bg-base-200">
        <summary class="collapse-title font-medium">{item.q}</summary>
        <div class="collapse-content text-sm opacity-80"><p>{item.a}</p></div>
      </details>
    {/each}
  </div>

  <div class="mt-6 flex justify-center gap-4">
    <a class="link" href={`/${data.lang}/compare`}>{$_('compare.title')}</a>
    <a class="link" href={`/${data.lang}/pricing`}>{$_('nav.pricing')}</a>
  </div>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
