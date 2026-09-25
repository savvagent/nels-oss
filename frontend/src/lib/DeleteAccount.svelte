<script>
  import { ArrowLeft, AlertTriangle } from "lucide-svelte";
  import { _ } from "svelte-i18n";

  // Content/logic lifted verbatim from App.svelte's former inline modal —
  // the business logic (canConfirm gating, the actual DELETE call) stays in
  // App.svelte (canDeleteAccount/confirmDeleteAccount, unchanged) and flows
  // in as props, matching every other routed page in this codebase.
  let {
    userEmail = "",
    emailValue = "",
    busy = false,
    canConfirm = false,
    onEmailInput,
    onConfirm,
    onBack,
  } = $props();

  let backButtonEl = $state(null);
  $effect(() => {
    backButtonEl?.focus();
  });
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("deleteAccount.title")}</h3>
    <button type="button" bind:this={backButtonEl} class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" /> {$_("history.back")}
    </button>
  </div>

  <div class="flex-grow flex items-start justify-center py-8">
    <div class="w-full max-w-md space-y-6">
      <div class="flex items-start gap-2 text-error">
        <AlertTriangle class="w-6 h-6 shrink-0 mt-0.5" />
        <div class="space-y-1">
          <p class="font-semibold">{$_("deleteAccount.warning")}</p>
          <p class="font-medium">{$_("deleteAccount.billingWarning")}</p>
        </div>
      </div>

      <div class="space-y-4">
        <div class="form-control">
          <label class="label" for="delete-email-input">
            <span class="label-text text-base-content/80">{$_("deleteAccount.emailLabel")}</span>
          </label>
          <input
            id="delete-email-input"
            type="email"
            autocomplete="off"
            placeholder={userEmail}
            class="input input-bordered bg-base-100 border-base-300 text-base-content focus:border-error focus:outline-none w-full"
            value={emailValue}
            oninput={(e) => onEmailInput?.(e.target.value)}
          />
        </div>
        <p class="text-sm text-base-content/70">{$_("deleteAccount.passkeyInfo")}</p>
      </div>

      <div class="flex justify-end gap-2">
        <button class="btn btn-ghost" onclick={() => onBack?.()}>{$_("common.cancel")}</button>
        <button class="btn btn-error" onclick={() => onConfirm?.()} disabled={!canConfirm}>
          {#if busy}<span class="loading loading-spinner"></span>{/if}
          {$_("deleteAccount.confirm")}
        </button>
      </div>
    </div>
  </div>
</div>
