<script>
  // Renders one pricing descriptor — from `pricingSummary()` for a competitor,
  // or `NELS_PRICING_SUMMARY` for us — as the contents of a comparison-table
  // cell. The surrounding <td> and its column-specific classes stay in the
  // templates, so the Nels column keeps its emphasis and competitor columns
  // keep theirs.
  //
  // The amount arrives already formatted and is interpolated into a locale
  // shell here. Locale JSON therefore never holds a figure, which is what makes
  // a price correction in competitors.js a one-line edit.
  import { _ } from 'svelte-i18n';

  /** @type {{ summary: import('$lib/compare/pricing.js').PricingSummary }} */
  let { summary } = $props();

  // The bare period line, e.g. "$5.99/mo". Empty string for the undisclosed
  // variant, which carries no amount and never reads this.
  const price = $derived(
    summary.variant === 'undisclosed'
      ? ''
      : $_(`compare.pricing.${summary.period}`, { values: { amount: summary.display } })
  );

  // The undisclosed cell reuses the existing caveat string, so a competitor
  // that publishes nothing says so in the cell rather than rendering blank.
  const text = $derived(
    summary.variant === 'undisclosed'
      ? $_('compare.note.undisclosed')
      : summary.variant === 'exact'
        ? price
        : $_(`compare.pricing.${summary.variant}`, { values: { price } })
  );

  // The same tier's annual price, when it publishes one. Shown for every
  // product including Nels, because annual discounts are not uniform across
  // this table and a monthly-only row reads as a wider gap than the vendors
  // actually charge.
  const alternative = $derived(
    summary.annualDisplay
      ? $_('compare.pricing.orAnnual', {
          values: {
            price: $_('compare.pricing.annual', { values: { amount: summary.annualDisplay } }),
          },
        })
      : ''
  );
</script>

<span>{text}</span>
{#if alternative}
  <span class="block text-xs">{alternative}</span>
{/if}
{#if summary.caveat}
  <!-- No opacity utility here on purpose. The competitor cells are already
       `opacity-70`, and CSS opacity compounds through nesting, so a second
       `opacity-70` would land the caveat at an effective 0.49 — 3.08:1 against
       the light theme, a WCAG 1.4.3 AA failure at this 12px size. Inheriting
       the cell's own 0.7 keeps it at 5.86:1, level with the price above it. -->
  <span class="block text-xs">{$_(`compare.note.${summary.caveat}`)}</span>
{/if}
