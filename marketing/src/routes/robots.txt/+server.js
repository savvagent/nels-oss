import { SITE } from '$lib/site.js';

export const prerender = true;

export function GET() {
  const body = `User-agent: *\nAllow: /\n\nSitemap: ${SITE.url}/sitemap.xml\n`;
  return new Response(body, { headers: { 'content-type': 'text/plain' } });
}
