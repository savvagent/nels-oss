import { SITE } from '$lib/site.js';

export function organization() {
  return {
    '@type': 'Organization',
    name: SITE.name,
    url: SITE.url,
    logo: `${SITE.url}/logo.png`,
    description: SITE.description,
    contactPoint: {
      '@type': 'ContactPoint',
      contactType: 'Customer Service',
      email: SITE.supportEmail,
    },
    ...(SITE.social.length ? { sameAs: SITE.social } : {}),
  };
}

export function softwareApplication() {
  return {
    '@type': 'SoftwareApplication',
    name: SITE.name,
    description: SITE.description,
    applicationCategory: 'FinanceApplication',
    operatingSystem: 'Web',
    offers: { '@type': 'Offer', priceCurrency: 'USD', price: SITE.price.monthly.replace('$', '') },
  };
}

export function productOffers() {
  const { basic, pro } = SITE.plans;
  const num = (p) => p.replace('$', '');
  return {
    '@type': 'Product',
    name: `${SITE.name} subscription`,
    description:
      'Nels plans: Basic (unlimited budgets, household sharing, AI insights, and goals) and Pro (everything in Basic plus automatic bank-account syncing).',
    offers: [
      { '@type': 'Offer', name: 'Basic Monthly', price: num(basic.monthly), priceCurrency: 'USD' },
      { '@type': 'Offer', name: 'Basic Annual', price: num(basic.annual), priceCurrency: 'USD' },
      { '@type': 'Offer', name: 'Pro Monthly', price: num(pro.monthly), priceCurrency: 'USD' },
      { '@type': 'Offer', name: 'Pro Annual', price: num(pro.annual), priceCurrency: 'USD' },
    ],
  };
}

/** @param {{q: string, a: string}[]} items */
export function faqPage(items) {
  return {
    '@type': 'FAQPage',
    mainEntity: items.map(({ q, a }) => ({
      '@type': 'Question',
      name: q,
      acceptedAnswer: { '@type': 'Answer', text: a },
    })),
  };
}

/** @param {{name: string, text: string}[]} steps */
export function howTo(steps) {
  return {
    '@type': 'HowTo',
    name: 'How to use Nels',
    step: steps.map((s) => ({ '@type': 'HowToStep', name: s.name, text: s.text })),
  };
}
