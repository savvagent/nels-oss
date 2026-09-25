import { describe, it, expect } from "vitest";
import {
  PROVIDER_TERMS_URLS,
  providerLabelKey,
  statusLineKey,
  canSave,
  providerErrorKey,
  embeddingsNoteVisible,
  providerRetired,
} from "./aiProviderView.js";

describe("aiProviderView", () => {
  it("has a terms URL for every provider", () => {
    for (const p of ["gemini", "openai", "anthropic"]) {
      expect(PROVIDER_TERMS_URLS[p]).toMatch(/^https:\/\//);
    }
  });

  it("maps providers to label keys with a safe fallback", () => {
    expect(providerLabelKey("openai")).toBe("aiProvider.providerOpenai");
    expect(providerLabelKey("anthropic")).toBe("aiProvider.providerAnthropic");
    expect(providerLabelKey("gemini")).toBe("aiProvider.providerGemini");
    expect(providerLabelKey("mistral")).toBe("aiProvider.providerUnknown");
  });

  it("status line reflects mode and last_error", () => {
    expect(statusLineKey({ mode: "nels" })).toBe("aiProvider.statusNels");
    expect(statusLineKey({ mode: "offline" })).toBe("aiProvider.statusOffline");
    expect(statusLineKey({ mode: "byo", last_error: null })).toBe("aiProvider.statusByoOk");
    expect(statusLineKey({ mode: "byo", last_error: "auth_rejected" })).toBe("aiProvider.statusByoRejected");
    expect(statusLineKey({ mode: "byo", last_error: "key_unavailable" })).toBe("aiProvider.statusByoUnreadable");
    expect(statusLineKey(null)).toBe("aiProvider.statusLoading");
  });

  it("canSave requires an offered provider and a 1-512 char trimmed key", () => {
    expect(canSave("openai", " sk-1 ", ["openai"])).toBe(true);
    expect(canSave("openai", "   ", ["openai"])).toBe(false);
    expect(canSave("openai", "x".repeat(513), ["openai"])).toBe(false);
    expect(canSave("anthropic", "k", ["openai"])).toBe(false);
  });

  it("maps chat error codes to i18n keys", () => {
    expect(providerErrorKey("auth_rejected")).toBe("aiProvider.chatErrorRejected");
    expect(providerErrorKey("rate_limited")).toBe("aiProvider.chatErrorQuota");
    expect(providerErrorKey("key_unavailable")).toBe("aiProvider.chatErrorUnreadable");
    expect(providerErrorKey("unavailable")).toBe("aiProvider.chatErrorUnavailable");
    expect(providerErrorKey(undefined)).toBe(null);
  });

  it("flags a saved provider that is no longer offered", () => {
    expect(providerRetired({ mode: "byo", provider: "anthropic", available_providers: ["openai"] })).toBe(true);
    expect(providerRetired({ mode: "byo", provider: "openai", available_providers: ["openai"] })).toBe(false);
    expect(providerRetired({ mode: "nels", provider: "gemini", available_providers: [] })).toBe(false);
  });

  it("embeddings note shows only for BYO providers that can't embed", () => {
    expect(embeddingsNoteVisible({ mode: "byo", embeddings_enabled: false })).toBe(true);
    expect(embeddingsNoteVisible({ mode: "byo", embeddings_enabled: true })).toBe(false);
    expect(embeddingsNoteVisible({ mode: "nels", embeddings_enabled: true })).toBe(false);
  });
});
