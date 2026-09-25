# daisyUI Light/Dark Theme Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Light / Dark / System theme support to the Nels frontend with a picker in the chat status strip, backed by two custom daisyUI v5 themes, with the existing hardcoded-slate UI converted to semantic daisyUI tokens so light mode genuinely looks light.

**Architecture:** Two custom daisyUI themes (`nels-light`, `nels-dark`) are declared in `app.css`. A pure helper module (`src/lib/theme.js`) persists the user's choice in `localStorage` and applies it by setting `data-theme` on `<html>` — except "System", which *removes* the attribute and lets daisyUI's `--default`/`--prefersdark` resolve from the OS automatically (no `matchMedia` needed, and it live-updates on OS change). A pre-paint inline script in `index.html` applies the saved choice before first render to avoid a flash. The two Svelte components are refactored from hardcoded slate/indigo/purple utility classes to semantic tokens (`base-*`, `primary`, `secondary`, `success`, etc.) so brand accents adapt per theme.

**Tech Stack:** Svelte 5 (runes), Tailwind CSS v4, daisyUI v5, lucide-svelte, Vite. Vitest + jsdom added for unit-testing the theme helper.

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `frontend/src/lib/theme.js` | Pure theme state: storage key, theme list, read/apply choice to DOM | Create |
| `frontend/src/lib/theme.test.js` | Unit tests for `theme.js` | Create |
| `frontend/vite.config.js` | Add Vitest config (jsdom env) | Modify |
| `frontend/package.json` | Add `vitest`, `jsdom` devDeps + `test` script | Modify |
| `frontend/src/app.css` | Register + define `nels-light` / `nels-dark` themes; drop forced body colors | Modify |
| `frontend/index.html` | Remove hardcoded `data-theme`; add pre-paint init script | Modify |
| `frontend/src/lib/ThemePicker.svelte` | 3-way segmented Light/System/Dark control | Create |
| `frontend/src/App.svelte` | Mount picker in status strip; refactor slate→tokens | Modify |
| `frontend/src/lib/Sidebar.svelte` | Refactor slate→tokens | Modify |

---

## Canonical Color Mapping Table

Both refactor tasks (Task 4, Task 5) apply **this exact table**. Brand accents map to theme tokens (so they adapt per theme, as chosen). Apply class-by-class; preserve any opacity/hover/state prefixes (e.g. `hover:`, `/60`).

| Hardcoded class | → Semantic token |
|---|---|
| `bg-slate-900` | `bg-base-100` |
| `bg-slate-950` | `bg-base-200` |
| `bg-slate-800` | `bg-base-300` |
| `border-slate-800`, `border-slate-700` | `border-base-300` |
| `divide-slate-800` | `divide-base-300` |
| `text-white` *on a primary/secondary/gradient background* (buttons, user chat bubble, brand pills) | `text-primary-content` |
| `text-white` *standalone / on base surfaces* | `text-base-content` |
| `text-slate-100` | `text-base-content` |
| `text-slate-200`, `text-slate-300` | `text-base-content/80` |
| `text-slate-400`, `text-slate-500`, `text-slate-600`, `text-slate-700` | `text-base-content/60` |
| `bg-indigo-600`, `bg-indigo-500` | `bg-primary` |
| `text-indigo-400`, `text-indigo-300` | `text-secondary` |
| `border-indigo-500` | `border-secondary` |
| `from-purple-700`, `from-purple-600`, `from-purple-400` | `from-primary` |
| `to-indigo-600`, `to-indigo-500`, `to-indigo-300` | `to-secondary` |
| `text-purple-400`, `text-purple-300` | `text-primary` |
| `border-purple-500` | `border-primary` |
| `hover:border-purple-500/60` | `hover:border-primary/60` |
| `text-emerald-400`, `text-emerald-500` | `text-success` |
| `border-emerald-500` | `border-success` |
| `bg-emerald-*` | `bg-success` |
| `text-red-*`, `bg-red-*`, `border-red-*` | `error` equivalents (`text-error`, `bg-error`, `border-error`) |
| `text-green-*` | `text-success` |

**Exceptions — do NOT convert:**
- `bg-white` on the **QR-code container** stays `bg-white` (a QR must render on a fixed white background to scan reliably in any theme). If `bg-white` appears anywhere else, convert to `bg-base-100`.

