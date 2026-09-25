# Router-Driven Main Content Outlet Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the modal (`Insights.svelte`, `showInsights`) and inline-chat-HTML
(`categories_table_html` in a chat bubble) rendering of categories/insights with a single
router-governed outlet inside `#chat-scroll-area`, without touching the header, status strip, or
bottom prompt region, and without losing chat state on navigation.

**Architecture:** A new pure module `frontend/src/lib/router.js` maps a URL hash to one of three
flat route names (`chat`/`categories`/`insights`) and back — unit-tested the same way
`chatWindow.js`/`commands.js` are. `App.svelte` gains a `route = $state("chat")` and a `navigate()`
helper that updates `route` and syncs `location.hash`; every existing `showInsights` toggle site
becomes a `navigate()` call. `Insights.svelte` sheds its DaisyUI modal wrapper (it's now mounted by
the outlet's `{#if}`, not gated by an `open` prop). A new `CategoriesView.svelte` — sibling to
`Insights.svelte`, same self-fetching convention — replaces the `table_html`-in-chat-bubble path.
See design doc: `docs/superpowers/specs/2026-07-03-router-outlet-design.md`.

**Tech Stack:** Svelte 5 (runes), Vite, Tailwind v4 + DaisyUI, `lucide-svelte`, `svelte-i18n`,
Vitest (`environment: "node"`, existing config at `frontend/vitest.config.js`).

---

## File Structure

- **Create** `frontend/src/lib/router.js` — pure `routeFromHash`/`hashForRoute`/`ROUTES`.
- **Create** `frontend/src/lib/router.test.js` — Vitest unit tests.
- **Create** `frontend/src/lib/CategoriesView.svelte` — the new categories outlet view.
- **Modify** `frontend/src/lib/Insights.svelte` — drop the modal wrapper + `open` prop.
- **Modify** `frontend/src/App.svelte` — route state, `navigate()`, hash sync, outlet wiring, every
  `showInsights`/categories-chat-bubble call site.
- **Modify** locale JSON files under `frontend/src/lib/i18n/locales/` (`en`, `de`, `es`, `fr`, `it`,
  `pt`) — add a `categories.*` namespace + `commands.categoriesOpened`.

---

## Task 1: Pure route-mapping module (TDD)

**Files:**
- Create: `frontend/src/lib/router.test.js`
- Create: `frontend/src/lib/router.js`

- [ ] **Step 1: Write the failing tests — `frontend/src/lib/router.test.js`**

```js
import { describe, it, expect } from "vitest";
import { ROUTES, routeFromHash, hashForRoute } from "./router.js";

describe("ROUTES", () => {
  it("lists the three flat routes", () => {
    expect(ROUTES).toEqual(["chat", "categories", "insights"]);
  });
});

describe("routeFromHash", () => {
  it("maps #/categories to categories", () => {
    expect(routeFromHash("#/categories")).toBe("categories");
  });

  it("maps #/insights to insights", () => {
    expect(routeFromHash("#/insights")).toBe("insights");
  });

  it("falls back to chat for an empty hash", () => {
    expect(routeFromHash("")).toBe("chat");
  });

  it("falls back to chat for a bare #", () => {
    expect(routeFromHash("#")).toBe("chat");
  });

  it("falls back to chat for #/", () => {
    expect(routeFromHash("#/")).toBe("chat");
  });

  it("falls back to chat for an unrecognized route", () => {
    expect(routeFromHash("#/nope")).toBe("chat");
  });

  it("falls back to chat for garbage/unrelated hashes", () => {
    expect(routeFromHash("#foo=bar")).toBe("chat");
  });

  it("falls back to chat for null/undefined", () => {
    expect(routeFromHash(null)).toBe("chat");
    expect(routeFromHash(undefined)).toBe("chat");
  });

  it("is case-sensitive (unrecognized case falls back to chat)", () => {
    expect(routeFromHash("#/Categories")).toBe("chat");
  });
});

describe("hashForRoute", () => {
  it("returns an empty string for chat (no hash)", () => {
    expect(hashForRoute("chat")).toBe("");
  });

  it("returns #/categories for categories", () => {
    expect(hashForRoute("categories")).toBe("#/categories");
  });

  it("returns #/insights for insights", () => {
    expect(hashForRoute("insights")).toBe("#/insights");
  });

  it("falls back to empty string for an unknown route", () => {
    expect(hashForRoute("nope")).toBe("");
  });

  it("round-trips with routeFromHash for every route", () => {
    for (const route of ROUTES) {
      expect(routeFromHash(hashForRoute(route))).toBe(route);
    }
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd frontend && pnpm test -- router`
Expected: FAIL — `Cannot find module './router.js'` (the module doesn't exist yet).

- [ ] **Step 3: Write the implementation — `frontend/src/lib/router.js`**

```js
// Pure hash <-> route mapping for the main-content outlet (#233). No Svelte,
// no I/O — unit-tested directly, mirrors frontend/src/lib/chatWindow.js and
// commands.js. The reactive `route` state itself lives in App.svelte (a
// `$state` sibling to the existing `activeScreen`); this module only owns the
// pure translation between a URL hash and a route name, kept separate so it's
// testable under vitest's `environment: "node"` (no jsdom/Svelte compiler
// needed here).
//
// Hash-based (not pushState/history-based) deliberately: this is a
// single-`index.html` Vite SPA with a hand-rolled service worker (see
// frontend/public/sw.js) that has no "unknown path -> index.html" fallback
// rule. A hash never leaves index.html as the request path, so routing needs
// zero SW changes.

export const ROUTES = ["chat", "categories", "insights"];

// "#/categories" -> "categories", "#/insights" -> "insights"; anything else
// (empty, "#", "#/", unrecognized, non-string) falls back to "chat" — the
// outlet always has a valid view to show.
export function routeFromHash(hash) {
  if (typeof hash !== "string") return "chat";
  const name = hash.replace(/^#\/?/, "");
  return ROUTES.includes(name) && name !== "chat" ? name : "chat";
}

// Inverse of routeFromHash. "chat" (the default) clears the hash entirely
// rather than round-tripping through "#/chat", so the URL stays clean for the
// common case.
export function hashForRoute(route) {
  return ROUTES.includes(route) && route !== "chat" ? `#/${route}` : "";
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && pnpm test -- router`
Expected: PASS — all `routeFromHash`/`hashForRoute`/`ROUTES` cases green.

- [ ] **Step 5: Commit**

```bash
cd frontend
git add src/lib/router.js src/lib/router.test.js
git commit -m "feat(#233): add pure hash<->route mapping module"
```

---

## Task 2: `CategoriesView.svelte` + i18n keys

**Files:**
- Create: `frontend/src/lib/CategoriesView.svelte`
- Modify: `frontend/src/lib/i18n/locales/en.json`
- Modify: `frontend/src/lib/i18n/locales/de.json`
- Modify: `frontend/src/lib/i18n/locales/es.json`
- Modify: `frontend/src/lib/i18n/locales/fr.json`
- Modify: `frontend/src/lib/i18n/locales/it.json`
- Modify: `frontend/src/lib/i18n/locales/pt.json`

No component-test harness exists in this repo (vitest runs `environment: "node"`, no
jsdom/testing-library). This task is not TDD; verification is `pnpm run build` here plus manual
verification once wired into `App.svelte` in Task 4.

- [ ] **Step 1: Add the `categories.*` namespace and `commands.categoriesOpened` to every locale file**

In `frontend/src/lib/i18n/locales/en.json`, add a new top-level `"categories"` object (place it as
a sibling right after the existing `"insights"` object, before `"notifications"`):

```json
"categories": {
  "title": "Categories",
  "loading": "Loading categories…",
  "back": "Back to chat"
},
```

And add `categoriesOpened` inside the existing `"commands"` object, next to `categoriesNone`:

```json
"categoriesOpened": "Opening categories…",
```

(Insert it as a new line directly after the existing `"categoriesNone": "..."` line inside
`"commands": { ... }`, keeping valid JSON — comma after, no trailing comma if it's the last key.)

Repeat for the other 5 locales with these values:

`frontend/src/lib/i18n/locales/de.json`:
```json
"categories": {
  "title": "Kategorien",
  "loading": "Kategorien werden geladen…",
  "back": "Zurück zum Chat"
},
```
```json
"categoriesOpened": "Kategorien werden geöffnet…",
```

`frontend/src/lib/i18n/locales/es.json`:
```json
"categories": {
  "title": "Categorías",
  "loading": "Cargando categorías…",
  "back": "Volver al chat"
},
```
```json
"categoriesOpened": "Abriendo categorías…",
```

`frontend/src/lib/i18n/locales/fr.json`:
```json
"categories": {
  "title": "Catégories",
  "loading": "Chargement des catégories…",
  "back": "Retour au chat"
},
```
```json
"categoriesOpened": "Ouverture des catégories…",
```

`frontend/src/lib/i18n/locales/it.json`:
```json
"categories": {
  "title": "Categorie",
  "loading": "Caricamento delle categorie…",
  "back": "Torna alla chat"
},
```
```json
"categoriesOpened": "Apertura delle categorie…",
```

`frontend/src/lib/i18n/locales/pt.json`:
```json
"categories": {
  "title": "Categorias",
  "loading": "Carregando categorias…",
  "back": "Voltar ao chat"
},
```
```json
"categoriesOpened": "Abrindo categorias…",
```

Each locale file's top-level object order should mirror `en.json`'s (place `"categories"` right
after `"insights"`) purely for readability — not load-bearing, `svelte-i18n` doesn't care about key
order.

- [ ] **Step 2: Create `frontend/src/lib/CategoriesView.svelte`**

```svelte
<script>
  // Router-outlet view for the active budget's categories (#233) — replaces
  // the old table_html-in-a-chat-bubble rendering (App.svelte's `pushAiTable`,
  // now deleted). Sibling to ./Insights.svelte: self-fetches via the shared
  // `fetchApi` helper rather than receiving data as a prop, so it behaves
  // identically whether reached via the `/categories-list` slash command or a
  // chat-detected LIST_CATEGORIES intent (both just navigate here).
  import { _ } from "svelte-i18n";
  import { ArrowLeft, AlertTriangle } from "lucide-svelte";

  let { fetchApi, activeBudget, onBack } = $props();

  let html = $state("");
  let loading = $state(false);
  let error = $state("");

  async function load() {
    if (!activeBudget) return;
    loading = true;
    error = "";
    try {
      const res = await fetchApi(
        `/budgets/${activeBudget.id}/categories-table`,
      );
      html = res?.html ?? "";
    } catch (e) {
      error = e?.message || "Failed to load categories";
      html = "";
    } finally {
      loading = false;
    }
  }

  // Mounting IS the "open" signal now (the outlet's {#if route === "categories"}
  // owns mount/unmount), so fetch unconditionally at init. Re-fetch if the
  // active budget changes while this view stays mounted (e.g. a chat-driven
  // budget switch while categories are showing).
  $effect(() => {
    activeBudget;
    load();
  });
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("categories.title")}</h3>
    <button
      type="button"
      class="btn btn-sm btn-ghost gap-1.5"
      onclick={() => onBack?.()}
    >
      <ArrowLeft class="w-4 h-4" />
      {$_("categories.back")}
    </button>
  </div>

  {#if loading}
    <div
      class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
    >
      <span class="loading loading-spinner loading-lg text-primary"></span>
      <span class="text-sm">{$_("categories.loading")}</span>
    </div>
  {:else if error}
    <div class="alert alert-error">
      <AlertTriangle class="w-5 h-5" />
      <span>{error}</span>
    </div>
  {:else if !activeBudget}
    <p class="text-sm text-base-content/50 py-4">
      {$_("commands.noActiveBudget")}
    </p>
  {:else if !html}
    <p class="text-sm text-base-content/50 py-4">
      {$_("commands.categoriesNone", { values: { name: activeBudget.name } })}
    </p>
  {:else}
    <!-- `res.html` is ONLY ever backend-built table markup from
         GET /budgets/:id/categories-table (the same endpoint the old
         /categories-list command used), never model free-text — {@html} is
         safe here for the same reason it was safe in the old chat bubble
         (see App.svelte's pre-#233 table_html comment). -->
    <div class="rounded-xl bg-base-100 border border-base-300 p-3">
      {@html html}
    </div>
  {/if}
</div>
```

- [ ] **Step 3: Verify the frontend still builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds (the component and locale JSON changes are syntactically valid; the
component isn't wired into `App.svelte` yet, so nothing renders it).

- [ ] **Step 4: Commit**

```bash
cd frontend
git add src/lib/CategoriesView.svelte src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json \
  src/lib/i18n/locales/es.json src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json \
  src/lib/i18n/locales/pt.json
git commit -m "feat(#233): add CategoriesView outlet component + i18n keys"
```

---

## Task 3: `Insights.svelte` — modal to outlet view

**Files:**
- Modify: `frontend/src/lib/Insights.svelte`

**Locate the blocks below by their literal code content**, not by line number (they may have
shifted from what's quoted here).

- [ ] **Step 1: Drop the `open` prop; load unconditionally on mount**

Find:
```svelte
  let { open = false, fetchApi, onClose } = $props();
```
Replace with:
```svelte
  let { fetchApi, onClose } = $props();
```

Find:
```js
  // Load whenever the panel opens or the period changes while open. The
  // component stays mounted (only {#if open} hides it), so move focus into the
  // dialog for keyboard/a11y users on open.
  $effect(() => {
    if (open) {
      load();
      setTimeout(() => closeButtonEl?.focus(), 50);
    }
  });
```
Replace with:
```js
  // Mounting IS the "open" signal now — the outlet's {#if route === "insights"}
  // in App.svelte owns mount/unmount (this component no longer gates its own
  // visibility). Re-load whenever `period` changes; run once unconditionally
  // at mount for the initial load, and move focus into the view for
  // keyboard/a11y users.
  $effect(() => {
    period;
    load();
  });

  $effect(() => {
    setTimeout(() => closeButtonEl?.focus(), 50);
  });
```

(Two separate `$effect`s: the first re-runs on every `period` change including the initial mount
— `load()` already reads `period` internally so this is the correct dependency; the second runs
exactly once at mount to move focus, matching the old on-open focus behavior without re-focusing on
every period change.)

- [ ] **Step 2: Replace the modal wrapper with a plain view container**

Find:
```svelte
{#if open}
  <div class="modal modal-open">
    <div
      class="modal-box bg-base-200 border border-base-300 max-w-3xl w-full max-h-[90dvh] overflow-y-auto"
      role="dialog"
      aria-modal="true"
      aria-labelledby="insights-dialog-title"
    >
```
Replace with:
```svelte
<div class="flex flex-col h-full">
  <div
    class="rounded-xl bg-base-200 border border-base-300 max-w-3xl w-full mx-auto p-4"
    aria-labelledby="insights-dialog-title"
  >
```

Find (the closing tags + backdrop at the end of the file):
```svelte
      <div class="modal-action">
        <button class="btn" onclick={() => onClose?.()}>
          {$_("insights.close")}
        </button>
      </div>
    </div>
    <button
      class="modal-backdrop"
      aria-label={$_("insights.dismiss")}
      onclick={() => onClose?.()}
    ></button>
  </div>
{/if}
```
Replace with:
```svelte
      <div class="flex justify-end mt-4">
        <button class="btn" onclick={() => onClose?.()}>
          {$_("insights.close")}
        </button>
      </div>
    </div>
  </div>
```

(This removes the `{#if open}` root gate entirely — the component's whole template is now
unconditional, since mount/unmount already gates it. It also removes the backdrop button, since
there's no longer an overlay to dismiss by clicking outside it — `onClose` is now only reachable
via the X icon and the "Done" button, both already wired.)

- [ ] **Step 3: Verify the build**

Run: `cd frontend && pnpm run build`
Expected: build succeeds. `App.svelte` still passes `open={showInsights}` to `<Insights>` at this
point — Svelte 5 silently ignores an extra prop the component no longer declares, so this is not
yet a build error (Task 4 removes the stale prop from the call site).

- [ ] **Step 4: Commit**

```bash
cd frontend
git add src/lib/Insights.svelte
git commit -m "feat(#233): convert Insights from a modal to an outlet view"
```

---

## Task 4: Wire the router outlet into `App.svelte`

**Files:**
- Modify: `frontend/src/App.svelte`

**Locate every block below by its literal code content, NOT by the stated line number** — App.svelte
is 2358 lines and earlier steps in this task shift later line numbers. Use `grep -n` on a
distinctive substring (e.g. `showInsights`, `chat-scroll-area`, `pushAiTable`) to re-locate each
block immediately before editing it.

- [ ] **Step 1: Add imports**

Find:
```js
  import Insights from "./lib/Insights.svelte";
```
Replace with:
```js
  import Insights from "./lib/Insights.svelte";
  import CategoriesView from "./lib/CategoriesView.svelte";
  import { ROUTES, routeFromHash, hashForRoute } from "./lib/router.js";
```

- [ ] **Step 2: Add `route` state and `navigate`/hash-sync helpers**

Find the existing modal-toggle state block:
```js
  let showDeleteAccount = $state(false);
  let showSettings = $state(false);
  let showInsights = $state(false);
```
Replace with:
```js
  let showDeleteAccount = $state(false);
  let showSettings = $state(false);
  // Governs #chat-scroll-area's content (#233): "chat" (default) | "categories"
  // | "insights". Synced to window.location.hash so browser back/forward and a
  // hard refresh on a deep link both work; see frontend/src/lib/router.js for
  // the pure hash<->route mapping.
  let route = $state("chat");

  function navigate(name) {
    route = ROUTES.includes(name) ? name : "chat";
    const hash = hashForRoute(route);
    if (window.location.hash !== hash) {
      if (hash) {
        window.location.hash = hash;
      } else {
        window.history.replaceState(
          null,
          "",
          window.location.pathname + window.location.search,
        );
      }
    }
  }

  // Browser back/forward. Only takes effect once the chat screen is showing —
  // the auth screen has no outlet to route.
  function handleHashChange() {
    if (activeScreen === "chat") {
      route = routeFromHash(window.location.hash);
    }
  }
```

- [ ] **Step 3: Initialize `route` from the hash at both `activeScreen = "chat"` sites**

Find (inside `saveSession`):
```js
    activeScreen = "chat";
    startFreshSession();
```
Replace with:
```js
    activeScreen = "chat";
    route = routeFromHash(window.location.hash);
    startFreshSession();
```

Find (inside `fetchMe`):
```js
      activeScreen = "chat";
      // Token auto-loaded from localStorage must NOT repopulate old messages.
      startFreshSession();
```
Replace with:
```js
      activeScreen = "chat";
      route = routeFromHash(window.location.hash);
      // Token auto-loaded from localStorage must NOT repopulate old messages.
      startFreshSession();
```

- [ ] **Step 4: Register the `hashchange` listener**

Find:
```svelte
<svelte:window onkeydown={handleKeydown} />
```
Replace with:
```svelte
<svelte:window onkeydown={handleKeydown} onhashchange={handleHashChange} />
```

- [ ] **Step 5: Update the Escape-key handler**

Find:
```js
    } else if (e.key === "Escape" && showInsights) {
      // Esc closes the insights panel (takes priority over the sidebar).
      showInsights = false;
    } else if (e.key === "Escape" && showDeleteAccount) {
```
Replace with:
```js
    } else if (e.key === "Escape" && route !== "chat") {
      // Esc returns to the chat view from the categories/insights outlet
      // (takes priority over the sidebar).
      navigate("chat");
    } else if (e.key === "Escape" && showDeleteAccount) {
```

- [ ] **Step 6: Update the focus-return guard**

Find:
```js
      await tick();
      if (!pendingDeletion && !showInsights) chatInputEl?.focus();
```
Replace with:
```js
      await tick();
      if (!pendingDeletion && route === "chat") chatInputEl?.focus();
```

- [ ] **Step 7: Delete `pushAiTable` and stop attaching `table_html` to chat messages**

Find:
```js
  // Push an AI message carrying a backend-built HTML table (#176). `tableHtml`
  // is ONLY ever set from backend-built table markup (the categories-table
  // endpoint or ChatResponse.categories_table_html) — never from model
  // free-text — because the bubble renders it via {@html}.
  function pushAiTable(text, tableHtml) {
    chatMessages = [
      ...chatMessages,
      {
        id: Math.random().toString(),
        sender: "ai",
        message_text: text,
        table_html: tableHtml,
        created_at: new Date().toISOString(),
      },
    ];
  }
```
Delete this function entirely (its only call site is removed in Step 9 below).

Find:
```js
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: chatRes.response,
          // Only ever the backend-built categories table HTML (#176); rendered
          // via {@html}. Never model free-text.
          table_html: chatRes.categories_table_html,
          created_at: new Date().toISOString(),
        },
      ];
```
Replace with:
```js
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: chatRes.response,
          created_at: new Date().toISOString(),
        },
      ];
```

- [ ] **Step 8: Route on `open_insights` / `categories_table_html` chat signals**

Find:
```js
      // Open the insights dialog when the assistant signals it (#157). Mirrors the
      // header-button and sidebar paths, which set the same state. Advisory only —
      // no data changes; this just surfaces the existing insights UI.
      if (chatRes.open_insights) {
        showInsights = true;
      }
```
Replace with:
```js
      // Navigate to the insights outlet when the assistant signals it (#157,
      // #233). Mirrors the sidebar path, which calls the same navigate().
      // Advisory only — no data changes; this just surfaces the existing
      // insights view.
      if (chatRes.open_insights) {
        navigate("insights");
      }

      // Navigate to the categories outlet when the assistant signals a
      // LIST_CATEGORIES turn (#233). The HTML itself is discarded here —
      // CategoriesView re-fetches its own fresh copy on mount, the same
      // convention Insights already uses (self-fetch via fetchApi, no data
      // via props).
      if (chatRes.categories_table_html) {
        navigate("categories");
      }
```

- [ ] **Step 9: Update the `/categories-list` command handler**

Find:
```js
    if (parsed.name === "/categories-list") {
      if (!activeBudget) {
        pushAiMessage(t("commands.noActiveBudget"));
        return;
      }
      isChatLoading = true;
      try {
        const res = await fetchApi(
          `/budgets/${activeBudget.id}/categories-table`,
        );
        if (!res || !res.html) {
          pushAiMessage(
            t("commands.categoriesNone", { values: { name: activeBudget.name } }),
          );
        } else {
          pushAiTable(
            t("commands.categoriesListHeader", {
              values: { name: activeBudget.name },
            }),
            res.html,
          );
        }
      } catch (err) {
        pushAiMessage(commandErrorMessage(err));
      } finally {
        isChatLoading = false;
      }
      return;
    }
```
Replace with:
```js
    if (parsed.name === "/categories-list") {
      if (!activeBudget) {
        pushAiMessage(t("commands.noActiveBudget"));
        return;
      }
      pushAiMessage(t("commands.categoriesOpened"));
      navigate("categories");
      return;
    }
```

(`CategoriesView` now owns the fetch, loading state, and empty-state copy — see Task 2 — so this
handler no longer needs `isChatLoading`, a try/catch, or `commands.categoriesListHeader`. Confirm
`commands.categoriesListHeader` has no other remaining callers before assuming it's now unused —
see Task 5 Step 2.)

- [ ] **Step 10: Update `/budgets-insights`**

Find:
```js
    if (parsed.name === "/budgets-insights") {
      showInsights = true;
      pushAiMessage(t("commands.insightsOpened"));
      return;
    }
```
Replace with:
```js
    if (parsed.name === "/budgets-insights") {
      navigate("insights");
      pushAiMessage(t("commands.insightsOpened"));
      return;
    }
```

- [ ] **Step 11: Update the sidebar's `onOpenInsights` callback**

Find:
```svelte
        onOpenInsights={() => {
          showInsights = true;
          closeSidebar();
        }}
```
Replace with:
```svelte
        onOpenInsights={() => {
          navigate("insights");
          closeSidebar();
        }}
```

- [ ] **Step 12: Remove `showInsights` from the `<main>` `inert` list**

Find:
```svelte
    <main
      inert={sidebarOpen || showSettings || showInsights ? true : undefined}
      class="flex-grow min-w-0 relative flex flex-col overflow-hidden"
    >
```
Replace with:
```svelte
    <main
      inert={sidebarOpen || showSettings ? true : undefined}
      class="flex-grow min-w-0 relative flex flex-col overflow-hidden"
    >
```

(Insights is no longer an overlay sitting on top of `<main>` — it's mounted inside it, in the
outlet — so there is nothing behind it left to `inert`.)

- [ ] **Step 13: Move `<Insights>` out of the top-level overlay block; mount the outlet**

Find the top-level `<Insights>` mount (a sibling of `<Settings>`, before the app shell root div):
```svelte
<Insights
  open={showInsights}
  {fetchApi}
  onClose={() => (showInsights = false)}
/>

<div class="flex flex-col bg-base-100 h-dvh">
```
Delete the `<Insights ... />` block here (it moves into the outlet below), leaving:
```svelte
<div class="flex flex-col bg-base-100 h-dvh">
```

Find the `#chat-scroll-area` div and its content:
```svelte
        <div
          id="chat-scroll-area"
          class="flex-grow p-4 md:p-6 overflow-y-auto space-y-4 bg-base-100/40"
          onscroll={handleChatScroll}
        >
          {#if chatMessages.length === 0}
```
Replace with:
```svelte
        <div
          id="chat-scroll-area"
          class="flex-grow p-4 md:p-6 overflow-y-auto space-y-4 bg-base-100/40"
          onscroll={handleChatScroll}
        >
          {#if route === "categories"}
            <CategoriesView {fetchApi} {activeBudget} onBack={() => navigate("chat")} />
          {:else if route === "insights"}
            <Insights {fetchApi} onClose={() => navigate("chat")} />
          {:else if chatMessages.length === 0}
```

Now find the matching close of the original `{#if chatMessages.length === 0}...{:else}...{/if}`
block (it ends right before the loading-indicator `{#if isChatLoading}` block, i.e. still inside
`#chat-scroll-area`):
```svelte
            {/each}
          {/if}

          {#if isChatLoading}
```
This `{/if}` still correctly closes the (now 3-way) `{#if route === "categories"} ... {:else if
route === "insights"} ... {:else if chatMessages.length === 0} ... {:else} ... {/if}` chain — no
further change needed here, since Svelte's `{#if}/{:else if}/{:else}/{/if}` is a single block
regardless of how many `{:else if}` branches it has. Leave `{#if isChatLoading}` and everything
after it untouched — it renders alongside (after) the outlet's `{/if}`, not inside it, exactly as
it does today for the plain-chat case.

- [ ] **Step 14: Verify the build**

Run: `cd frontend && pnpm run build`
Expected: build succeeds with no errors (no more `open`/`showInsights` prop mismatch, no unused
`pushAiTable` reference).

- [ ] **Step 15: Manual verification**

Run: `cd frontend && pnpm run dev`

Using a logged-in session with at least one budget that has some categories:

1. Type `/categories-list` — confirm it navigates to a dedicated categories view in the main
   content area (not a chat bubble with an embedded table), and the URL hash becomes
   `#/categories`.
2. Ask the assistant something that triggers a categories listing (a LIST_CATEGORIES-style
   request, e.g. "what categories do I have?") — confirm the same categories view opens (not a
   chat bubble).
3. Type `/budgets-insights` — confirm it navigates to the insights view in the main content area
   (no modal/backdrop), URL hash becomes `#/insights`.
4. Open the sidebar and click its Insights entry — confirm the same insights view opens.
5. Ask the assistant something that triggers `open_insights` (e.g. a spending-summary request) —
   confirm the insights view opens.
6. From either view, click its "back"/"close" affordance and confirm it returns to the normal chat
   conversation view, and the hash clears.
7. From either view, press Escape and confirm it returns to chat.
8. Send several chat messages, then navigate `categories -> chat -> insights -> chat` — confirm the
   chat history/scroll position is preserved throughout (no reload, no lost messages).
9. While on `#/insights` or `#/categories`, use the browser's back button — confirm it returns to
   chat; forward returns to the same view. Hard-refresh the page while on `#/insights` — confirm it
   deep-links back into the insights view after auth restores.
10. Confirm the header, budget status strip, and bottom prompt/input bar look and behave exactly as
    before, in every one of the three outlet states.

- [ ] **Step 16: Run the full test suite**

Run: `cd frontend && pnpm test`
Expected: PASS — all existing tests plus the new `router.test.js` suite green.

- [ ] **Step 17: Commit**

```bash
cd frontend
git add src/App.svelte
git commit -m "feat(#233): wire the router outlet into App.svelte"
```

---

## Task 5: Final pass

- [ ] **Step 1: Re-read the diff against the AC**

Run: `git diff origin/main...HEAD --stat` and `git diff origin/main...HEAD` from the worktree root.

Confirm against savvagent/nels#233's acceptance criteria:
- [ ] A client-side route-state mechanism governs the outlet, without disturbing the
      header/budget-banner/prompt regions. (Task 1, Task 4 Steps 2-4, 13)
- [ ] `/categories-list` and a categories chat intent both navigate to categories and render in the
      outlet, replacing the old `table_html`-in-chat-bubble behavior. (Task 2, Task 4 Steps 7-9)
- [ ] Insights (chat message, slash command, or sidebar button) navigates to insights and renders
      in the outlet, replacing the modal. (Task 3, Task 4 Steps 8, 10-11, 13)
- [ ] A normal chat reply renders in the outlet via the existing chat view; `chatWindow.js`
      windowing keeps working (untouched — Task 4 Step 13 only adds two new branches ahead of the
      existing `{#if chatMessages.length === 0}...{:else}...` chain).
- [ ] Switching routes doesn't lose in-flight chat state or reload. (Task 4 Step 15.8-9;
      `chatMessages`/`showAllMessages` are never touched by any change in this plan)
- [ ] All existing trigger points (slash commands, backend action fields, sidebar button) route
      correctly. (Task 4 Steps 8-11)

- [ ] **Step 2: Confirm no orphaned i18n keys or unused code**

Run: `grep -rn "categoriesListHeader" frontend/src/` — if this returns no hits (the old
`/categories-list` handler was its only caller and Task 4 Step 9 removed it), remove the
`commands.categoriesListHeader` key from all 6 locale files in this step; if it has other callers,
leave it untouched.

Run: `grep -n "showInsights\|pushAiTable\|table_html\|open={" frontend/src/App.svelte` — expect no
`showInsights` or `pushAiTable` hits at all, and no `msg.table_html` hits in the template. `open={`
should only match unrelated props if any remain (verify by eye — there should be no `open={` left
for `Insights` or `Settings`... note `Settings` still legitimately uses `open={showSettings}`, that
one stays).

Run: `grep -n "modal-open\|modal-backdrop\|aria-modal" frontend/src/lib/Insights.svelte` — expect no
hits (fully converted from modal to plain view).

- [ ] **Step 3: Final full check**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: both succeed.
