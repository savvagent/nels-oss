// Pure helpers for the inline category-choice chips (#376).
//
// When Nels would auto-create a brand-new category for an un-categorized
// transaction, the backend returns a `pending_category_choice` payload instead
// of logging. The chat surfaces it as a row of tappable chips: one per existing
// candidate category, a "Create new '<Name>'" chip, and a free-type escape.
// These helpers turn the payload into chip view-models and turn a tapped chip
// into the POST /budgets/:id/transactions/finalize body.

/**
 * Build the chip view-models for a pending category choice.
 * @param {object|null} choice - the `pending_category_choice` payload.
 * @param {(key: string, opts?: object) => string} t - i18n translator.
 * @returns {Array<{kind: string, label: string, categoryId?: string}>}
 */
export function buildChoiceChips(choice, t) {
  if (!choice) return [];
  // `key` is a stable, collision-proof identity for each chip (existing chips key
  // on the category id, not the label, so two categories with the same display
  // name can't clash in a keyed {#each}).
  const chips = (choice.candidates || []).map((c) => ({
    kind: "existing",
    categoryId: c.id,
    key: `existing:${c.id}`,
    label: c.name,
  }));
  chips.push({
    kind: "create",
    key: "create",
    label: t("categoryChoice.createNew", {
      values: { name: choice.proposed_new_name },
    }),
  });
  chips.push({
    kind: "freetype",
    key: "freetype",
    label: t("categoryChoice.somethingElse"),
  });
  return chips;
}

/**
 * Build the finalize request body for a tapped chip. Returns null for the
 * free-type escape (handled by focusing the chat input, not a direct finalize).
 * @param {object} choice - the `pending_category_choice` payload.
 * @param {{kind: string, categoryId?: string}} chip
 * @returns {object|null}
 */
export function finalizePayloadFor(choice, chip) {
  const base = { amount: choice.amount, description: choice.description };
  if (chip.kind === "existing") return { ...base, category_id: chip.categoryId };
  if (chip.kind === "create")
    return { ...base, new_category_name: choice.proposed_new_name };
  return null;
}
