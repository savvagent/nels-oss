// Set <html lang> per request/prerendered page from the leading URL segment.
// app.html ships a literal `lang="%lang%"` placeholder; we resolve it here so
// each prerendered locale gets the correct language attribute baked in. Pages
// outside the [lang] tree (the English-only legal pages) default to "en".
//
// The supported locales come from the single source of truth in lib/i18n so a
// newly added locale can't silently fall through to the English default here.
import { LOCALES, FALLBACK } from '$lib/i18n/index.js';

const LOCALE_CODES = new Set(LOCALES.map((l) => l.code));

export async function handle({ event, resolve }) {
  const seg = event.url.pathname.split('/')[1];
  const lang = LOCALE_CODES.has(seg) ? seg : FALLBACK;
  return resolve(event, {
    transformPageChunk: ({ html }) => html.replace('%lang%', lang),
  });
}
