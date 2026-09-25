// PWA lifecycle: service-worker registration + reactive update detection.
//
// The service worker parks a freshly-deployed version in `waiting` instead of
// activating it immediately (see public/sw.js). This module surfaces that as a
// reactive `updateAvailable` flag so the UI can offer an "update now" button,
// and `applyUpdate()` promotes the waiting worker and reloads into it.
//
// Note: content stays fresh on any reload regardless (the worker is
// network-first for navigations); this flow just lets users opt into the new
// version without hunting for a refresh.

let updateAvailable = $state(false);
let registration = null;
let reloading = false;

export const pwa = {
  get updateAvailable() {
    return updateAvailable;
  },
  // Activate the waiting worker, then reload (the controllerchange listener
  // below performs the reload once it takes control). If nothing is waiting,
  // a plain reload still pulls the freshest bundle via the network-first SW.
  applyUpdate() {
    const waiting = registration && registration.waiting;
    if (waiting) {
      waiting.postMessage({ type: "SKIP_WAITING" });
    } else {
      location.reload();
    }
  },
};

// Reflect the unread-notification count on the app icon via the Badging API
// (#55). Where the API is unavailable (most desktop browsers, iOS) this is a
// silent no-op — the in-app topbar badge still works. A count of 0 (or less)
// clears the badge. All calls are wrapped because setAppBadge can reject/throw
// in some embedded contexts and a failed badge update must never break the app.
export function setAppBadge(count) {
  try {
    if (typeof navigator === "undefined") return;
    const n = Math.max(0, Math.floor(Number(count) || 0));
    if (n > 0) {
      if ("setAppBadge" in navigator) navigator.setAppBadge(n).catch(() => {});
    } else if ("clearAppBadge" in navigator) {
      navigator.clearAppBadge().catch(() => {});
    }
  } catch {
    // Non-fatal: Badging API unsupported or threw; the in-app badge suffices.
  }
}

export function initPwa() {
  if (!("serviceWorker" in navigator) || !import.meta.env.PROD) return;

  // Whether a worker already controlled this page before we registered. On a
  // brand-new install, activate() -> clients.claim() fires controllerchange
  // with no prior controller — we must NOT reload then (it would interrupt the
  // first session). Only reload when an update the user opted into takes over
  // an already-controlled page.
  const hadController = !!navigator.serviceWorker.controller;

  navigator.serviceWorker.addEventListener("controllerchange", () => {
    if (!hadController || reloading) return;
    reloading = true;
    location.reload();
  });

  window.addEventListener("load", async () => {
    try {
      registration = await navigator.serviceWorker.register("/sw.js");
    } catch (err) {
      console.error("PWA Service Worker registration failed:", err);
      return;
    }

    // Mark an update available once a newly-installing worker reaches the
    // "installed" state while a controller is already in charge (i.e. it's an
    // update, not the first install).
    const trackInstalling = (worker) => {
      worker.addEventListener("statechange", () => {
        if (worker.state === "installed" && navigator.serviceWorker.controller) {
          updateAvailable = true;
        }
      });
    };

    // A worker already waiting (with a controller present, so it's an update
    // rather than the first install) means a new version is ready right now.
    if (registration.waiting && navigator.serviceWorker.controller) {
      updateAvailable = true;
    }

    // A worker already mid-install when register() resolved is missed by both
    // the waiting check above and the updatefound listener below — catch it.
    if (registration.installing && navigator.serviceWorker.controller) {
      trackInstalling(registration.installing);
    }

    // Catch a worker that starts installing while the app is open.
    registration.addEventListener("updatefound", () => {
      const installing = registration.installing;
      if (installing) trackInstalling(installing);
    });

    // Look for a fresh deploy when the app regains focus and on a slow timer,
    // so long-lived PWA sessions still notice updates.
    const checkForUpdate = () => registration.update().catch(() => {});
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") checkForUpdate();
    });
    setInterval(checkForUpdate, 60 * 1000);
  });
}
