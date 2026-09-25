<script>
  // Single info affordance for the chat header's active-budget status flags
  // (#232) — replaces the row of Project/Fixed/Closed/Archived/Rollover/
  // Auto-renew/Rollup/Rolled-up badges with one icon that opens a popover
  // listing whichever of those are currently active. Click-toggle (not
  // hover-only) so it works identically on touch and desktop, following the
  // same open/outside-click/Escape-close pattern as ./Notifications.svelte.
  import { Info } from "lucide-svelte";
  import { _ } from "svelte-i18n";
  import { activeBudgetStatuses } from "./budgetDisplay.js";

  let { budget = null } = $props();

  let open = $state(false);
  let panelEl = $state(null);
  let buttonEl = $state(null);

  let statuses = $derived(activeBudgetStatuses(budget));

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

{#if statuses.length > 0}
  <div class="relative">
    <button
      bind:this={buttonEl}
      type="button"
      onclick={toggle}
      aria-haspopup="true"
      aria-expanded={open}
      aria-label={$_("header.statusInfoLabel")}
      title={$_("header.statusInfoLabel")}
      class="p-1 rounded-lg text-base-content/60 hover:text-base-content hover:bg-base-100 transition-colors"
    >
      <Info class="w-4 h-4" />
    </button>

    {#if open}
      <div
        bind:this={panelEl}
        role="dialog"
        aria-label={$_("header.statusInfoLabel")}
        class="absolute left-0 top-full mt-2 z-50 w-72 max-w-[calc(100vw-1.5rem)]
               rounded-xl bg-base-200 border border-base-300 shadow-2xl p-3"
      >
        <ul class="flex flex-col gap-2.5">
          {#each statuses as status (status.key)}
            <li class="flex flex-col gap-0.5">
              <span class="text-xs font-semibold text-base-content">{status.label}</span>
              <span class="text-[11px] leading-snug text-base-content/70"
                >{status.description}</span
              >
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  </div>
{/if}
