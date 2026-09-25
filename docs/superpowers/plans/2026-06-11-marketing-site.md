# Nels Marketing Site (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Use the **sveltekit-svelte5-tailwind** skill for all SvelteKit/Svelte 5/Tailwind v4 scaffolding and idioms (runes, snippets, the Vite Tailwind plugin).

**Goal:** Ship an SEO-friendly, fully-prerendered marketing website for Nels at `nels.money` on Cloudflare Pages, with a subscription CTA that points at the app signup (Stripe wiring is Phase 2).

**Architecture:** A new top-level `marketing/` SvelteKit app in the existing monorepo, built with `@sveltejs/adapter-cloudflare` and `prerender = true` so every route is static HTML. Reusable `<Seo>` and JSON-LD components provide per-page metadata and structured data; `sitemap.xml`/`robots.txt` are prerendered endpoints. Deployed via a path-filtered GitHub Actions workflow to a new `nels-money` Cloudflare Pages project, mirroring the existing `frontend/`+`backend/` deploy setup.

**Tech Stack:** SvelteKit 2, Svelte 5 (runes), Tailwind CSS v4 (`@tailwindcss/vite`), daisyUI v5, `@sveltejs/adapter-cloudflare`, Vitest (unit), Cloudflare Pages, GitHub Actions, Wrangler.

**Source of truth for copy:** `docs/marketing/nels-seo-strategy.md`. Apply the spec's honesty guardrails (`docs/superpowers/specs/2026-06-11-marketing-site-and-subscriptions-design.md`): single plan / no free tier, no "data export" or "money-back" claims, no fabricated testimonials/user counts, "encrypted in transit" only (not "at rest" unless confirmed), real or omitted social links.

---

## File Structure

```
marketing/
  package.json
  svelte.config.js            # adapter-cloudflare
  vite.config.js              # tailwind plugin + vitest
  src/
    app.html
    app.css                   # tailwind + daisyUI + nels-light/nels-dark themes
    lib/
      site.js                 # site constants (name, url, prices, CTA target)
      components/
        Seo.svelte            # per-page <svelte:head> meta/OG/canonical
        JsonLd.svelte         # renders a JSON-LD <script> from an object
        schema.js             # builders: organization(), softwareApplication(), faqPage(), howTo(), breadcrumb(), productOffers()
        Nav.svelte
        Footer.svelte
        Cta.svelte            # primary "Start free trial" button → app signup
        Section.svelte        # layout wrapper for page sections
      schema.test.js          # unit tests for schema builders
      seo.test.js             # unit test that Seo renders expected tags
    routes/
      +layout.svelte          # Nav + Footer + default Seo + theme
      +layout.js              # export const prerender = true
      +page.svelte            # Home
      features/+page.svelte
      how-it-works/+page.svelte
      pricing/+page.svelte
      security/+page.svelte
      compare/+page.svelte
      about/+page.svelte
      faq/+page.svelte
      sitemap.xml/+server.js
      robots.txt/+server.js
    routes/sitemap.test.js    # unit test for sitemap endpoint
.github/workflows/deploy-marketing.yml
```

Each component has one responsibility; `schema.js` centralizes JSON-LD so structured data is DRY and testable.

---

## Task 1: Scaffold the SvelteKit app

**Files:**
- Create: `marketing/` (via scaffolder)
- Modify: `marketing/svelte.config.js`, `marketing/vite.config.js`, `marketing/package.json`

- [ ] **Step 1: Scaffold**

Run from the repo root:
```bash
cd /home/robhicks/dev/nels
npx sv create marketing --template minimal --types jsdoc --no-add-ons
cd marketing && pnpm install
```
(Use the sveltekit-svelte5-tailwind skill; choose SvelteKit minimal, JSDoc types, pnpm.)

- [ ] **Step 2: Add deps**

```bash
cd /home/robhicks/dev/nels/marketing
pnpm add -D @sveltejs/adapter-cloudflare @tailwindcss/vite tailwindcss daisyui vitest @testing-library/svelte jsdom
```

- [ ] **Step 3: Configure the Cloudflare adapter**

Replace `marketing/svelte.config.js`:
```js
import adapter from '@sveltejs/adapter-cloudflare';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter(),
  },
};
export default config;
```

- [ ] **Step 4: Configure Vite (Tailwind v4 + Vitest)**

Replace `marketing/vite.config.js`:
```js
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [tailwindcss(), sveltekit()],
  test: {
    environment: 'jsdom',
    include: ['src/**/*.{test,spec}.{js,ts}'],
  },
});
```

- [ ] **Step 5: Add the test script**

In `marketing/package.json` `"scripts"`, add:
```json
"test": "vitest run",
"test:watch": "vitest"
```

- [ ] **Step 6: Verify dev server boots**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm dev`
Expected: Vite serves at `http://localhost:5173` with no errors. Stop it (Ctrl-C).