**Refactor verification (used in Tasks 4 & 5):** after converting a file, this must return **no matches**:
```bash
grep -nE '\b(bg|text|border|from|to|via|ring|divide|placeholder|fill)-(slate|indigo|purple|emerald|zinc|gray)-[0-9]' <file>
```
(`bg-white` on the QR container is the only allowed literal-color leftover.)

---

### Task 1: Theme helper module (`theme.js`) — TDD

**Files:**
- Modify: `frontend/package.json`
- Modify: `frontend/vite.config.js`
- Create: `frontend/src/lib/theme.js`
- Test: `frontend/src/lib/theme.test.js`

- [ ] **Step 1: Add test tooling deps**

Run:
```bash
cd frontend && pnpm add -D vitest jsdom
```
Expected: `vitest` and `jsdom` appear under `devDependencies` in `package.json`.

- [ ] **Step 2: Add `test` script to `package.json`**

In `frontend/package.json`, add to `"scripts"`:
```json
    "test": "vitest run"
```

- [ ] **Step 3: Configure Vitest (jsdom env) in `vite.config.js`**

Replace the contents of `frontend/vite.config.js` with:
```js
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  server: {
    port: 5173,
    host: true, // Listen on all interfaces (LAN access to the dev server)
  },
  test: {
    environment: 'jsdom',
  },
})
```

- [ ] **Step 4: Write the failing tests**

Create `frontend/src/lib/theme.test.js`:
```js
import { beforeEach, describe, expect, test } from "vitest";
import { STORAGE_KEY, THEMES, getStoredTheme, applyTheme } from "./theme.js";

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
});

describe("getStoredTheme", () => {
  test("defaults to 'system' when nothing is stored", () => {
    expect(getStoredTheme()).toBe("system");
  });

  test("returns a valid stored choice", () => {
    localStorage.setItem(STORAGE_KEY, "dark");
    expect(getStoredTheme()).toBe("dark");
  });

  test("falls back to 'system' for an invalid stored value", () => {
    localStorage.setItem(STORAGE_KEY, "banana");
    expect(getStoredTheme()).toBe("system");
  });
});

describe("applyTheme", () => {
  test("light sets data-theme=nels-light and persists", () => {
    applyTheme("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("nels-light");
    expect(localStorage.getItem(STORAGE_KEY)).toBe("light");
  });

  test("dark sets data-theme=nels-dark and persists", () => {
    applyTheme("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("nels-dark");
    expect(localStorage.getItem(STORAGE_KEY)).toBe("dark");
  });

  test("system removes the attribute and persists 'system'", () => {
    applyTheme("light"); // set something first
    applyTheme("system");
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
    expect(localStorage.getItem(STORAGE_KEY)).toBe("system");
  });

  test("an invalid choice falls back to system", () => {
    const result = applyTheme("banana");
    expect(result).toBe("system");
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  });

  test("THEMES is the picker order light, system, dark", () => {
    expect(THEMES).toEqual(["light", "system", "dark"]);
  });
});
```

- [ ] **Step 5: Run the tests to verify they fail**

Run: `cd frontend && pnpm test`
Expected: FAIL — `Failed to resolve import "./theme.js"` (module not created yet).

- [ ] **Step 6: Implement `theme.js`**

Create `frontend/src/lib/theme.js`:
```js
// Theme management for Nels: persists the user's choice and applies it to the
// document via daisyUI's data-theme attribute.
//
//   "light"  -> data-theme="nels-light"
//   "dark"   -> data-theme="nels-dark"
//   "system" -> no data-theme attribute; daisyUI auto-resolves from the OS
//               (--default = nels-light, --prefersdark = nels-dark)

export const STORAGE_KEY = "nels_theme";

// Order the picker displays: light, system, dark.
export const THEMES = ["light", "system", "dark"];

const THEME_ATTR = {
  light: "nels-light",
  dark: "nels-dark",
};

// Read the saved choice, defaulting to "system" when absent or invalid.
export function getStoredTheme() {
  const stored =
    typeof localStorage !== "undefined" ? localStorage.getItem(STORAGE_KEY) : null;
  return THEMES.includes(stored) ? stored : "system";
}

// Apply a theme choice to <html> and persist it. "system" removes the
// attribute so daisyUI's prefers-color-scheme resolution takes over.
export function applyTheme(theme) {
  const choice = THEMES.includes(theme) ? theme : "system";
  const root = document.documentElement;
  const attr = THEME_ATTR[choice];
  if (attr) {
    root.setAttribute("data-theme", attr);
  } else {
    root.removeAttribute("data-theme");
  }
  try {
    localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode); theme still applies.
  }
  return choice;
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cd frontend && pnpm test`
Expected: PASS — all 8 tests green.

