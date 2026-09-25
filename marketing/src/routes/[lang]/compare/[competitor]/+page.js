import { error } from '@sveltejs/kit';
import { findCompetitor } from '$lib/compare/competitors.js';

// `prerender` and `trailingSlash` are inherited from [lang]/+layout.js.
export async function load({ params, parent }) {
  const { lang } = await parent();
  const competitor = findCompetitor(params.competitor);
  if (!competitor) throw error(404, `Unknown competitor: ${params.competitor}`);
  return { lang, competitor };
}
