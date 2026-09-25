<script>
  // Soft install nudge: a dismissible banner encouraging users to install Nels
  // as a PWA. The app stays fully usable in a browser tab — this never gates.
  //
  // Two paths, because install UX differs by platform:
  //  - Chromium (Android/desktop) fires `beforeinstallprompt`; we capture it and
  //    drive a one-tap install via prompt().
  //  - iOS Safari never fires that event, so we show "Add to Home Screen"
  //    instructions instead of a button.
  import { onMount } from "svelte";
  import { Download, Share, X } from "lucide-svelte";
  import { _ } from "svelte-i18n";

  let deferredPrompt = $state(null); // captured beforeinstallprompt event (Chromium)
  let isIos = $state(false); // iOS Safari (manual install only)
  let visible = $state(false);

  // True when already running as an installed PWA — never nudge in that case.
  function isStandalone() {
    return (
      window.matchMedia?.("(display-mode: standalone)").matches ||
      window.navigator.standalone === true ||
      document.referrer.startsWith("android-app://")
    );
  }

  // Hide for the rest of this page view only. We intentionally don't persist
  // the dismissal: the nudge reappears on each load until the PWA is installed
  // (at which point isStandalone() suppresses it).
  function dismiss() {
    visible = false;
  }

  async function install() {
    if (!deferredPrompt) return;
    deferredPrompt.prompt();
    await deferredPrompt.userChoice;
    // The event is single-use; clear it and hide regardless of choice.
    deferredPrompt = null;
    visible = false;
  }

  onMount(() => {
    if (isStandalone()) return;

    const ua = navigator.userAgent || "";
    const iosDevice = /iphone|ipad|ipod/i.test(ua);
    const isSafari = /safari/i.test(ua) && !/crios|fxios|edgios/i.test(ua);
    if (iosDevice && isSafari) {
      isIos = true;
      visible = true;
      return;
    }

    const onPrompt = (e) => {
      e.preventDefault(); // suppress the mini-infobar; we show our own UI
      deferredPrompt = e;
      visible = true;
    };
    window.addEventListener("beforeinstallprompt", onPrompt);

    // Hide once the app is actually installed (subsequent loads run as a
    // standalone PWA, so isStandalone() keeps the nudge suppressed).
    const onInstalled = () => dismiss();
    window.addEventListener("appinstalled", onInstalled);

    return () => {
      window.removeEventListener("beforeinstallprompt", onPrompt);
      window.removeEventListener("appinstalled", onInstalled);
    };
  });
</script>

{#if visible}
  <div class="toast toast-middle toast-center z-50 w-full max-w-md px-3">
    <div class="alert bg-base-200 shadow-lg flex items-start gap-3">
      <Download class="h-5 w-5 shrink-0 mt-0.5 text-primary" aria-hidden="true" />
      <div class="flex-1 text-left">
        <h3 class="font-semibold">{$_("install.title")}</h3>
        {#if isIos}
          <p class="text-sm opacity-80">
            {$_("install.iosBody")}
            <Share class="inline h-4 w-4 align-text-bottom" aria-hidden="true" />
          </p>
        {:else}
          <p class="text-sm opacity-80">{$_("install.body")}</p>
          <button class="btn btn-primary btn-sm mt-2" onclick={install}>
            <Download class="h-4 w-4" aria-hidden="true" />
            {$_("install.action")}
          </button>
        {/if}
      </div>
      <button
        class="btn btn-ghost btn-xs btn-circle"
        onclick={dismiss}
        aria-label={$_("install.dismiss")}
      >
        <X class="h-4 w-4" aria-hidden="true" />
      </button>
    </div>
  </div>
{/if}