- [ ] **Step 8: Commit**

```bash
git add frontend/package.json frontend/pnpm-lock.yaml frontend/vite.config.js frontend/src/lib/theme.js frontend/src/lib/theme.test.js
git commit -m "feat(theme): add theme helper module with vitest coverage"
```

---

### Task 2: Define daisyUI themes + pre-paint init

**Files:**
- Modify: `frontend/src/app.css`
- Modify: `frontend/index.html`

- [ ] **Step 1: Replace `app.css` with theme registration + custom themes**

Replace the entire contents of `frontend/src/app.css` with:
```css
@import "tailwindcss";
@plugin "daisyui" {
  themes: nels-light --default, nels-dark --prefersdark;
}

/* Light theme — purple/indigo brand on a clean light base. */
@plugin "daisyui/theme" {
  name: "nels-light";
  default: true;
  prefersdark: false;
  color-scheme: light;

  --color-base-100: oklch(99% 0.003 264);
  --color-base-200: oklch(96% 0.006 264);
  --color-base-300: oklch(91% 0.01 264);
  --color-base-content: oklch(25% 0.03 264);

  --color-primary: oklch(55% 0.26 300);
  --color-primary-content: oklch(98% 0.01 300);

  --color-secondary: oklch(52% 0.2 277);
  --color-secondary-content: oklch(98% 0.01 277);

  --color-accent: oklch(60% 0.2 300);
  --color-accent-content: oklch(98% 0.01 300);

  --color-neutral: oklch(35% 0.03 264);
  --color-neutral-content: oklch(98% 0.01 264);

  --color-info: oklch(65% 0.18 230);
  --color-info-content: oklch(98% 0.01 230);

  --color-success: oklch(60% 0.16 160);
  --color-success-content: oklch(98% 0.01 160);

  --color-warning: oklch(80% 0.16 80);
  --color-warning-content: oklch(25% 0.05 80);

  --color-error: oklch(58% 0.24 25);
  --color-error-content: oklch(98% 0.01 25);

  --radius-selector: 0.5rem;
  --radius-field: 0.5rem;
  --radius-box: 0.75rem;
  --border: 1px;
}

/* Dark theme — tuned to preserve the existing slate-900/950 look. */
@plugin "daisyui/theme" {
  name: "nels-dark";
  default: false;
  prefersdark: true;
  color-scheme: dark;

  --color-base-100: oklch(20% 0.03 264);
  --color-base-200: oklch(16% 0.026 264);
  --color-base-300: oklch(27% 0.035 264);
  --color-base-content: oklch(96% 0.01 264);

  --color-primary: oklch(70% 0.24 300);
  --color-primary-content: oklch(16% 0.03 300);

  --color-secondary: oklch(62% 0.2 277);
  --color-secondary-content: oklch(98% 0.01 277);

  --color-accent: oklch(78% 0.16 300);
  --color-accent-content: oklch(20% 0.04 300);

  --color-neutral: oklch(30% 0.03 264);
  --color-neutral-content: oklch(96% 0.01 264);

  --color-info: oklch(72% 0.16 230);
  --color-info-content: oklch(16% 0.03 230);

  --color-success: oklch(76% 0.17 160);
  --color-success-content: oklch(18% 0.04 160);

  --color-warning: oklch(82% 0.17 80);
  --color-warning-content: oklch(20% 0.05 80);

  --color-error: oklch(68% 0.23 25);
  --color-error-content: oklch(18% 0.04 25);

  --radius-selector: 0.5rem;
  --radius-field: 0.5rem;
  --radius-box: 0.75rem;
  --border: 1px;
}

:root {
  font-family: system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, sans-serif;
}

/* Let daisyUI's active theme drive the page background and text colour.
   (Previously these were hardcoded to slate, which forced dark in every theme.) */
body {
  margin: 0;
  padding: 0;
  min-height: 100vh;
  background-color: var(--color-base-100);
  color: var(--color-base-content);
}
```

