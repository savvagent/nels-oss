# Nels marketing site

The public marketing website for Nels (https://nels.money), built with SvelteKit 2 + Svelte 5, Tailwind v4, and daisyUI, fully prerendered to static HTML and deployed to Cloudflare Pages.

## Develop
```bash
pnpm install
pnpm dev
```

## Build & test
```bash
pnpm build     # static output in .svelte-kit/cloudflare
pnpm test      # vitest
pnpm check     # svelte-check
```

Deployment is automated via `.github/workflows/deploy-marketing.yml` (deploys `main` to the `nels-money` Cloudflare Pages project). Site copy/SEO strategy: `../docs/marketing/nels-seo-strategy.md`.
