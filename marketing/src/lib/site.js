export const SITE = {
  name: 'Nels',
  tagline: 'AI Budgeting Co-Pilot',
  url: 'https://nels.money',
  appUrl: 'https://app.nels.money',
  supportEmail: 'support@nels.money',
  // Public AGPL-3.0 source. AGPL §13 requires offering network users the source,
  // so the footer links here from every page.
  sourceUrl: 'https://github.com/savvagent/nels-oss',
  description:
    'Chat your way to financial clarity. Manage budgets, track expenses, and share with family — all with passwordless sign-in.',
  trialDays: 7,
  // Two-tier pricing. `price` is kept as the ENTRY (Basic) price so existing
  // "from {monthly}" copy on the home/faq/features/terms pages stays correct via
  // i18n interpolation; the /pricing page reads the full `plans` structure.
  plans: {
    basic: { monthly: '$3', annual: '$30' },
    pro: { monthly: '$5', annual: '$50' },
  },
  price: { monthly: '$3', annual: '$30' },
  // Real social profiles only; leave empty until they exist (no placeholders in schema).
  social: [],
};

/**
 * Marketing nav links. `key` is an i18n message key (resolved with $_), `path`
 * is the locale-independent path that the nav prefixes with the active locale.
 */
export const NAV = [
  { key: 'nav.features', path: '/features' },
  { key: 'nav.howItWorks', path: '/how-it-works' },
  { key: 'nav.pricing', path: '/pricing' },
  { key: 'nav.security', path: '/security' },
  { key: 'nav.compare', path: '/compare' },
];
