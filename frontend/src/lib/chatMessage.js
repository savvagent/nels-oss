// Pure helper deciding whether a chat response's categories table markup should be
// attached to the chat message for inline rendering (nels#301). Framework-free so the
// decision is unit-testable without mounting App.svelte (this repo's vitest runs
// environment: "node" — no jsdom/testing-library, see every other *.test.js).
//
// Both the backend's CATEGORY_BALANCE and LIST_CATEGORIES chat actions populate
// `categories_table_html`, but only LIST_CATEGORIES should navigate to the dedicated
// Categories page (signaled by `open_categories_list`, see App.svelte's navigation gate) —
// CATEGORY_BALANCE must answer inline in the chat transcript instead (rule 20c). So the
// table is attached to the message ONLY when the backend did NOT signal navigation:
// LIST_CATEGORIES's table stays discarded here, exactly as before (CategoriesView
// self-fetches its own fresh copy after navigating, per #233's existing convention).
export function inlineCategoriesTableHtml(chatRes) {
  if (!chatRes || chatRes.open_categories_list || !chatRes.categories_table_html) {
    return null;
  }
  return chatRes.categories_table_html;
}
