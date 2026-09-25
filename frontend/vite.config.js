import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync, existsSync } from 'node:fs'

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url)))

// Short commit SHA for the build, injected so the UI can show an exact version
// for support and stamped into the service-worker CACHE_NAME (below). Falls back
// to "dev" when git isn't available (e.g. some CI shallow contexts) so the build
// never fails on this. A fixed --short=12 length keeps the stamp deterministic
// as history grows (bare --short can change abbreviation length). execFileSync
// (no shell) with a fixed argument list — no interpolation, no injection surface.
let commitSha = 'dev'
try {
  commitSha = execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], { encoding: 'utf8' }).trim()
} catch {
  // no git — keep "dev"
}

// Stamp the per-build commit SHA into the emitted dist/sw.js, replacing the
// __BUILD_HASH__ placeholder in CACHE_NAME. A code-only deploy never byte-changes
// public/sw.js, so browsers never detect a new service worker; stamping the SHA
// makes the worker byte-change every release, triggering update detection.
function stampServiceWorker(sha) {
  return {
    name: 'stamp-sw',
    apply: 'build',
    closeBundle() {
      const swUrl = new URL('./dist/sw.js', import.meta.url)
      if (!existsSync(swUrl)) {
        throw new Error('stamp-sw: dist/sw.js not found after build')
      }
      const src = readFileSync(swUrl, 'utf8')
      if (!src.includes('__BUILD_HASH__')) {
        throw new Error('stamp-sw: __BUILD_HASH__ placeholder not found in dist/sw.js')
      }
      if (sha === 'dev') {
        throw new Error(
          'stamp-sw: commit SHA unavailable ("dev" fallback) — refusing to stamp a non-unique service-worker CACHE_NAME. Build from a git checkout so deploys are byte-distinct.'
        )
      }
      writeFileSync(swUrl, src.replaceAll('__BUILD_HASH__', sha))
    },
  }
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [tailwindcss(), svelte(), stampServiceWorker(commitSha)],
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
    __BUILD_SHA__: JSON.stringify(commitSha),
  },
  server: {
    port: 5173,
    host: true, // Listen on all interfaces (LAN access to the dev server)
  }
})
