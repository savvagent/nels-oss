import { describe, it, expect } from 'vitest';
import { GET } from './sitemap.xml/+server.js';

describe('sitemap.xml', () => {
  it('lists every locale × page URL plus the English-only legal pages', async () => {
    const res = await GET();
    const body = await res.text();
    expect(res.headers.get('content-type')).toContain('xml');

    // Localized home + a localized inner page, across multiple locales.
    expect(body).toContain('<loc>https://nels.money/en</loc>');
    expect(body).toContain('<loc>https://nels.money/en/pricing</loc>');
    expect(body).toContain('<loc>https://nels.money/es/pricing</loc>');
    expect(body).toContain('<loc>https://nels.money/pt/features</loc>');

    // Legal pages stay top-level (no locale prefix).
    expect(body).toContain('<loc>https://nels.money/privacy</loc>');
    expect(body).toContain('<loc>https://nels.money/terms</loc>');

    // Reciprocal hreflang alternates, including x-default → English.
    expect(body).toContain('hreflang="x-default" href="https://nels.money/en/pricing"');
    expect(body).toContain('hreflang="fr" href="https://nels.money/fr/pricing"');
  });
});
