# App Localization (i18n) — Design Spec

**Date:** 2026-06-10
**Status:** Approved, ready for implementation planning
**Scope:** Localize the Nels frontend UI into 6 languages and have the AI
assistant reply in the user's chosen language. Frontend: add `svelte-i18n`,
extract all hardcoded UI strings into message catalogs, add a header language
picker. Backend: thread an optional `locale` through `/chat` and
`/suggested-question` into the Gemini system prompt.

## 1. Goals & Non-Goals

**Goals**
- Users can switch the entire UI between English, Spanish, French, German,
  Italian, and Portuguese via a picker in the header.
- The chosen language persists across reloads and sets `<html lang>`.
- The AI assistant writes its user-facing replies and suggested questions in the
  chosen language; database action keys and JSON structure stay English.
- Sidebar relative/absolute dates format in the active locale.

**Non-Goals (YAGNI)**
- No right-to-left languages (all six are LTR).
- No per-message language override — language is a single app-wide setting.
- No translation of user-generated content (budget names, transaction notes).
- No translation-management tooling/CI; catalogs are plain JSON in-repo.
- No regional variants beyond a single generic Portuguese (`pt`).
- No automated test harness (none exists in the repo); verification is manual.

## 2. Decisions (from brainstorming)

| Decision | Choice | Rationale |
|---|---|---|
| Scope | **UI chrome + AI replies** | Full localization; backend passes locale to the LLM prompt. |
| Library | **svelte-i18n** | User preference; ICU formatting, mature Svelte integration. |
| Locales | **en, es, fr, de, it, pt** | English is source/fallback; `pt` is generic Portuguese. |
| Picker UI | **daisyUI dropdown** | 6 options is too many for ThemePicker's inline button row. |
| Picker placement | **Header RIGHT cluster, beside ThemePicker** | Always visible on auth + chat screens. |
| Message loading | **Synchronous `addMessages`** | Avoids flash-of-keys; catalogs imported statically. |
| Default locale | **localStorage → navigator.language → en** | Respects prior choice, then browser, then fallback. |
| AI action keys | **Stay English** | They are machine identifiers parsed by the backend, not shown to users. |

## 3. Frontend Architecture

### 3.1 i18n module — `src/lib/i18n/index.js`
- Imports the six locale JSON catalogs statically and registers them with
  `addMessages(code, dict)` (synchronous — no async loaders, so no loading flash).
- Exports `STORAGE_KEY = "nels_lang"`, `LOCALES` (ordered list of
  `{ code, label }` endonyms), `getStoredLocale()`, and `setLocale(code)`.
- `getStoredLocale()`: returns the persisted code if supported, else the base of
  `navigator.language` if supported, else `"en"`.
- `setLocale(code)`: validates, calls svelte-i18n `locale.set(code)`, persists to
  `localStorage["nels_lang"]`, and sets `document.documentElement.lang = code`.
- `init({ fallbackLocale: "en", initialLocale: getStoredLocale() })` is called at
  module load so the store is ready before first render.

This mirrors the existing `src/lib/theme.js` contract (storage key + getter +
applier) for consistency.

### 3.2 Bootstrap — `src/main.js`
Import `./lib/i18n/index.js` **before** mounting the app, and set the initial
`<html lang>` from the stored locale, so the first paint is already localized.

### 3.3 Message catalogs — `src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`
Nested keys grouped by surface. Indicative structure (en is authoritative):

```
{
  "header": { "tagline": "Smart Budgeting" },
  "auth": {
    "heroTitle": "Nels thinks about your money so you don't have to.",
    "heroSubtitle": "Your authenticator-secured AI budgeting fiend.",
    "emailLabel": "Email Address",
    "emailPlaceholder": "name@company.com",
    "signIn": "Sign In",
    "registerCta": "Register with Authenticator App",
    "toggleToRegister": "New to Nels? Register a new account",
    "toggleToSignIn": "Already have an account? Sign In",
    "totpInfo": "Nels uses TOTP authenticator codes. No password! ...",
    "regSuccessTitle": "Registration Successful!",
    "continueToDashboard": "Continue to Dashboard",
    "qrInstructions": "Scan this QR code with an authenticator app ...",
    "codeLabel": "6-digit code", "verify": "Verify"
  },
  "chat": {
    "welcomeNamed": "Hi {name}! I'm Nels, your personal budgeting assistant. ...",
    "welcomeAnon": "Hi there! I'm Nels ... what should I call you?",
    "inputPlaceholder": "Message Nels...", "send": "Send"
  },
  "sidebar": {
    "newConversation": "New conversation", "recent": "Recent",
    "rename": "Rename", "delete": "Delete", "logout": "Log out",
    "emptyState": "No conversations yet"
  },
  "theme": { "light": "Light", "system": "System", "dark": "Dark" },
  "language": { "label": "Language" },
  "alerts": { "genericError": "Something went wrong.", ... }
}
```

