import { describe, it, expect } from 'vitest';
import { organization, softwareApplication, faqPage, howTo, productOffers } from './components/schema.js';

describe('schema builders', () => {
  it('organization has required fields and no empty sameAs', () => {
    const o = organization();
    expect(o['@type']).toBe('Organization');
    expect(o.url).toBe('https://nels.money');
    expect('sameAs' in o).toBe(false);
  });
  it('softwareApplication is a FinanceApplication', () => {
    const s = softwareApplication();
    expect(s['@type']).toBe('SoftwareApplication');
    expect(s.applicationCategory).toBe('FinanceApplication');
  });
  it('faqPage maps Q/A pairs', () => {
    const f = faqPage([{ q: 'Is it safe?', a: 'Yes.' }]);
    expect(f['@type']).toBe('FAQPage');
    expect(f.mainEntity[0].name).toBe('Is it safe?');
    expect(f.mainEntity[0].acceptedAnswer.text).toBe('Yes.');
  });
  it('howTo lists ordered steps', () => {
    const h = howTo([{ name: 'A', text: 'do a' }, { name: 'B', text: 'do b' }]);
    expect(h.step).toHaveLength(2);
    expect(h.step[1].name).toBe('B');
  });
  it('productOffers lists both tiers × both cadences with their prices', () => {
    const p = productOffers();
    const byName = Object.fromEntries(p.offers.map((o) => [o.name, o.price]));
    expect(byName['Basic Monthly']).toBe('3');
    expect(byName['Basic Annual']).toBe('30');
    expect(byName['Pro Monthly']).toBe('5');
    expect(byName['Pro Annual']).toBe('50');
    p.offers.forEach((o) => expect(o.priceCurrency).toBe('USD'));
  });
});
