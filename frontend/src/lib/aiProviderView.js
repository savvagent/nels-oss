// Pure view helpers for the Settings "AI provider" card (nels-oss#3).
// The component itself is gated by `pnpm run build` (node-env vitest).

// Each provider's official API data-use page, verified 2026-09-25.
// OpenAI's old `openai.com/policies/api-data-usage-policies/` returns 403 to
// non-browser clients; its current API data-controls page is the one below
// (platform.openai.com/docs/guides/your-data 301s here).
export const PROVIDER_TERMS_URLS = {
  gemini: "https://ai.google.dev/gemini-api/terms",
  openai: "https://developers.openai.com/api/docs/guides/your-data",
  anthropic: "https://www.anthropic.com/legal/commercial-terms",
};

const LABELS = {
  gemini: "aiProvider.providerGemini",
  openai: "aiProvider.providerOpenai",
  anthropic: "aiProvider.providerAnthropic",
};

export function providerLabelKey(p) {
  return LABELS[p] ?? "aiProvider.providerUnknown";
}

export function statusLineKey(view) {
  if (!view) return "aiProvider.statusLoading";
  if (view.mode === "nels") return "aiProvider.statusNels";
  if (view.mode === "offline") return "aiProvider.statusOffline";
  if (view.last_error === "auth_rejected") return "aiProvider.statusByoRejected";
  if (view.last_error === "key_unavailable") return "aiProvider.statusByoUnreadable";
  return "aiProvider.statusByoOk";
}

export function canSave(provider, key, available) {
  const k = (key ?? "").trim();
  return (available ?? []).includes(provider) && k.length > 0 && k.length <= 512;
}

const CHAT_ERRORS = {
  auth_rejected: "aiProvider.chatErrorRejected",
  rate_limited: "aiProvider.chatErrorQuota",
  key_unavailable: "aiProvider.chatErrorUnreadable",
  unavailable: "aiProvider.chatErrorUnavailable",
};

export function providerErrorKey(code) {
  return code ? (CHAT_ERRORS[code] ?? "aiProvider.chatErrorUnavailable") : null;
}

export function embeddingsNoteVisible(view) {
  return view?.mode === "byo" && view?.embeddings_enabled === false;
}

// Spec §6: a saved key for a provider later removed from ENABLED_BYO_PROVIDERS
// keeps working, but the provider is no longer offered for new keys.
export function providerRetired(view) {
  return (
    view?.mode === "byo" &&
    !!view?.provider &&
    !(view?.available_providers ?? []).includes(view.provider)
  );
}

// A free (unpaid) Google AI Studio key falls under Google's unpaid-service
// terms, which let Google use prompts and responses to improve its products.
// The note shows only in the own-key branch while Gemini is selected.
export function showsGeminiFreeTierNote(choice, provider) {
  return choice === "own" && provider === "gemini";
}
