<script>
  import { _ } from 'svelte-i18n';
  import { SITE } from '$lib/site.js';
  /** @type {{ label?: string, classes?: string, upgrade?: 'monthly' | 'annual', plan?: 'basic' | 'pro', cadence?: 'monthly' | 'annual' }} */
  let { label, classes = '', upgrade, plan, cadence } = $props();
  // Default to the translated "Start free trial" when no explicit label is given.
  const text = $derived(label ?? $_('cta.startTrial'));
  // Deep-link into the app's upgrade flow (#25). The prerendered marketing site
  // can't know auth state, so the app reads these params after login and starts
  // checkout. Two-tier: `plan` (+ optional `cadence`) selects the tier; the legacy
  // cadence-only `upgrade` is kept for existing CTAs (app defaults tier to Pro).
  // Plain link when neither is given.
  const href = $derived(
    plan
      ? `${SITE.appUrl}?plan=${plan}&cadence=${cadence ?? 'monthly'}`
      : upgrade
        ? `${SITE.appUrl}?upgrade=${upgrade}`
        : SITE.appUrl,
  );
</script>

<a {href} class={`btn btn-primary ${classes}`}>{text}</a>