- [ ] **Step 7: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/
git commit -m "feat(marketing): scaffold SvelteKit app with Cloudflare adapter"
```

---

## Task 2: Global prerender + Tailwind/daisyUI brand themes

**Files:**
- Create: `marketing/src/routes/+layout.js`
- Modify: `marketing/src/app.css`

- [ ] **Step 1: Force full static prerender**

Create `marketing/src/routes/+layout.js`:
```js
// Prerender every route to static HTML — best SEO, $0 hosting.
export const prerender = true;
export const trailingSlash = 'never';
```

- [ ] **Step 2: Tailwind v4 + daisyUI with Nels brand themes**

Replace `marketing/src/app.css`:
```css
@import 'tailwindcss';
@plugin 'daisyui';

/* Nels brand themes, mirroring the app. Primary = #aa3bff. */
@plugin 'daisyui/theme' {
  name: 'nels-light';
  default: true;
  color-scheme: light;
  --color-primary: #aa3bff;
  --color-primary-content: #ffffff;
  --color-base-100: #ffffff;
  --color-base-200: #f5f3f7;
  --color-base-300: #e7e2ee;
}

@plugin 'daisyui/theme' {
  name: 'nels-dark';
  prefersdark: true;
  color-scheme: dark;
  --color-primary: #aa3bff;
  --color-primary-content: #ffffff;
  --color-base-100: #17151c;
  --color-base-200: #1f1c27;
  --color-base-300: #2a2633;
}
```

- [ ] **Step 3: Set the theme on the document**

In `marketing/src/app.html`, set the `<html>` tag:
```html
<html lang="en" data-theme="nels-light">
```

- [ ] **Step 4: Verify Tailwind compiles**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm build`
Expected: build succeeds; `.svelte-kit`/output produced with no Tailwind errors.

- [ ] **Step 5: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/app.css marketing/src/app.html marketing/src/routes/+layout.js
git commit -m "feat(marketing): prerender all routes + Nels daisyUI themes"
```

---

## Task 3: Site constants

**Files:**
- Create: `marketing/src/lib/site.js`

- [ ] **Step 1: Define shared constants**

Create `marketing/src/lib/site.js`:
```js
export const SITE = {
  name: 'Nels',
  tagline: 'AI Budgeting Co-Pilot',
  url: 'https://nels.money',
  appUrl: 'https://app.nels.money',
  supportEmail: 'support@nels.money',
  description:
    'Chat your way to financial clarity. Manage budgets, track expenses, and share with family — all with passwordless sign-in.',
  trialDays: 14,
  price: { monthly: '$5', annual: '$50' },
  // Real social profiles only; leave empty until they exist (no placeholders in schema).
  social: [],
};

/** Marketing nav links (label, href). */
export const NAV = [
  { label: 'Features', href: '/features' },
  { label: 'How it works', href: '/how-it-works' },
  { label: 'Pricing', href: '/pricing' },
  { label: 'Security', href: '/security' },
];
```

- [ ] **Step 2: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/lib/site.js
git commit -m "feat(marketing): add site constants"
```

---

## Task 4: SEO component (TDD)

**Files:**
- Create: `marketing/src/lib/components/Seo.svelte`
- Test: `marketing/src/lib/seo.test.js`

- [ ] **Step 1: Write the failing test**

Create `marketing/src/lib/seo.test.js`:
```js
import { render } from '@testing-library/svelte';
import { describe, it, expect } from 'vitest';
import Seo from './components/Seo.svelte';

describe('Seo', () => {
  it('sets title, description, canonical, and OG tags', () => {
    render(Seo, { title: 'Pricing', description: 'Plans and pricing', path: '/pricing' });
    expect(document.title).toBe('Pricing | Nels');
    const desc = document.head.querySelector('meta[name="description"]');
    expect(desc?.getAttribute('content')).toBe('Plans and pricing');
    const canonical = document.head.querySelector('link[rel="canonical"]');
    expect(canonical?.getAttribute('href')).toBe('https://nels.money/pricing');
    const ogTitle = document.head.querySelector('meta[property="og:title"]');
    expect(ogTitle?.getAttribute('content')).toBe('Pricing | Nels');
  });

  it('uses the bare site name when no title is given', () => {
    render(Seo, { description: 'Home', path: '/' });
    expect(document.title).toBe('Nels — AI Budgeting Co-Pilot');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test src/lib/seo.test.js`
Expected: FAIL (cannot resolve `./components/Seo.svelte`).

- [ ] **Step 3: Implement Seo.svelte**

Create `marketing/src/lib/components/Seo.svelte`:
```svelte
<script>
  import { SITE } from '$lib/site.js';

  /** @type {{ title?: string, description?: string, path?: string, image?: string }} */
  let { title, description = SITE.description, path = '/', image = '/og-image.png' } = $props();

  const fullTitle = $derived(title ? `${title} | ${SITE.name}` : `${SITE.name} — ${SITE.tagline}`);
  const canonical = $derived(`${SITE.url}${path === '/' ? '' : path}`);
  const ogImage = $derived(image.startsWith('http') ? image : `${SITE.url}${image}`);
</script>

<svelte:head>
  <title>{fullTitle}</title>
  <meta name="description" content={description} />
  <link rel="canonical" href={canonical} />

  <meta property="og:type" content="website" />
  <meta property="og:site_name" content={SITE.name} />
  <meta property="og:title" content={fullTitle} />
  <meta property="og:description" content={description} />
  <meta property="og:url" content={canonical} />
  <meta property="og:image" content={ogImage} />

  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content={fullTitle} />
  <meta name="twitter:description" content={description} />
  <meta name="twitter:image" content={ogImage} />
</svelte:head>
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test src/lib/seo.test.js`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/lib/components/Seo.svelte marketing/src/lib/seo.test.js
git commit -m "feat(marketing): Seo component with meta/OG/canonical (tested)"
```

---

## Task 5: JSON-LD schema builders + component (TDD)

**Files:**
- Create: `marketing/src/lib/components/schema.js`, `marketing/src/lib/components/JsonLd.svelte`
- Test: `marketing/src/lib/schema.test.js`

- [ ] **Step 1: Write the failing test**

Create `marketing/src/lib/schema.test.js`:
```js
import { describe, it, expect } from 'vitest';
import { organization, softwareApplication, faqPage, howTo, productOffers } from './components/schema.js';

