<script>
  /** @type {{ id?: string, classes?: string, children?: import('svelte').Snippet }} */
  let { id, classes = '', children } = $props();
</script>

<section {id} class={`mx-auto w-full max-w-5xl px-4 py-16 ${classes}`}>
  {@render children?.()}
</section>
