import { SITE } from '$lib/site.js';
import { LOCALES, FALLBACK } from '$lib/i18n/index.js';
import { LOCALIZED_PAGES } from './pages.js';

export const prerender = true;

const STATIC_PAGES = ['/privacy', '/terms'];

// hreflang alternates (one per locale + x-default) for a localized page path,
// emitted as xhtml:link entries so search engines see reciprocal alternates.
/** @param {string} path */
function alternates(path) {
  const links = LOCALES.map(
    ({ code }) => `    <xhtml:link rel="alternate" hreflang="${code}" href="${SITE.url}/${code}${path}"/>`
  );
  links.push(`    <xhtml:link rel="alternate" hreflang="x-default" href="${SITE.url}/${FALLBACK}${path}"/>`);
  return links.join('\n');
}

export function GET() {
  const entries = [];

  for (const path of LOCALIZED_PAGES) {
    const alt = alternates(path);
    for (const { code } of LOCALES) {
      entries.push(
        `  <url>\n    <loc>${SITE.url}/${code}${path}</loc>\n${alt}\n    <changefreq>weekly</changefreq>\n  </url>`
      );
    }
  }

  for (const path of STATIC_PAGES) {
    entries.push(`  <url>\n    <loc>${SITE.url}${path}</loc>\n    <changefreq>weekly</changefreq>\n  </url>`);
  }

  const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">\n${entries.join('\n')}\n</urlset>`;
  return new Response(xml, { headers: { 'content-type': 'application/xml' } });
}