- [ ] **Step 2: Update `index.html` — remove forced theme, add pre-paint init**

In `frontend/index.html`:
1. Change the opening html tag from `<html lang="en" data-theme="dark">` to `<html lang="en">`.
2. Add this script inside `<head>`, **before** the `<script type="module" src="/src/main.js">` tag (so the attribute is set before first paint):
```html
    <script>
      // Apply the saved theme before paint to avoid a flash of the wrong theme.
      // Mirrors src/lib/theme.js: "light"/"dark" set data-theme; "system" (or
      // unset) leaves it off so daisyUI resolves from prefers-color-scheme.
      (function () {
        try {
          var t = localStorage.getItem("nels_theme");
          if (t === "light")
            document.documentElement.setAttribute("data-theme", "nels-light");
          else if (t === "dark")
            document.documentElement.setAttribute("data-theme", "nels-dark");
        } catch (e) {}
      })();
    </script>
```

- [ ] **Step 3: Verify the build compiles with the new themes**

Run: `cd frontend && pnpm build`
Expected: build succeeds; output CSS includes the daisyUI themes (no `@plugin` parse errors).

- [ ] **Step 4: Commit**

```bash
git add frontend/src/app.css frontend/index.html
git commit -m "feat(theme): define nels-light/nels-dark daisyUI themes + pre-paint init"
```

---

### Task 3: Refactor `App.svelte` to semantic tokens

**Files:**
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Apply the Canonical Color Mapping Table to `App.svelte`**

Convert every hardcoded color utility in `frontend/src/App.svelte` per the **Canonical Color Mapping Table** at the top of this plan. Work through the file top to bottom. Key judgement calls:
- `text-white` inside buttons/pills/user-chat-bubble that sit on `bg-primary`/`bg-secondary`/brand gradients → `text-primary-content`. Standalone `text-white` → `text-base-content`.
- The Nels brand wordmark gradient (`bg-clip-text` with `from-purple-* to-indigo-*`) → `from-primary to-secondary`.
- The user chat bubble currently `bg-indigo-600 text-white` → `bg-primary text-primary-content`.
- The QR-code container `bg-white` stays `bg-white` (scannability). Leave it untouched.

- [ ] **Step 2: Verify no hardcoded colors remain**

Run:
```bash
cd frontend && grep -nE '\b(bg|text|border|from|to|via|ring|divide|placeholder|fill)-(slate|indigo|purple|emerald|zinc|gray)-[0-9]' src/App.svelte
```
Expected: no output (empty). The only allowed literal-color class is `bg-white` on the QR container.

- [ ] **Step 3: Verify the build compiles**

Run: `cd frontend && pnpm build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "refactor(ui): convert App.svelte to semantic daisyUI tokens"
```

---

### Task 4: Refactor `Sidebar.svelte` to semantic tokens

**Files:**
- Modify: `frontend/src/lib/Sidebar.svelte`

- [ ] **Step 1: Apply the Canonical Color Mapping Table to `Sidebar.svelte`**

Convert every hardcoded color utility in `frontend/src/lib/Sidebar.svelte` per the **Canonical Color Mapping Table**. Same judgement rules as Task 3 (active/selected items on `bg-primary` use `text-primary-content`; muted timestamps/labels use `text-base-content/60`).

- [ ] **Step 2: Verify no hardcoded colors remain**

Run:
```bash
cd frontend && grep -nE '\b(bg|text|border|from|to|via|ring|divide|placeholder|fill)-(slate|indigo|purple|emerald|zinc|gray)-[0-9]' src/lib/Sidebar.svelte
```
Expected: no output (empty).

- [ ] **Step 3: Verify the build compiles**

Run: `cd frontend && pnpm build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/lib/Sidebar.svelte
git commit -m "refactor(ui): convert Sidebar.svelte to semantic daisyUI tokens"
```

---

### Task 5: Theme picker component + mount in status strip

**Files:**
- Create: `frontend/src/lib/ThemePicker.svelte`
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Create `ThemePicker.svelte`**

