<script>
  import '../app.css';
  import Nav from '$lib/components/Nav.svelte';
  import Footer from '$lib/components/Footer.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { organization, softwareApplication } from '$lib/components/schema.js';
  let { children } = $props();
</script>

<JsonLd data={[organization(), softwareApplication()]} />
<Nav />
<main class="min-h-screen">
  {@render children?.()}
</main>
<Footer />
