// Detect whether the current device runs iOS or Android.
// UA-based on purpose: the requirement targets two OS families specifically,
// not "touch" devices in general (which would include touchscreen laptops).
// Extends the iOS-detection pattern from InstallPrompt.svelte to also cover
// iPadOS, which reports a desktop ("Macintosh") UA but has multi-touch.
export function isMobileOS() {
  if (typeof navigator === "undefined") return false; // fail open → show buttons
  const ua = navigator.userAgent || "";
  const isAndroid = /android/i.test(ua);
  const isIOS =
    /iphone|ipad|ipod/i.test(ua) ||
    (/macintosh/i.test(ua) && (navigator.maxTouchPoints || 0) > 1);
  return isAndroid || isIOS;
}
