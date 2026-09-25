<script>
  import { _ } from 'svelte-i18n';
  import { COMPETITORS, FEATURE_KEYS, SEGMENTS } from '$lib/compare/competitors.js';
  import { pricingSummary } from '$lib/compare/pricing.js';
  import { NELS_FEATURES, NELS_PRICING_SUMMARY, SEGMENT_KEY, mark, markLabel, oldestVerifiedOn } from '$lib/compare/nels.js';
  import PriceCell from '$lib/components/PriceCell.svelte';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';

  let { data } = $props();

  const grouped = $derived(
    SEGMENTS.map((s) => {
      const items = COMPETITORS.filter((c) => c.segment === s);
      return {
        segment: s,
        items,
        verifiedOn: oldestVerifiedOn(items),
      };
    })
  );

</script>

<Seo
  title={$_('compare.seo.title')}
  path="/compare"
  lang={data.lang}
  description={$_('compare.seo.description')}
/>

<Section>
  <h1 class="text-center text-4xl font-bold">{$_('compare.title')}</h1>
  <p class="mx-auto mt-4 max-w-2xl text-center opacity-80">{$_('compare.subtitle')}</p>
  <p class="mx-auto mt-2 max-w-2xl text-center opacity-70">{$_('compare.hubIntro')}</p>

  {#each grouped as group}
    <h2 class="mt-12 text-2xl font-bold">{$_(`compare.segment.${SEGMENT_KEY[group.segment]}`)}</h2>
    <!-- tabindex+role=region is the WCAG SC 2.1.1 technique (SCR29) for making
         a horizontally-scrollable region keyboard-operable; the linter can't
         see that this div scrolls, so it flags a non-interactive tabindex. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="mt-4 overflow-x-auto"
      tabindex="0"
      role="region"
      aria-label={$_('compare.scrollRegion', {
        values: { name: $_(`compare.segment.${SEGMENT_KEY[group.segment]}`) },
      })}
    >
      <table class="table">
        <caption class="sr-only">
          {$_('compare.tableScrollLabel', {
            values: { name: $_(`compare.segment.${SEGMENT_KEY[group.segment]}`) },
          })}
        </caption>
        <thead>
          <tr>
            <th scope="col">{$_('compare.colFeature')}</th>
            <th scope="col">{$_('compare.colNels')}</th>
            {#each group.items as c}<th scope="col">{c.name}</th>{/each}
          </tr>
        </thead>
        <tbody>
          <tr>
            <th scope="row" class="font-normal text-start">{$_('compare.pricing.label')}</th>
            <td class="font-semibold text-primary"><PriceCell summary={NELS_PRICING_SUMMARY} /></td>
            {#each group.items as c}
              <td class="opacity-70"><PriceCell summary={pricingSummary(c.pricing)} /></td>
            {/each}
          </tr>
          {#each FEATURE_KEYS as key}
            <tr>
              <th scope="row" class="font-normal text-start">{$_(`compare.feature.${key}`)}</th>
              <td class="font-semibold text-primary">
                <span aria-hidden="true">{mark(NELS_FEATURES[key])}</span>
                <span class="sr-only">{$_(markLabel(NELS_FEATURES[key]))}</span>
              </td>
              {#each group.items as c}
                <td class="opacity-70">
                  <span aria-hidden="true">{mark(c.features[key])}</span>
                  <span class="sr-only">{$_(markLabel(c.features[key]))}</span>
                </td>
              {/each}
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
    {#if group.verifiedOn}
      <p class="mt-2 text-xs opacity-60">
        {$_('compare.verifiedOn', { values: { date: group.verifiedOn } })}
      </p>
    {/if}
    <div class="mt-3 flex flex-wrap gap-4">
      {#each group.items.filter((c) => c.published) as c}
        <a class="link" href={`/${data.lang}/compare/${c.slug}`}>
          {$_('compare.viewComparison')} — {c.name}
        </a>
      {/each}
    </div>
  {/each}

  <p class="mt-12 opacity-80">{$_('compare.spreadsheetRow')}</p>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