ICU placeholders (`{name}`) are used for interpolated strings. The exact key set
is finalized during implementation by extracting every literal from
`App.svelte`, `Sidebar.svelte`, and `ThemePicker.svelte` (including
`aria-label`, `placeholder`, `title`, `alt`). The non-English catalogs are
machine-generated by the implementer and flagged for native review.

### 3.4 Language picker — `src/lib/LanguagePicker.svelte`
- daisyUI `dropdown` (`dropdown-end`): trigger is a `btn btn-ghost btn-xs` with a
  `Globe` icon (lucide) and the current language endonym/short code.
- Menu (`menu` in a `dropdown-content`) lists `LOCALES`; the active one shows a
  `Check` icon. Selecting calls `setLocale(code)` and closes the menu.
- `aria-label` from `$_('language.label')`; each item labeled by its endonym.

### 3.5 Component changes
- **App.svelte** — replace every hardcoded UI literal with `$_('...')`; mount
  `<LanguagePicker />` in the RIGHT cluster beside `<ThemePicker />`. Welcome
  bubble uses `$_('chat.welcomeNamed', { values: { name } })` / `welcomeAnon`.
- **Sidebar.svelte** — labels via `$_`; `toLocaleDateString` switches from
  `undefined` to the active locale code.
- **ThemePicker.svelte** — option labels via `$_('theme.*')` (values stay
  `light`/`system`/`dark`).

## 4. Backend Architecture (`backend/src/rag.rs`)

### 4.1 Locale → language map
A small helper `language_name(locale: Option<&str>) -> Option<&'static str>`
mapping the base code to an English language name (`es`→"Spanish", `fr`→"French",
`de`→"German", `it`→"Italian", `pt`→"Portuguese"). `en`, `None`, or unknown →
`None` (English, current behavior).

### 4.2 Chat endpoint
- Add `pub locale: Option<String>` to `ChatRequest`.
- Build a `language_context` string (empty when `None`):
  *"Always write your user-facing reply text in {Language}. Do NOT translate the
  JSON field names or action identifiers — those stay exactly as specified."*
- Inject `language_context` into the existing `system_instructions` `format!`
  (alongside `name_context`).

### 4.3 Suggested-question endpoint
- Read an optional `?locale=` query param.
- When a language maps, append an instruction to the suggestion prompt so the
  generated question is in that language.

### 4.4 Compatibility
Absent/`en`/unknown locale reproduces today's behavior exactly. No DB schema
change — locale is request-scoped, not persisted server-side.

## 5. Frontend → Backend wiring
- `/chat` POST body gains `locale: <current base code>`.
- `/suggested-question` GET gains `?locale=<current base code>`.
- The base code is derived from svelte-i18n's active `locale` (strip any region).

## 6. Error handling
- Unsupported/garbled stored locale → fall back to `en` (getter validates).
- `localStorage` unavailable (private mode) → locale still applies in-memory;
  persistence is best-effort (mirrors `theme.js`).
- Missing translation key → svelte-i18n falls back to `en`, then to the key
  itself; `en` is kept complete so users never see raw keys.
- Backend locale unknown → English reply (no error surfaced).

## 7. Testing / Verification
- `pnpm build` (frontend) and `cargo build` (backend) succeed.
- Dev-server visual check: switch through at least English + two other languages
  on both the auth screen and the chat screen; confirm no raw keys, layout holds.
- Manual chat check: with a non-English locale selected, confirm the assistant's
  reply text comes back in that language while actions still execute.

## 8. Files

**New**
- `frontend/src/lib/i18n/index.js`
- `frontend/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`
- `frontend/src/lib/LanguagePicker.svelte`

**Modified**
- `frontend/package.json` (+`svelte-i18n`)
- `frontend/src/main.js`
- `frontend/src/App.svelte`
- `frontend/src/lib/Sidebar.svelte`
- `frontend/src/lib/ThemePicker.svelte`
- `frontend/index.html` (initial `lang` attribute, optional)
- `backend/src/rag.rs`
