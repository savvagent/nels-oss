import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  fetchWithTimeout,
  DEFAULT_FETCH_TIMEOUT_MS,
  TIMEOUT_MESSAGE,
} from "./fetchTimeout.js";

// A fetch mock that mimics the real fetch/AbortController contract: given an AbortSignal, the
// returned promise stays pending until EITHER the signal aborts (rejects with an AbortError,
// exactly like a real aborted fetch) OR the test's own `settle(resolve, reject)` hook fires.
function mockFetchUntilAbortOr(settle) {
  return vi.fn((url, options) => {
    return new Promise((resolve, reject) => {
      options?.signal?.addEventListener("abort", () => {
        const err = new Error("The operation was aborted.");
        err.name = "AbortError";
        reject(err);
      });
      settle(resolve, reject);
    });
  });
}

describe("fetchWithTimeout", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("rejects with a clear timeout error when fetch never settles, and aborts the signal", async () => {
    const fetchMock = mockFetchUntilAbortOr(() => {}); // never resolves/rejects on its own
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithTimeout("https://example.test/api", {}, 1000);
    const assertion = expect(promise).rejects.toThrow(TIMEOUT_MESSAGE);
    await vi.advanceTimersByTimeAsync(1000);
    await assertion;

    const passedSignal = fetchMock.mock.calls[0][1].signal;
    expect(passedSignal.aborted).toBe(true);
  });

  it("times out after DEFAULT_FETCH_TIMEOUT_MS when timeoutMs is omitted entirely", async () => {
    const fetchMock = mockFetchUntilAbortOr(() => {}); // never resolves/rejects on its own
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithTimeout("https://example.test/api", {});
    const assertion = expect(promise).rejects.toThrow(TIMEOUT_MESSAGE);
    await vi.advanceTimersByTimeAsync(DEFAULT_FETCH_TIMEOUT_MS);
    await assertion;
  });

  it("resolves normally when fetch settles before the timeout, and clears the timer", async () => {
    const fakeResponse = { ok: true, status: 200 };
    const fetchMock = mockFetchUntilAbortOr((resolve) => resolve(fakeResponse));
    vi.stubGlobal("fetch", fetchMock);

    const result = await fetchWithTimeout("https://example.test/api", {}, 1000);

    expect(result).toBe(fakeResponse);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("rethrows a genuine (non-abort) network error unchanged", async () => {
    const networkError = new TypeError("Failed to fetch");
    const fetchMock = mockFetchUntilAbortOr((_resolve, reject) => reject(networkError));
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchWithTimeout("https://example.test/api", {}, 1000)).rejects.toBe(
      networkError,
    );
  });

  it("honors a custom timeoutMs and timeoutMessage", async () => {
    const fetchMock = mockFetchUntilAbortOr(() => {});
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithTimeout(
      "https://example.test/api",
      {},
      500,
      "Custom timeout message",
    );
    const assertion = expect(promise).rejects.toThrow("Custom timeout message");
    await vi.advanceTimersByTimeAsync(500);
    await assertion;
  });

  it("passes an existing options.signal straight through with no timeout applied", async () => {
    const externalController = new AbortController();
    const fakeResponse = { ok: true, status: 200 };
    const fetchMock = vi.fn(async () => fakeResponse);
    vi.stubGlobal("fetch", fetchMock);

    const result = await fetchWithTimeout(
      "https://example.test/api",
      { signal: externalController.signal },
      1000,
    );

    expect(result).toBe(fakeResponse);
    expect(fetchMock).toHaveBeenCalledWith("https://example.test/api", {
      signal: externalController.signal,
    });
    expect(vi.getTimerCount()).toBe(0);
  });

  it("exports a sane positive default timeout", () => {
    expect(DEFAULT_FETCH_TIMEOUT_MS).toBeGreaterThan(0);
  });

  // Mirrors App.svelte's downloadExport idiom (#249): a caller that needs its timeout to also
  // cover work done AFTER fetch() resolves (e.g. res.blob(), which per the Fetch spec is not
  // covered by fetch() settling) supplies its own AbortController via options.signal and owns
  // its own timer, only clearing it once ALL of its own work is done. Because fetchWithTimeout
  // passes an already-signaled call straight through with no timeout of its own, the caller's
  // timer keeps running past fetch() resolving and can still abort a slow subsequent step.
  it("lets a caller-owned AbortController/timer outlive fetch() resolving, so it can also abort a slow post-fetch step", async () => {
    const controller = new AbortController();
    const fakeResponse = { ok: true, status: 200 };
    const fetchMock = vi.fn(async () => fakeResponse);
    vi.stubGlobal("fetch", fetchMock);

    const timer = setTimeout(() => controller.abort(), 1000);
    const res = await fetchWithTimeout(
      "https://example.test/api",
      { signal: controller.signal },
      1000,
    );
    expect(res).toBe(fakeResponse);
    // fetch() has resolved, but the caller's own timer is still pending (fetchWithTimeout never
    // touched it, since it took the pass-through path for an already-set signal).
    expect(controller.signal.aborted).toBe(false);

    // Simulate a slow body read (e.g. res.blob()) racing the same controller's signal. The
    // `.rejects` assertion must be attached before the timer fires (mirroring this file's other
    // "never settles on its own" tests above) so the rejection always has a handler by the time
    // it happens, instead of racing vi.advanceTimersByTimeAsync as an unhandled rejection.
    const slowBodyRead = new Promise((_resolve, reject) => {
      controller.signal.addEventListener("abort", () => {
        const err = new Error("The operation was aborted.");
        err.name = "AbortError";
        reject(err);
      });
    });
    const assertion = expect(slowBodyRead).rejects.toThrow();

    await vi.advanceTimersByTimeAsync(1000);
    await assertion;
    expect(controller.signal.aborted).toBe(true);

    clearTimeout(timer);
  });
});
