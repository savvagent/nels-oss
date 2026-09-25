import { readFileSync, existsSync } from 'node:fs';

const ROUTES_PATH = '.svelte-kit/cloudflare/_routes.json';

if (!existsSync(ROUTES_PATH)) {
  process.stderr.write(
    `ERROR: ${ROUTES_PATH} not found. Did the adapter run?\n`
  );
  process.exit(1);
}

let routes;
try {
  routes = JSON.parse(readFileSync(ROUTES_PATH, 'utf8'));
} catch (err) {
  process.stderr.write(
    `ERROR: failed to parse ${ROUTES_PATH}: ${err.message}\n`
  );
  process.exit(1);
}

const { include = [], exclude = [] } = routes;
if (!Array.isArray(include) || !Array.isArray(exclude)) {
  process.stderr.write(
    `ERROR: ${ROUTES_PATH} has unexpected format (include/exclude must be arrays)\n`
  );
  process.exit(1);
}

const total = include.length + exclude.length;

// Cloudflare's limit is 100 total include+exclude rules. The adapter starts
// dropping exclude rules when total > 100, so treat > 100 as a build failure.
if (total > 100) {
  process.stderr.write(
    `ERROR: ${ROUTES_PATH} has ${total} total rules (limit: 100). ` +
    'Add wildcard excludes or reduce the route count.\n'
  );
  process.exit(1);
}

process.stdout.write(
  `_routes.json: ${total} total rules (OK, limit: 100)\n`
);