Create `frontend/src/lib/ThemePicker.svelte`:
```svelte
<script>
  import { Sun, Monitor, Moon } from "lucide-svelte";
  import { getStoredTheme, applyTheme } from "./theme.js";

  // Highlight reflects the persisted choice; the pre-paint script already
  // applied it to <html>, so we only re-apply on user interaction.
  let current = $state(getStoredTheme());

  const OPTIONS = [
    { value: "light", label: "Light", Icon: Sun },
    { value: "system", label: "System", Icon: Monitor },
    { value: "dark", label: "Dark", Icon: Moon },
  ];

  function choose(value) {
    current = applyTheme(value);
  }
</script>

<div class="join" role="group" aria-label="Theme">
  {#each OPTIONS as opt}
    <button
      type="button"
      class="join-item btn btn-xs {current === opt.value
        ? 'btn-primary'
        : 'btn-ghost text-base-content/60'}"
      aria-pressed={current === opt.value}
      aria-label={opt.label}
      title={opt.label}
      onclick={() => choose(opt.value)}
    >
      <opt.Icon class="w-3.5 h-3.5" />
    </button>
  {/each}
</div>
```

- [ ] **Step 2: Import `ThemePicker` in `App.svelte`**

In `frontend/src/App.svelte`, add to the existing component imports (next to `import Sidebar from "./lib/Sidebar.svelte";`):
```js
  import ThemePicker from "./lib/ThemePicker.svelte";
```

- [ ] **Step 3: Mount the picker in the status strip**

In `frontend/src/App.svelte`, locate the status micro-strip — the `flex items-center justify-between` bar containing the "Nels ready" indicator (originally near line 835, classes now token-converted from Task 3). It currently has the status indicator as its only child, so `justify-between` has nothing on the right. Add the picker as the second child, immediately after the closing `</div>` of the "Nels ready" group and before the strip's closing `</div>`:
```svelte
          <ThemePicker />
```
Result: status indicator on the left, theme picker on the right.

- [ ] **Step 4: Verify the build compiles**

Run: `cd frontend && pnpm build`
Expected: build succeeds.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/ThemePicker.svelte frontend/src/App.svelte
git commit -m "feat(theme): add Light/System/Dark picker to chat status strip"
```

---

### Task 6: Manual verification + color tuning

**Files:**
- Possibly modify: `frontend/src/app.css` (oklch tuning only)

- [ ] **Step 1: Run the app**

Run: `cd frontend && pnpm dev` and open the printed URL. (Backend not required for visual theme checks; the auth screen alone exercises most tokens.)

- [ ] **Step 2: Walk the verification checklist**

Confirm each:
- [ ] **Dark** selected → matches the previous look (deep slate surfaces, light text, purple/indigo brand accents).
- [ ] **Light** selected → page and surfaces are genuinely light; text is dark and readable; **no element is left dark-slate** (scan auth screen, chat welcome, suggested chip, input bar, status strip, and open the sidebar drawer).
- [ ] **System** selected → matches the OS appearance; toggling the OS light/dark setting flips the app live with no reload.
- [ ] Reload the page on each choice → the theme persists with **no flash** of the wrong theme on load.
- [ ] The active picker segment is highlighted (`btn-primary`) and switching is instant.
- [ ] Brand accents (Nels wordmark gradient, suggested-chip sparkle, primary buttons, user chat bubble) read correctly in both themes — readable contrast, recognizably purple/indigo.
- [ ] The QR-code container (registration flow) stays on a white background.

- [ ] **Step 3: Tune oklch values if needed**

If any surface/accent looks off (e.g. light-mode contrast too low, dark base too flat), adjust the corresponding `--color-*` oklch values in the `nels-light` / `nels-dark` blocks in `frontend/src/app.css`. Re-run `pnpm build` and re-check. Keep edits limited to color values.

- [ ] **Step 4: Commit any tuning**

```bash
git add frontend/src/app.css
git commit -m "style(theme): tune nels-light/nels-dark color values"
```
(Skip this commit if no tuning was needed.)

---

## Done When

- `pnpm test` passes (theme helper).
- `pnpm build` is clean.
- The grep guard returns empty for both `App.svelte` and `Sidebar.svelte` (only the QR `bg-white` remains).
- Light / Dark / System all work, persist across reload without flash, follow the OS in System mode, and every screen is fully themed.
