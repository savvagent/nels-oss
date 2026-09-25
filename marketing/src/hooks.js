// Universal `reroute` hook: internally map the bare apex `/` onto the English
// locale route so local dev (and `vite preview`) render real content instead of
// 404ing. All pages live under [lang]/, so `/` matches no route on its own.
//
// In production this never fires for `/`: Cloudflare Pages serves the `_redirects`
// 302 (`/  /en`) before SvelteKit runs, so the apex still issues a real redirect
// that changes the URL to /en. This hook only covers the dev server, which does
// not process `_redirects`.
import { FALLBACK } from '$lib/i18n/index.js';

/** @type {import('@sveltejs/kit').Reroute} */
export function reroute({ url }) {
  if (url.pathname === '/') {
    return `/${FALLBACK}`;
  }
}
