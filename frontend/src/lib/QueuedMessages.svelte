<script>
  // "N queued" indicator + preview/cancel popover for messages queued while Nels is generating
  // (#230). Mirrors BudgetStatusInfo.svelte's click-toggle / outside-click / Escape-close pattern
  // — the same repo idiom, not a new one. Renders nothing when the queue is empty. Purely
  // presentational: all queue mutation goes back through the two callback props so App.svelte
  // stays the single owner of `pendingQueue` state.
  import { X } from "lucide-svelte";
  import { _ } from "svelte-i18n";

  let { queue = [], onRemove = () => {}, onClearAll = () => {} } = $props();

  let open = $state(false);
  let panelEl = $state(null);
  let buttonEl = $state(null);

  // The popover markup is gated behind `{#if queue.length > 0}` below, so nothing renders while
  // the queue is empty — but `open` itself doesn't reset on its own. Without this, clearing the
  // queue (or draining the last item) while the popover was open leaves `open === true`, and the
  // next time the queue refills the popover would render already-open with no click.
  $effect(() => {
    if (queue.length === 0) open = false;
  });

  function toggle() {
    open = !open;
  }

  function close() {
    open = false;
  }

  function onWindowClick(e) {
    if (!open) return;
    if (panelEl?.contains(e.target) || buttonEl?.contains(e.target)) return;
    close();
  }

  function onWindowKeydown(e) {
    if (e.key === "Escape" && open) close();
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onWindowKeydown} />

{#if queue.length > 0}
  <div class="relative">
    <button
      bind:this={buttonEl}
      type="button"
      onclick={toggle}
      aria-haspopup="true"
      aria-expanded={open}
      aria-label={$_("chat.queueToggleAria")}
      class="badge badge-sm badge-neutral gap-1 cursor-pointer"
    >
      {$_("chat.queuedCount", { values: { count: queue.length } })}
    </button>

    {#if open}
      <div
        bind:this={panelEl}
        role="dialog"
        aria-label={$_("chat.queueToggleAria")}
        class="absolute bottom-full right-0 mb-2 z-50 w-72 max-w-[calc(100vw-1.5rem)]
               rounded-xl bg-base-200 border border-base-300 shadow-2xl p-3"
      >
        <ul class="flex flex-col gap-1.5 max-h-56 overflow-y-auto">
          {#each queue as item (item.id)}
            <li class="flex items-start gap-2">
              <span class="flex-1 text-xs text-base-content/80 line-clamp-2">{item.text}</span>
              <button
                type="button"
                onclick={() => onRemove(item.id)}
                aria-label={$_("chat.queueRemoveAria")}
                class="shrink-0 text-base-content/50 hover:text-base-content"
              >
                <X class="w-3.5 h-3.5" />
              </button>
            </li>
          {/each}
        </ul>
        <button
          type="button"
          onclick={onClearAll}
          class="btn btn-ghost btn-xs w-full mt-2 text-base-content/70"
        >
          {$_("chat.queueClearAll")}
        </button>
      </div>
    {/if}
  </div>
{/if}
