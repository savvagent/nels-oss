<script>
  /** @type {{ data: object | object[] }} */
  let { data } = $props();
  const graph = $derived(Array.isArray(data) ? { '@context': 'https://schema.org', '@graph': data } : { '@context': 'https://schema.org', ...data });
  // Escape the less-than char so app data containing a closing script tag can't break out of the inline tag.
  const LT = String.fromCharCode(60);
  const json = $derived(JSON.stringify(graph).replaceAll(LT, '\\u003c'));
</script>

<svelte:head>
  {@html `<script type="application/ld+json">${json}</` + `script>`}
</svelte:head>
