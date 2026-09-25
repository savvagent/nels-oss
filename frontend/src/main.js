import "./lib/i18n/index.js";
// Self-hosted variable Inter (bundled locally by Vite — no external/Google
// Fonts request, which is also a privacy win for a finance app).
import "@fontsource-variable/inter";
import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'
import { initPwa } from './lib/pwa.svelte.js'
import { canonicalRedirect } from './lib/canonicalHost.js'

const redirectTo = canonicalRedirect(window.location);

let app;
if (redirectTo) {
  window.location.replace(redirectTo);
} else {
  // Register the service worker and wire up update detection (see pwa.svelte.js).
  initPwa();

  app = mount(App, {
    target: document.getElementById('app'),
  })
}

export default app
