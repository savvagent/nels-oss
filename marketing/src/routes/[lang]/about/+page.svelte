<script>
  import { _ } from 'svelte-i18n';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';

  let { data } = $props();
</script>

<Seo title={$_('about.seo.title')} path="/about" lang={data.lang} description={$_('about.seo.description')} />

<Section classes="max-w-2xl">
  <h1 class="text-4xl font-bold">{$_('about.title')}</h1>
  <p class="mt-6 opacity-90">{$_('about.body')}</p>
</Section>
