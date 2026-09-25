<script>
  import { SITE } from '$lib/site.js';
  import { LOCALES, FALLBACK } from '$lib/i18n/index.js';

  /**
   * @type {{
   *   title?: string,
   *   description?: string,
   *   path?: string,
   *   lang?: string,
   *   localized?: boolean,
   *   image?: string,
   * }}
   *
   * `path` is the locale-independent page path (e.g. '' for the home page,
   * '/features'). For localized pages the canonical/alternates are built as
   * `${url}/${lang}${path}`. Set `localized={false}` (and pass the full path,
   * e.g. '/privacy') for the English-only legal pages: no hreflang alternates.
   */
  let {
    title,
    description = SITE.description,
    path = '',
    lang = FALLBACK,
    localized = true,
    image = '/og-image.png',
  } = $props();

  const fullTitle = $derived(title ? `${title} | ${SITE.name}` : `${SITE.name} — ${SITE.tagline}`);

  // Build the canonical URL. Localized pages are prefixed with their locale.
  const canonical = $derived(
    localized ? `${SITE.url}/${lang}${path}` : `${SITE.url}${path === '/' ? '' : path}`
  );

  // hreflang alternates: one per locale + x-default → English.
  const alternates = $derived(
    localized
      ? [
          ...LOCALES.map((l) => ({ hreflang: l.code, href: `${SITE.url}/${l.code}${path}` })),
          { hreflang: 'x-default', href: `${SITE.url}/${FALLBACK}${path}` },
        ]
      : []
  );

  const ogImage = $derived(image.startsWith('http') ? image : `${SITE.url}${image}`);

  // Open Graph expects locale in language_TERRITORY form, not a bare ISO code.
  /** @type {Record<string, string>} */
  const OG_LOCALE = { en: 'en_US', es: 'es_ES', fr: 'fr_FR', de: 'de_DE', it: 'it_IT', pt: 'pt_PT' };
  const ogLocale = $derived(OG_LOCALE[lang] ?? 'en_US');
</script>

<svelte:head>
  <title>{fullTitle}</title>
  <meta name="description" content={description} />
  <link rel="canonical" href={canonical} />

  {#each alternates as alt}
    <link rel="alternate" hreflang={alt.hreflang} href={alt.href} />
  {/each}

  <meta property="og:type" content="website" />
  <meta property="og:site_name" content={SITE.name} />
  <meta property="og:title" content={fullTitle} />
  <meta property="og:description" content={description} />
  <meta property="og:url" content={canonical} />
  <meta property="og:image" content={ogImage} />
  {#if localized}
    <meta property="og:locale" content={ogLocale} />
  {/if}

  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content={fullTitle} />
  <meta name="twitter:description" content={description} />
  <meta name="twitter:image" content={ogImage} />
</svelte:head>
