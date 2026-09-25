import { render } from '@testing-library/svelte';
import { describe, it, expect } from 'vitest';
import Seo from './components/Seo.svelte';

describe('Seo', () => {
  it('sets a locale-prefixed canonical and reciprocal hreflang alternates', () => {
    render(Seo, { title: 'Pricing', description: 'Plans and pricing', path: '/pricing', lang: 'en' });
    expect(document.title).toBe('Pricing | Nels');

    const desc = document.head.querySelector('meta[name="description"]');
    expect(desc?.getAttribute('content')).toBe('Plans and pricing');

    const canonical = document.head.querySelector('link[rel="canonical"]');
    expect(canonical?.getAttribute('href')).toBe('https://nels.money/en/pricing');

    const ogTitle = document.head.querySelector('meta[property="og:title"]');
    expect(ogTitle?.getAttribute('content')).toBe('Pricing | Nels');

    // One alternate per locale + x-default.
    const es = document.head.querySelector('link[rel="alternate"][hreflang="es"]');
    expect(es?.getAttribute('href')).toBe('https://nels.money/es/pricing');
    const xDefault = document.head.querySelector('link[rel="alternate"][hreflang="x-default"]');
    expect(xDefault?.getAttribute('href')).toBe('https://nels.money/en/pricing');
  });

  it('uses the bare site name and the locale home URL when no title is given', () => {
    render(Seo, { description: 'Home', path: '', lang: 'en' });
    expect(document.title).toBe('Nels — AI Budgeting Co-Pilot');
    const canonical = document.head.querySelector('link[rel="canonical"]');
    expect(canonical?.getAttribute('href')).toBe('https://nels.money/en');
  });

  it('omits hreflang alternates for non-localized (legal) pages', () => {
    render(Seo, { title: 'Privacy Policy', path: '/privacy', localized: false });
    const canonical = document.head.querySelector('link[rel="canonical"]');
    expect(canonical?.getAttribute('href')).toBe('https://nels.money/privacy');
    expect(document.head.querySelector('link[rel="alternate"]')).toBeNull();
  });
});
