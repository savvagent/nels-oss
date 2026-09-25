import { describe, it, expect } from "vitest";
import { buildChoiceChips, finalizePayloadFor } from "./categoryChoice.js";

const choice = {
  budget_id: "b1",
  amount: 35,
  description: "haircut",
  proposed_new_name: "Haircut",
  candidates: [
    { id: "c1", name: "Rob" },
    { id: "c2", name: "Food" },
  ],
};

// A trivial translator: echoes the key, interpolating {name} when present.
const t = (key, opts) => {
  const name = opts?.values?.name;
  return name ? `${key}:${name}` : key;
};

describe("buildChoiceChips", () => {
  it("returns a chip per candidate plus create-new and something-else", () => {
    const chips = buildChoiceChips(choice, t);
    expect(chips.map((c) => c.kind)).toEqual([
      "existing",
      "existing",
      "create",
      "freetype",
    ]);
    expect(chips[0]).toMatchObject({ kind: "existing", categoryId: "c1", key: "existing:c1", label: "Rob" });
    expect(chips[1]).toMatchObject({ kind: "existing", categoryId: "c2", key: "existing:c2", label: "Food" });
    expect(chips[2]).toMatchObject({ kind: "create", key: "create" });
    expect(chips[2].label).toContain("Haircut");
  });

  it("gives every chip a unique key", () => {
    const chips = buildChoiceChips(choice, t);
    const keys = chips.map((c) => c.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("returns [] for a null choice", () => {
    expect(buildChoiceChips(null, t)).toEqual([]);
  });

  it("still offers create + something-else when there are no candidates", () => {
    const chips = buildChoiceChips({ ...choice, candidates: [] }, t);
    expect(chips.map((c) => c.kind)).toEqual(["create", "freetype"]);
  });
});

describe("finalizePayloadFor", () => {
  it("existing chip -> category_id payload", () => {
    expect(finalizePayloadFor(choice, { kind: "existing", categoryId: "c1" })).toEqual({
      amount: 35,
      description: "haircut",
      category_id: "c1",
    });
  });

  it("create chip -> new_category_name payload", () => {
    expect(finalizePayloadFor(choice, { kind: "create" })).toEqual({
      amount: 35,
      description: "haircut",
      new_category_name: "Haircut",
    });
  });

  it("freetype chip -> null (handled by focusing the input)", () => {
    expect(finalizePayloadFor(choice, { kind: "freetype" })).toBeNull();
  });
});
