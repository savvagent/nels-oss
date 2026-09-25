<script>
  import { _, locale } from 'svelte-i18n';
  import { SITE } from '$lib/site.js';

  // Product/Company links are localized; legal pages stay English-only top-level.
  let lang = $derived(($locale || 'en').slice(0, 2));
</script>

<footer class="footer mx-auto max-w-5xl gap-6 px-4 py-12 text-sm">
  <nav>
    <span class="footer-title">{$_('footer.product')}</span>
    <a href={`/${lang}/features`} class="link link-hover">{$_('footer.features')}</a>
    <a href={`/${lang}/pricing`} class="link link-hover">{$_('footer.pricing')}</a>
    <a href={`/${lang}/security`} class="link link-hover">{$_('footer.security')}</a>
    <a href={`/${lang}/compare`} class="link link-hover">{$_('footer.compare')}</a>
  </nav>
  <nav>
    <span class="footer-title">{$_('footer.company')}</span>
    <a href={`/${lang}/about`} class="link link-hover">{$_('footer.about')}</a>
    <a href={`/${lang}/faq`} class="link link-hover">{$_('footer.faq')}</a>
    <a href={`mailto:${SITE.supportEmail}`} class="link link-hover">{$_('footer.contact')}</a>
  </nav>
  <nav>
    <span class="footer-title">{$_('footer.legal')}</span>
    <a href="/privacy" class="link link-hover">{$_('footer.privacy')}</a>
    <a href="/terms" class="link link-hover">{$_('footer.terms')}</a>
  </nav>
  <aside>
    <p>{$_('footer.rights')}</p>
  </aside>
</footer>
