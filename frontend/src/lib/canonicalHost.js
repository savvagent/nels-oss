// Passkeys are bound to the RP ID `nels.money` (nels#560), so they can't be
// used from the Cloudflare Pages alias `nels.pages.dev`. Send visitors there
// to the canonical app domain instead of letting sign-in fail. Preview
// deployments (`<hash>.nels.pages.dev`) are left alone.
export const CANONICAL_ORIGIN = 'https://app.nels.money';
const LEGACY_HOST = 'nels.pages.dev';

/** Returns the URL to redirect to, or null if already on an allowed host. */
export function canonicalRedirect({ hostname, pathname, search, hash }) {
  if (hostname !== LEGACY_HOST) return null;
  return `${CANONICAL_ORIGIN}${pathname}${search}${hash}`;
}
