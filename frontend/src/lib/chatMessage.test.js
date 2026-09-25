import { describe, it, expect } from "vitest";
import { inlineCategoriesTableHtml } from "./chatMessage.js";

describe("inlineCategoriesTableHtml", () => {
  it("returns the table html when present and the backend did not signal navigation (CATEGORY_BALANCE)", () => {
    const chatRes = { categories_table_html: "<table><tr><td>Entertainment</td></tr></table>" };
    expect(inlineCategoriesTableHtml(chatRes)).toBe(chatRes.categories_table_html);
  });

  it("returns null when open_categories_list is set, even though categories_table_html is present (LIST_CATEGORIES navigates instead)", () => {
    const chatRes = {
      categories_table_html: "<table><tr><td>Groceries</td></tr></table>",
      open_categories_list: true,
    };
    expect(inlineCategoriesTableHtml(chatRes)).toBeNull();
  });

  it("returns null when there is no table html at all", () => {
    expect(inlineCategoriesTableHtml({})).toBeNull();
    expect(inlineCategoriesTableHtml({ categories_table_html: null })).toBeNull();
    expect(inlineCategoriesTableHtml({ categories_table_html: "" })).toBeNull();
  });

  it("returns null for a nullish chat response", () => {
    expect(inlineCategoriesTableHtml(null)).toBeNull();
    expect(inlineCategoriesTableHtml(undefined)).toBeNull();
  });
});
