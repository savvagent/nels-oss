import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'
import { initPwa } from './lib/pwa.svelte.js'

// Register the service worker and wire up update detection (see pwa.svelte.js).
initPwa();

const app = mount(App, {
  target: document.getElementById('app'),
})

export default app
