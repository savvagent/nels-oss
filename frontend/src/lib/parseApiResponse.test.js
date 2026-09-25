import { describe, it, expect, vi } from "vitest";
import { parseApiResponse } from "./parseApiResponse.js";

// A minimal Response-like stub. We deliberately do NOT use a real browser/undici
// Response (vitest runs environment: "node" here — see fetchTimeout.test.js), so a
// plain object with a `status` and an async `text()` is all parseApiResponse needs.
function fakeResponse(status, body) {
  return {
    status,
    text: vi.fn(async () => body),
  };
}

describe("parseApiResponse", () => {
  it("returns null for a 202 with an empty body (the #366 refresh regression)", async () => {
    // refresh_linked_account_handler returns 202 Accepted with an EMPTY body; the old
    // `res.json()` threw "Unexpected end of JSON input" on exactly this response.
    const res = fakeResponse(202, "");
    await expect(parseApiResponse(res)).resolves.toBeNull();
  });

  it("returns null for a 204 without ever reading the body", async () => {
    const res = fakeResponse(204, "should never be read");
    await expect(parseApiResponse(res)).resolves.toBeNull();
    expect(res.text).not.toHaveBeenCalled();
  });

  it("returns the parsed object for a 200 with a valid JSON body", async () => {
    const res = fakeResponse(200, JSON.stringify({ id: 7, name: "Checking" }));
    await expect(parseApiResponse(res)).resolves.toEqual({ id: 7, name: "Checking" });
  });

  it("returns null for a 200 with an empty-string body", async () => {
    const res = fakeResponse(200, "");
    await expect(parseApiResponse(res)).resolves.toBeNull();
  });

  it("returns null for a 205 with an empty body (generic bodiless-2xx path, not a status allowlist)", async () => {
    // A different bodiless 2xx than the #366 202 — proves the empty-body handling keys off
    // the body content, not a hard-coded status, so future bodiless successes are covered too.
    const res = fakeResponse(205, "");
    await expect(parseApiResponse(res)).resolves.toBeNull();
  });

  it("still throws a JSON parse error on a 200 with a non-empty invalid-JSON body (preserved behavior)", async () => {
    const res = fakeResponse(200, "not json");
    await expect(parseApiResponse(res)).rejects.toThrow(SyntaxError);
  });
});