describe('schema builders', () => {
  it('organization has required fields and no empty sameAs', () => {
    const o = organization();
    expect(o['@type']).toBe('Organization');
    expect(o.url).toBe('https://nels.money');
    expect('sameAs' in o).toBe(false); // omitted when no social links
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

  it('productOffers includes monthly and annual prices', () => {
    const p = productOffers();
    const names = p.offers.map((o) => o.name);
    expect(names).toContain('Monthly');
    expect(names).toContain('Annual');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test src/lib/schema.test.js`
Expected: FAIL (cannot resolve `schema.js`).

- [ ] **Step 3: Implement schema.js**

Create `marketing/src/lib/components/schema.js`:
```js
import { SITE } from '$lib/site.js';

export function organization() {
  const o = {
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
  };
  if (SITE.social.length) o.sameAs = SITE.social;
  return o;
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
  return {
    '@type': 'Product',
    name: `${SITE.name} subscription`,
    description: 'Full access to Nels: unlimited budgets, household sharing, AI insights, and goals.',
    offers: [
      { '@type': 'Offer', name: 'Monthly', price: SITE.price.monthly.replace('$', ''), priceCurrency: 'USD' },
      { '@type': 'Offer', name: 'Annual', price: SITE.price.annual.replace('$', ''), priceCurrency: 'USD' },
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test src/lib/schema.test.js`
Expected: PASS (5 tests).

- [ ] **Step 5: Implement JsonLd.svelte**

Create `marketing/src/lib/components/JsonLd.svelte`:
```svelte
<script>
  /** @type {{ data: object | object[] }} */
  let { data } = $props();
  const graph = $derived(Array.isArray(data) ? { '@context': 'https://schema.org', '@graph': data } : { '@context': 'https://schema.org', ...data });
  const json = $derived(JSON.stringify(graph));
</script>

<svelte:head>
  {@html `<script type="application/ld+json">${json}</` + `script>`}
</svelte:head>
```

- [ ] **Step 6: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/lib/components/schema.js marketing/src/lib/components/JsonLd.svelte marketing/src/lib/schema.test.js
git commit -m "feat(marketing): JSON-LD schema builders + component (tested)"
```

---

## Task 6: robots.txt + sitemap.xml endpoints (TDD)

**Files:**
- Create: `marketing/src/routes/robots.txt/+server.js`, `marketing/src/routes/sitemap.xml/+server.js`
- Test: `marketing/src/routes/sitemap.test.js`

- [ ] **Step 1: Write the failing test**

Create `marketing/src/routes/sitemap.test.js`:
```js
import { describe, it, expect } from 'vitest';
import { GET } from './sitemap.xml/+server.js';

describe('sitemap.xml', () => {
  it('lists the canonical home and pricing URLs', async () => {
    const res = await GET();
    const body = await res.text();
    expect(res.headers.get('content-type')).toContain('xml');
    expect(body).toContain('<loc>https://nels.money</loc>');
    expect(body).toContain('<loc>https://nels.money/pricing</loc>');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test src/routes/sitemap.test.js`
Expected: FAIL (cannot resolve `./sitemap.xml/+server.js`).

- [ ] **Step 3: Implement the sitemap endpoint**

Create `marketing/src/routes/sitemap.xml/+server.js`:
```js
import { SITE } from '$lib/site.js';

export const prerender = true;

const PATHS = ['/', '/features', '/how-it-works', '/pricing', '/security', '/compare', '/about', '/faq'];

export function GET() {
  const urls = PATHS.map(
    (p) => `  <url><loc>${SITE.url}${p === '/' ? '' : p}</loc><changefreq>weekly</changefreq></url>`
  ).join('\n');
  const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls}\n</urlset>`;
  return new Response(xml, { headers: { 'content-type': 'application/xml' } });
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test src/routes/sitemap.test.js`
Expected: PASS.

- [ ] **Step 5: Implement robots.txt**

Create `marketing/src/routes/robots.txt/+server.js`:
```js
import { SITE } from '$lib/site.js';

export const prerender = true;

export function GET() {
  const body = `User-agent: *\nAllow: /\n\nSitemap: ${SITE.url}/sitemap.xml\n`;
  return new Response(body, { headers: { 'content-type': 'text/plain' } });
}
```

- [ ] **Step 6: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/routes/robots.txt marketing/src/routes/sitemap.xml marketing/src/routes/sitemap.test.js
git commit -m "feat(marketing): sitemap.xml and robots.txt endpoints (tested)"
```

---

## Task 7: Shared layout — Nav, Footer, CTA, Section

**Files:**
- Create: `marketing/src/lib/components/Nav.svelte`, `Footer.svelte`, `Cta.svelte`, `Section.svelte`
- Create/Modify: `marketing/src/routes/+layout.svelte`

- [ ] **Step 1: CTA button**

Create `marketing/src/lib/components/Cta.svelte`:
```svelte
<script>
  import { SITE } from '$lib/site.js';
  /** @type {{ label?: string, classes?: string }} */
  let { label = 'Start free trial', classes = '' } = $props();
</script>

<a href={`${SITE.appUrl}`} class={`btn btn-primary ${classes}`}>{label}</a>
```

- [ ] **Step 2: Section wrapper**

Create `marketing/src/lib/components/Section.svelte`:
```svelte
<script>
  /** @type {{ id?: string, classes?: string, children?: import('svelte').Snippet }} */
  let { id, classes = '', children } = $props();
</script>

<section {id} class={`mx-auto w-full max-w-5xl px-4 py-16 ${classes}`}>
  {@render children?.()}
</section>
```

- [ ] **Step 3: Nav**

Create `marketing/src/lib/components/Nav.svelte`:
```svelte
<script>
  import { SITE, NAV } from '$lib/site.js';
  import Cta from './Cta.svelte';
</script>

<header class="navbar mx-auto max-w-5xl px-4">
  <div class="flex-1">
    <a href="/" class="text-xl font-bold text-primary">{SITE.name}</a>
  </div>
  <nav class="hidden gap-2 md:flex">
    {#each NAV as item}
      <a href={item.href} class="btn btn-ghost btn-sm">{item.label}</a>
    {/each}
  </nav>
  <div class="ml-2">
    <Cta label="Start free trial" classes="btn-sm" />
  </div>
</header>
```

- [ ] **Step 4: Footer**

Create `marketing/src/lib/components/Footer.svelte`:
```svelte
<script>
  import { SITE } from '$lib/site.js';
</script>

<footer class="footer mx-auto max-w-5xl gap-6 px-4 py-12 text-sm">
  <nav>
    <span class="footer-title">Product</span>
    <a href="/features" class="link link-hover">Features</a>
    <a href="/pricing" class="link link-hover">Pricing</a>
    <a href="/security" class="link link-hover">Security</a>
  </nav>
  <nav>
    <span class="footer-title">Company</span>
    <a href="/about" class="link link-hover">About</a>
    <a href="/faq" class="link link-hover">FAQ</a>
    <a href={`mailto:${SITE.supportEmail}`} class="link link-hover">Contact</a>
  </nav>
  <aside>
    <p>© {SITE.name}. Nels provides general budgeting tools, not financial advice.</p>
  </aside>
</footer>
```

- [ ] **Step 5: Layout wires Nav/Footer + site-wide schema + default Seo**

Replace `marketing/src/routes/+layout.svelte`:
```svelte
<script>
  import '../app.css';
  import Nav from '$lib/components/Nav.svelte';
  import Footer from '$lib/components/Footer.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { organization, softwareApplication } from '$lib/components/schema.js';
  let { children } = $props();
</script>

<JsonLd data={[organization(), softwareApplication()]} />
<Nav />
<main class="min-h-screen">
  {@render children?.()}
</main>
<Footer />
```

- [ ] **Step 6: Verify build**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm build`
Expected: build succeeds; routes prerender.

- [ ] **Step 7: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/lib/components marketing/src/routes/+layout.svelte
git commit -m "feat(marketing): shared layout, nav, footer, CTA"
```

---

## Task 8: Home page

**Files:**
- Modify: `marketing/src/routes/+page.svelte`

Copy is finalized from the strategy doc (hero option #1), honesty-corrected (no data-export/testimonial claims).

- [ ] **Step 1: Implement the home page**

Replace `marketing/src/routes/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { faqPage } from '$lib/components/schema.js';
  import { SITE } from '$lib/site.js';

  const features = [
    { title: 'Set budgets your way', body: 'Create one budget or many — household, side income, annual goals. Set limits by category and time frame. The AI helps you build a realistic budget, then share it with your partner or advisor.' },
    { title: 'Just tell the AI', body: 'Text "spent $25 on gas yesterday" and Nels categorizes it automatically. Get alerts at 80% and 100% of a limit, plus daily, weekly, or monthly reminders.' },
    { title: 'Understand your money', body: 'Ask "how much did I spend on dining out this month?" Nels gives category breakdowns, trends, top transactions, and a plain-English narrative of where your money goes.' },
    { title: 'Save together, celebrate together', body: 'Set savings or debt-payoff goals, track progress in real time, and celebrate milestones — building motivation, not guilt. Share goals so everyone pulls the same direction.' },
  ];

  const faqs = [
    { q: 'Do you give financial or investment advice?', a: 'No. Nels offers personalized spending insights based on your data, but it is not a registered financial advisor.' },
    { q: 'Is my data safe?', a: 'Yes. Nels uses passwordless authenticator-app sign-in and encrypts data in transit. We do not sell your data.' },
    { q: 'Can I share a budget with my partner or family?', a: 'Yes — invite them by email with view, edit, or owner permissions. Every change is recorded in an audit log.' },
    { q: 'How does the free trial work?', a: `Try Nels free for ${SITE.trialDays} days, no credit card required. After that it's ${SITE.price.monthly}/mo or ${SITE.price.annual}/yr.` },
  ];
</script>

<Seo path="/" description={SITE.description} />
<JsonLd data={faqPage(faqs)} />

<Section classes="text-center">
  <h1 class="text-5xl font-bold tracking-tight">Chat your way to financial clarity</h1>
  <p class="mx-auto mt-4 max-w-2xl text-lg opacity-80">
    Budget smarter with AI that understands you — no passwords, no spreadsheets, no jargon.
  </p>
  <div class="mt-8 flex justify-center gap-3">
    <Cta label={`Try free for ${SITE.trialDays} days`} />
    <a href="/how-it-works" class="btn btn-ghost">See how it works</a>
  </div>
  <p class="mt-4 text-sm opacity-70">{SITE.trialDays}-day free trial · No credit card · Passwordless sign-in</p>
</Section>

<Section id="why" classes="text-center">
  <h2 class="text-3xl font-bold">Why most people give up on budgeting</h2>
  <div class="mt-8 grid gap-6 md:grid-cols-3">
    <div class="card bg-base-200 p-6"><h3 class="font-semibold">Forms are tedious</h3><p class="mt-2 text-sm opacity-80">Logging each transaction into categories feels like data entry. By month two, you've stopped.</p></div>
    <div class="card bg-base-200 p-6"><h3 class="font-semibold">Spreadsheets don't collaborate</h3><p class="mt-2 text-sm opacity-80">Conflicting edits, no audit trail, confusion about who changed what. Partners give up.</p></div>
    <div class="card bg-base-200 p-6"><h3 class="font-semibold">Apps treat you like a stranger</h3><p class="mt-2 text-sm opacity-80">Most software ignores what you told it last year and can't give you personalized insight.</p></div>
  </div>
</Section>

<Section id="features">
  <h2 class="text-center text-3xl font-bold">Everything you need to budget together</h2>
  <div class="mt-8 grid gap-6 md:grid-cols-2">
    {#each features as f}
      <div class="card bg-base-200 p-6">
        <h3 class="text-xl font-semibold text-primary">{f.title}</h3>
        <p class="mt-2 opacity-80">{f.body}</p>
      </div>
    {/each}
  </div>
</Section>

<Section id="security" classes="text-center">
  <h2 class="text-3xl font-bold">Built on trust. Secured by standards.</h2>
  <p class="mx-auto mt-4 max-w-2xl opacity-80">
    Sign in with passwordless TOTP — the same standard banks use — via Google Authenticator, 1Password, Authy, or any standard app. No passwords to steal. Your data is encrypted in transit, and we never sell it.
  </p>
  <a href="/security" class="btn btn-ghost mt-6">Learn more about security</a>
</Section>

<Section id="pricing" classes="text-center">
  <h2 class="text-3xl font-bold">Simple pricing, no surprises</h2>
  <p class="mx-auto mt-3 max-w-xl opacity-80">Free for {SITE.trialDays} days, then {SITE.price.monthly}/mo or {SITE.price.annual}/yr. Cancel anytime.</p>
  <a href="/pricing" class="btn btn-ghost mt-4">See pricing details</a>
</Section>

<Section id="faq">
  <h2 class="text-center text-3xl font-bold">Frequently asked</h2>
  <div class="mt-8 space-y-3">
    {#each faqs as item}
      <div class="collapse-arrow collapse bg-base-200">
        <input type="checkbox" />
        <div class="collapse-title font-medium">{item.q}</div>
        <div class="collapse-content text-sm opacity-80"><p>{item.a}</p></div>
      </div>
    {/each}
  </div>
</Section>

<Section classes="text-center">
  <h2 class="text-3xl font-bold">Ready to budget smarter?</h2>
  <p class="mt-3 opacity-80">Start your free {SITE.trialDays}-day trial. No credit card. No passwords.</p>
  <div class="mt-6"><Cta /></div>
</Section>
```

- [ ] **Step 2: Verify**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm build`
Expected: build succeeds, `/` prerenders. Optionally `pnpm preview` and eyeball at `http://localhost:4173`.

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/routes/+page.svelte
git commit -m "feat(marketing): home page"
```

---

## Task 9: Features page

**Files:**
- Create: `marketing/src/routes/features/+page.svelte`

- [ ] **Step 1: Implement**

Create `marketing/src/routes/features/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';

  const pillars = [
    { h: 'Budget Management', sub: 'Set it up your way', body: 'Create one budget or many for household, freelance income, or annual savings. Set limits by category and time frame. Share with partner, accountant, or advisor with view/edit/owner permissions and a full audit log.' },
    { h: 'Conversational Expense Tracking', sub: 'Just tell the AI', body: 'Type what you spent and Nels categorizes it automatically. Real-time alerts at 80% and 100% of a budget limit, plus daily, weekly, or monthly reminders to stay on track.' },
    { h: 'Analysis & Insights', sub: 'Understand your money', body: 'Category breakdowns, spending trends, top transactions, and an AI-written narrative of your patterns — with semantic memory of past conversations for context-aware answers.' },
    { h: 'Goal Setting & Celebration', sub: 'Save together', body: 'Define savings and debt-payoff goals, track progress in real time, and celebrate milestones. Share goals with family so everyone pulls together.' },
  ];
</script>

<Seo title="Features" path="/features" description="Manage budgets, share with family, track expenses conversationally, and get AI-powered insights. No passwords. No spreadsheets." />

<Section classes="text-center">
  <h1 class="text-4xl font-bold">Everything Nels does</h1>
  <p class="mx-auto mt-4 max-w-2xl opacity-80">Four pillars, one conversational co-pilot.</p>
</Section>

<Section>
  <div class="space-y-6">
    {#each pillars as p}
      <div class="card bg-base-200 p-8">
        <p class="text-sm uppercase tracking-wide text-primary">{p.sub}</p>
        <h2 class="mt-1 text-2xl font-semibold">{p.h}</h2>
        <p class="mt-3 opacity-80">{p.body}</p>
      </div>
    {/each}
  </div>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
```

- [ ] **Step 2: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/routes/features
git commit -m "feat(marketing): features page"
```

---

## Task 10: How It Works page (with HowTo schema)

**Files:**
- Create: `marketing/src/routes/how-it-works/+page.svelte`

- [ ] **Step 1: Implement**

Create `marketing/src/routes/how-it-works/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { howTo } from '$lib/components/schema.js';

  const steps = [
    { name: 'Sign up passwordlessly', text: 'Create your account with an authenticator app — no passwords to set or remember.' },
    { name: 'Create budgets and share', text: 'Set up one or more budgets and invite family with view, edit, or owner permissions.' },
    { name: 'Chat to log expenses', text: 'Tell Nels what you spent and it categorizes everything automatically.' },
    { name: 'Get AI insights', text: 'See spending trends, category breakdowns, and progress toward your goals.' },
  ];
</script>

<Seo title="How it works" path="/how-it-works" description="See how Nels makes budgeting easy: sign up passwordlessly, create budgets, chat to log expenses, and get AI insights." />
<JsonLd data={howTo(steps)} />

<Section classes="text-center">
  <h1 class="text-4xl font-bold">How Nels works</h1>
  <p class="mx-auto mt-4 max-w-2xl opacity-80">Four steps from sign-up to insight.</p>
</Section>

<Section>
  <ol class="space-y-6">
    {#each steps as s, i}
      <li class="flex gap-4">
        <span class="flex h-10 w-10 flex-none items-center justify-center rounded-full bg-primary font-bold text-primary-content">{i + 1}</span>
        <div><h2 class="text-xl font-semibold">{s.name}</h2><p class="mt-1 opacity-80">{s.text}</p></div>
      </li>
    {/each}
  </ol>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
```

- [ ] **Step 2: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/routes/how-it-works
git commit -m "feat(marketing): how-it-works page with HowTo schema"
```

---

## Task 11: Pricing page (single plan, trial-then-paywall, Product schema)

**Files:**
- Create: `marketing/src/routes/pricing/+page.svelte`

- [ ] **Step 1: Implement**

Create `marketing/src/routes/pricing/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { productOffers } from '$lib/components/schema.js';
  import { SITE } from '$lib/site.js';

  const included = [
    'Unlimited budgets',
    'Conversational expense tracking & auto-categorization',
    'Household sharing with view/edit/owner permissions + audit log',
    'AI insights, reports, and spending trends',
    'Savings & debt-payoff goals with milestone tracking',
    'Passwordless authenticator-app sign-in',
  ];
</script>

<Seo title="Pricing" path="/pricing" description={`Free for ${SITE.trialDays} days, then ${SITE.price.monthly}/mo or ${SITE.price.annual}/yr. No credit card to start. Cancel anytime.`} />
<JsonLd data={productOffers()} />

<Section classes="text-center">
  <h1 class="text-4xl font-bold">One simple plan</h1>
  <p class="mx-auto mt-4 max-w-xl opacity-80">Free for {SITE.trialDays} days, then choose monthly or annual. No credit card to start. Cancel anytime.</p>

  <div class="mx-auto mt-10 grid max-w-3xl gap-6 md:grid-cols-2">
    <div class="card border border-base-300 bg-base-100 p-8">
      <h2 class="text-xl font-semibold">Monthly</h2>
      <p class="mt-2 text-4xl font-bold">{SITE.price.monthly}<span class="text-base font-normal opacity-70">/mo</span></p>
      <div class="mt-6"><Cta /></div>
    </div>
    <div class="card border-2 border-primary bg-base-100 p-8">
      <div class="badge badge-primary">Best value</div>
      <h2 class="mt-2 text-xl font-semibold">Annual</h2>
      <p class="mt-2 text-4xl font-bold">{SITE.price.annual}<span class="text-base font-normal opacity-70">/yr</span></p>
      <p class="mt-1 text-sm opacity-70">≈ 2 months free</p>
      <div class="mt-6"><Cta /></div>
    </div>
  </div>
</Section>

<Section classes="max-w-2xl">
  <h2 class="text-center text-2xl font-bold">Everything's included</h2>
  <ul class="mt-6 space-y-2">
    {#each included as item}
      <li class="flex gap-2"><span class="text-primary">✓</span><span class="opacity-80">{item}</span></li>
    {/each}
  </ul>
  <p class="mt-6 text-center text-sm opacity-70">{SITE.trialDays}-day free trial · No credit card required · Passwordless sign-in</p>
</Section>
```

- [ ] **Step 2: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/routes/pricing
git commit -m "feat(marketing): pricing page (single plan, trial-then-paywall)"
```

---

## Task 12: Security, Compare, About, FAQ pages

**Files:**
- Create: `marketing/src/routes/security/+page.svelte`, `compare/+page.svelte`, `about/+page.svelte`, `faq/+page.svelte`

- [ ] **Step 1: Security page**

Create `marketing/src/routes/security/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
</script>

<Seo title="Security & Privacy" path="/security" description="Nels uses passwordless TOTP authenticator-app sign-in and encrypts data in transit. Your financial data stays private." />

<Section classes="max-w-2xl">
  <h1 class="text-4xl font-bold">Passwordless and private by design</h1>
  <div class="prose mt-6 max-w-none opacity-90">
    <h2>Passwordless sign-in</h2>
    <p>Nels uses TOTP (time-based one-time passwords) — the same standard banks and cloud providers use. Sign in with Google Authenticator, 1Password, Authy, or any standard authenticator app. There are no passwords to forget or to leak in a breach.</p>
    <h2>Your data</h2>
    <p>Your budget data is encrypted in transit (HTTPS everywhere). We never sell your data and we don't build advertising profiles. Your financial conversations stay between you and Nels.</p>
    <h2>Collaboration you can audit</h2>
    <p>Share budgets with view, edit, or owner permissions. Every change is recorded in an audit log, so there are no surprises about who changed what.</p>
  </div>
  <div class="mt-8"><Cta /></div>
</Section>
```
(Honesty: states "encrypted in transit" only; do not add "at rest" unless confirmed for Fly MPG.)

- [ ] **Step 2: About page**

Create `marketing/src/routes/about/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
</script>

<Seo title="About" path="/about" description="Nels helps people manage money conversationally, securely, and together." />

<Section classes="max-w-2xl">
  <h1 class="text-4xl font-bold">About Nels</h1>
  <p class="mt-6 opacity-90">Nels exists to make budgeting feel modern: manage money conversationally, securely, and together. We're privacy-first, we keep the interface human, and we're honest about what the product can and can't do today — our insights get better over time.</p>
</Section>
```

- [ ] **Step 3: Compare page**

Create `marketing/src/routes/compare/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';

  const rows = [
    { feature: 'Conversational expense logging', nels: 'Yes', others: 'Rare' },
    { feature: 'Passwordless sign-in (TOTP)', nels: 'Yes', others: 'No' },
    { feature: 'Household sharing with audit log', nels: 'Yes', others: 'Limited' },
    { feature: 'AI spending narrative', nels: 'Yes', others: 'Varies' },
  ];
</script>

<Seo title="Compare" path="/compare" description="An honest look at how Nels' conversational, passwordless budgeting compares to traditional apps and spreadsheets." />

<Section>
  <h1 class="text-center text-4xl font-bold">How Nels compares</h1>
  <p class="mx-auto mt-4 max-w-2xl text-center opacity-80">An honest comparison. We're newer, so some insights will keep improving — but the fundamentals are here today.</p>
  <div class="mt-8 overflow-x-auto">
    <table class="table">
      <thead><tr><th>Feature</th><th>Nels</th><th>Traditional apps</th></tr></thead>
      <tbody>
        {#each rows as r}<tr><td>{r.feature}</td><td class="font-semibold text-primary">{r.nels}</td><td class="opacity-70">{r.others}</td></tr>{/each}
      </tbody>
    </table>
  </div>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
```

- [ ] **Step 4: FAQ page (with FAQPage schema)**

Create `marketing/src/routes/faq/+page.svelte`:
```svelte
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { faqPage } from '$lib/components/schema.js';
  import { SITE } from '$lib/site.js';

  const faqs = [
    { q: 'What is Nels?', a: 'A conversational AI budgeting co-pilot: set budgets, log expenses by chatting, analyze spending, and track goals.' },
    { q: 'Do you give financial or investment advice?', a: 'No. Nels offers personalized spending insights based on your data, but it is not a registered financial advisor.' },
    { q: 'Is my data safe?', a: 'Yes. Nels uses passwordless authenticator-app sign-in and encrypts data in transit. We do not sell your data.' },
    { q: 'Can I share my budget?', a: 'Yes — invite people by email with view, edit, or owner permissions. Every change is recorded in an audit log.' },
    { q: 'How does the free trial work?', a: `Try Nels free for ${SITE.trialDays} days with no credit card. After that it's ${SITE.price.monthly}/mo or ${SITE.price.annual}/yr, and you can cancel anytime.` },
  ];
</script>

<Seo title="FAQ" path="/faq" description="Common questions about Nels: pricing, security, sharing, and the free trial." />
<JsonLd data={faqPage(faqs)} />

<Section classes="max-w-2xl">
  <h1 class="text-center text-4xl font-bold">Frequently asked questions</h1>
  <div class="mt-8 space-y-3">
    {#each faqs as item}
      <div class="collapse-arrow collapse bg-base-200">
        <input type="checkbox" />
        <div class="collapse-title font-medium">{item.q}</div>
        <div class="collapse-content text-sm opacity-80"><p>{item.a}</p></div>
      </div>
    {/each}
  </div>
</Section>
```

- [ ] **Step 5: Verify all pages build**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm build`
Expected: build succeeds; all 8 routes prerender, plus `sitemap.xml` and `robots.txt`.

- [ ] **Step 6: Commit**

```bash
cd /home/robhicks/dev/nels
git add marketing/src/routes/security marketing/src/routes/compare marketing/src/routes/about marketing/src/routes/faq
git commit -m "feat(marketing): security, compare, about, and FAQ pages"
```

---

## Task 13: Full test + build gate

**Files:** none (verification)

- [ ] **Step 1: Run the unit tests**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm test`
Expected: all tests PASS (Seo, schema, sitemap).

- [ ] **Step 2: Production build**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm build`
Expected: success. Confirm the output dir is `.svelte-kit/cloudflare` (adapter-cloudflare default) — note this path for the deploy workflow.

- [ ] **Step 3: Preview and spot-check**

Run: `cd /home/robhicks/dev/nels/marketing && pnpm preview`
Open `http://localhost:4173`, click through nav, confirm CTA links to `https://app.nels.money`, view-source shows `<title>`, meta description, canonical, and JSON-LD. Stop the server.

---

## Task 14: Deploy — Cloudflare Pages project + GitHub Actions

**Files:**
- Create: `.github/workflows/deploy-marketing.yml`

- [ ] **Step 1: Create the Pages project (one-time)**

Run (uses the existing `CLOUDFLARE_API_TOKEN`/`CLOUDFLARE_ACCOUNT_ID` env or `wrangler login`):
```bash
cd /home/robhicks/dev/nels/marketing
pnpm dlx wrangler@3 pages project create nels-money --production-branch=main
```
Expected: project `nels-money` created. (If it exists, this is a no-op-ish error — fine.)

- [ ] **Step 2: Add the deploy workflow**

Create `.github/workflows/deploy-marketing.yml`:
```yaml
name: Deploy marketing

on:
  push:
    branches: [main]
    paths:
      - 'marketing/**'
      - '.github/workflows/deploy-marketing.yml'
  workflow_dispatch:

concurrency:
  group: deploy-marketing
  cancel-in-progress: true

jobs:
  deploy:
    runs-on: ubuntu-latest
    if: github.ref == 'refs/heads/main'
    defaults:
      run:
        working-directory: marketing
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
        with:
          version: 10
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
          cache-dependency-path: marketing/pnpm-lock.yaml
      - name: Install dependencies
        run: pnpm install --frozen-lockfile
      - name: Build
        run: pnpm build
      - name: Ensure Pages project exists
        env:
          CLOUDFLARE_API_TOKEN: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          CLOUDFLARE_ACCOUNT_ID: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
        run: |
          if ! pnpm dlx wrangler@3 pages project list | grep -qw nels-money; then
            pnpm dlx wrangler@3 pages project create nels-money --production-branch=main
          fi
      - name: Deploy to Cloudflare Pages
        uses: cloudflare/wrangler-action@v3
        with:
          apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
          workingDirectory: marketing
          command: pages deploy .svelte-kit/cloudflare --project-name=nels-money --branch=main
```

- [ ] **Step 3: Commit and push**

```bash
cd /home/robhicks/dev/nels
git add .github/workflows/deploy-marketing.yml
git commit -m "ci(marketing): deploy marketing site to Cloudflare Pages"
git push origin main
```

- [ ] **Step 4: Watch the run**

Run: `gh run list --repo savvagent/nels --workflow=deploy-marketing.yml --limit 1`
Then watch it; expected conclusion: success, with a `*.nels-money.pages.dev` URL in the logs.

- [ ] **Step 5: Attach the custom domain (one-time, dashboard or CLI)**

In Cloudflare → Pages → `nels-money` → Custom domains → add `nels.money` (apex). Since the zone is on Cloudflare, DNS is wired automatically. Verify `https://nels.money` serves the site.

---

## Done / handoff to Phase 2

When this plan is complete: the marketing site is live at `nels.money`, fully prerendered, SEO-instrumented (meta, OG, canonical, JSON-LD, sitemap, robots), with the CTA pointing at `app.nels.money`. **Phase 2 (Stripe subscriptions)** is a separate plan: it adds the billing endpoints + webhook + entitlement gate to the Rust backend and repoints the CTA at a Checkout session. Also pending (cross-cutting, Phase 2): attach `app.nels.money` to the existing `nels` Pages project and add both `nels.money` origins to the backend CORS allowlist.

> **Note on blog/SEO content (Phase 3):** the `/blog` routes and content clusters from `docs/marketing/nels-seo-strategy.md` are deliberately out of scope here.
