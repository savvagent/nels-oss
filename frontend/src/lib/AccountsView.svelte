<script>
  // Router-outlet view for the dedicated Accounts page (#338) — hosts the
  // LinkedAccounts.svelte UI, previously embedded in Settings.svelte (see
  // Settings.svelte's git history / #303). LinkedAccounts renders its own
  // card/dialog surface (#365 redesign), so this wrapper only supplies the
  // page chrome (title + back button) and no longer boxes it in an extra
  // bg-base-200 container. Its isPro gating, pro-gate error handling, and
  // chat-reachability are unaffected by this view.
  import { _ } from "svelte-i18n";
  import { ArrowLeft } from "lucide-svelte";
  import LinkedAccounts from "./LinkedAccounts.svelte";

  let { fetchApi, activeBudget, subscription, onUpgrade, onBack } = $props();

  let backButtonEl = $state(null);

  // Mounting this route IS the "opened" signal (matches Settings.svelte's/
  // CategoriesView.svelte's convention) — move focus to the back button for
  // keyboard/a11y users, mirroring Settings.svelte's own focus-on-mount
  // behavior specifically (CategoriesView.svelte re-fetches on mount but
  // does not move focus).
  $effect(() => {
    backButtonEl?.focus();
  });
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("accounts.title")}</h3>
    <button type="button" bind:this={backButtonEl} class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" /> {$_("accounts.back")}
    </button>
  </div>

  <div class="flex-grow overflow-y-auto">
    {#if !activeBudget}
      <p class="text-sm text-base-content/50 py-4">
        {$_("commands.noActiveBudget")}
      </p>
    {:else}
      <LinkedAccounts
        budgetId={activeBudget.id}
        isPro={subscription?.is_pro}
        {onUpgrade}
        {fetchApi}
      />
    {/if}
  </div>
</div>
