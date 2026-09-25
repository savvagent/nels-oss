# App Localization (i18n) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Localize the Nels UI into English, Spanish, French, German, Italian, and Portuguese with a header language picker, and have the AI assistant reply in the chosen language.

**Architecture:** Frontend adds `svelte-i18n` with statically-registered JSON catalogs and a `src/lib/i18n/index.js` module (mirroring `theme.js`). A `LanguagePicker.svelte` dropdown sits in the header beside `ThemePicker`. The chosen base locale code is sent to the Rust backend on `/chat` (POST body) and `/suggested-question` (query param); the backend injects a language instruction into the Gemini system prompt so prose localizes while JSON action keys stay English.

**Tech Stack:** Svelte 5 (runes), Vite, daisyUI 5, svelte-i18n, lucide-svelte; Rust (axum) backend calling Gemini 2.5-flash.

**Spec:** `docs/superpowers/specs/2026-06-10-app-localization-design.md`

**Verification note:** This repo has no automated test harness. "Verify" steps use `pnpm build` (frontend), `cargo check` (backend), and dev-server visual checks. That is the established verification path for this codebase.

---

## File Structure

**New**
- `frontend/src/lib/i18n/index.js` — init, locale list, getter/setter, persistence.
- `frontend/src/lib/i18n/locales/en.json` — authoritative source catalog.
- `frontend/src/lib/i18n/locales/{es,fr,de,it,pt}.json` — translations.
- `frontend/src/lib/LanguagePicker.svelte` — header dropdown.

**Modified**
- `frontend/package.json` — add `svelte-i18n`.
- `frontend/src/main.js` — import i18n before mount; set `<html lang>`.
- `frontend/src/App.svelte` — replace literals with `$_()`, mount picker, send `locale`.
- `frontend/src/lib/Sidebar.svelte` — replace literals; locale-aware dates.
- `frontend/src/lib/ThemePicker.svelte` — labels via `$_()`.
- `backend/src/rag.rs` — `locale` on `ChatRequest`; `language_context`; suggested-question query param.

---

## Task 1: Install svelte-i18n

**Files:**
- Modify: `frontend/package.json`

- [ ] **Step 1: Install the dependency**

Run (from `frontend/`):
```bash
pnpm add svelte-i18n
```
Expected: `svelte-i18n` appears under `dependencies` in `package.json`; `pnpm-lock.yaml` updates.

- [ ] **Step 2: Verify it resolves**

Run: `pnpm build`
Expected: build succeeds (no usage yet; this just confirms the dep installed cleanly).

- [ ] **Step 3: Commit**

```bash
git add frontend/package.json frontend/pnpm-lock.yaml
git commit -m "build(i18n): add svelte-i18n dependency"
```

---

## Task 2: i18n module + authoritative English catalog

**Files:**
- Create: `frontend/src/lib/i18n/index.js`
- Create: `frontend/src/lib/i18n/locales/en.json`

- [ ] **Step 1: Write the English catalog**

Create `frontend/src/lib/i18n/locales/en.json` with the complete key set (this is the contract every other catalog mirrors):

```json
{
  "header": { "tagline": "Smart Budgeting" },
  "language": { "label": "Language" },
  "theme": { "groupLabel": "Theme", "light": "Light", "system": "System", "dark": "Dark" },
  "common": {
    "cancel": "Cancel",
    "copyCode": "Copy code",
    "copiedToClipboard": "Copied to clipboard!"
  },
  "auth": {
    "heroTitle": "Nels thinks about your money so you don't have to.",
    "heroSubtitle": "Your authenticator-secured AI budgeting fiend.",
    "emailLabel": "Email Address",
    "signIn": "Sign In",
    "registerSubmit": "Register with Authenticator App",
    "toggleToSignIn": "Already have an account? Sign In",
    "toggleToRegister": "New to Nels? Register a new account",
    "dividerOr": "OR",
    "totpInfo": "Nels uses <strong>TOTP authenticator codes</strong>. No password! Register once with an authenticator app (Google Authenticator, 1Password, Authy, etc.) and sign in with the rotating 6-digit code.",
    "regSuccessTitle": "Registration Successful!",
    "regSuccessBody": "Your account is registered. Here is your session token. Copy this if you need to authenticate on other devices or via the API.",
    "continueToDashboard": "Continue to Dashboard",
    "modalTitleRegister": "Set Up Your Authenticator",
    "modalTitleLogin": "Enter Your Code",
    "modalDescRegister": "Scan this QR code with an authenticator app (Google Authenticator, 1Password, Authy, etc.), or add the key manually. Then enter the current 6-digit code to finish.",
    "modalDescLogin": "Open your authenticator app and enter the current 6-digit code for Nels.",
    "qrAlt": "TOTP authenticator QR code",
    "copySecretAria": "Copy secret key to clipboard",
    "codeAria": "6-digit authentication code",
    "verifyCreate": "Verify & Create Account",
    "verifySignIn": "Verify & Sign In"
  },
  "chat": {
    "statusReady": "Nels ready",
    "welcomeNamed": "Hi {name}! I'm Nels, your personal budgeting assistant. I can log transactions, set budget limits, switch between your budgets, and give you tailored advice. What would you like to do?",
    "welcomeAnon": "Hi there! I'm Nels, your personal budgeting assistant. I can log transactions, set budget limits, switch between your budgets, and give you tailored advice. By the way — what should I call you?",
    "analyzing": "Nels is analyzing context...",
    "inputPlaceholder": "Ask Nels to log something or generate insights...",
    "sendAria": "Send"
  },
  "alerts": {
    "enterEmail": "Please enter your email address.",
    "scanQr": "Scan the QR code with your authenticator app.",
    "startRegisterFailed": "Failed to start registration.",
    "startLoginFailed": "Failed to start login.",
    "enterCode": "Enter the 6-digit code from your authenticator app.",
    "regSuccess": "Registration successful!",
    "invalidCode": "Invalid code. Please check your app and try again.",
    "fetchBudgetsFailed": "Failed to fetch budgets list.",
    "chatFailed": "Message failed. Please try again."
  },
  "sidebar": {
    "navAria": "Conversation history",
    "recentAria": "Recent conversations",
    "newConversation": "New conversation",
    "closeAria": "Close sidebar",
    "emptyState": "No conversations yet",
    "groupToday": "Today",
    "groupYesterday": "Yesterday",
    "groupEarlier": "Earlier",
    "conversationActionsAria": "Conversation actions",
    "rename": "Rename",
    "delete": "Delete",
    "accountMenuAria": "Account menu",
    "copyPasscode": "Copy Passcode",
    "copied": "Copied!",
    "signOut": "Sign Out"
  }
}
```

- [ ] **Step 2: Write the i18n module**

Create `frontend/src/lib/i18n/index.js`:

```js
// i18n setup for Nels. Mirrors the theme.js contract: a storage key, a getter
// that resolves the active locale, and a setter that applies + persists it.
import { addMessages, init, locale } from "svelte-i18n";
import { get } from "svelte/store";

import en from "./locales/en.json";
import es from "./locales/es.json";
import fr from "./locales/fr.json";
import de from "./locales/de.json";
import it from "./locales/it.json";
import pt from "./locales/pt.json";

export const STORAGE_KEY = "nels_lang";
export const FALLBACK = "en";

// Display order in the picker; labels are endonyms (the language's own name).
export const LOCALES = [
  { code: "en", label: "English" },
  { code: "es", label: "Español" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "it", label: "Italiano" },
  { code: "pt", label: "Português" },
];

const SUPPORTED = LOCALES.map((l) => l.code);

// Register synchronously so messages exist before first paint (no flash of keys).
addMessages("en", en);
addMessages("es", es);
addMessages("fr", fr);
addMessages("de", de);
addMessages("it", it);
addMessages("pt", pt);

// Resolve initial locale: stored choice -> base of navigator.language -> fallback.
export function getStoredLocale() {
  let stored = null;
  try {
    stored = localStorage.getItem(STORAGE_KEY);
  } catch {
    stored = null;
  }
  if (stored && SUPPORTED.includes(stored)) return stored;
  const nav =
    typeof navigator !== "undefined" && navigator.language
      ? navigator.language.slice(0, 2)
      : null;
  if (nav && SUPPORTED.includes(nav)) return nav;
  return FALLBACK;
}

const initialLocale = getStoredLocale();

init({ fallbackLocale: FALLBACK, initialLocale });

if (typeof document !== "undefined") {
  document.documentElement.setAttribute("lang", initialLocale);
}

// Active base code (region stripped), e.g. "es". Used when calling the backend.
export function currentLocale() {
  const l = get(locale) || FALLBACK;
  return l.slice(0, 2);
}

// Apply + persist a locale choice. Unknown codes fall back silently.
export function setLocale(code) {
  const choice = SUPPORTED.includes(code) ? code : FALLBACK;
  locale.set(choice);
  try {
    localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode).
  }
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("lang", choice);
  }
  return choice;
}
```

- [ ] **Step 3: Commit** (build is verified in Task 4 after wiring into main.js; the JSON imports for es/fr/de/it/pt are created in Task 3)

```bash
git add frontend/src/lib/i18n/index.js frontend/src/lib/i18n/locales/en.json
git commit -m "feat(i18n): add i18n module and English catalog"
```

> Note: `index.js` imports `es/fr/de/it/pt` JSON, created in Task 3. Do not run `pnpm build` until Task 3 is done, or the missing imports will fail the build. Implement Task 3 immediately next.

---

## Task 3: Translation catalogs (es, fr, de, it, pt)

**Files:**
- Create: `frontend/src/lib/i18n/locales/es.json`
- Create: `frontend/src/lib/i18n/locales/fr.json`
- Create: `frontend/src/lib/i18n/locales/de.json`
- Create: `frontend/src/lib/i18n/locales/it.json`
- Create: `frontend/src/lib/i18n/locales/pt.json`

Each file has the **exact same key structure** as `en.json`. Translation rules:
- Translate every string **value**; never change a key.
- Preserve ICU placeholders verbatim: `{name}` stays `{name}`.
- Preserve inline HTML in `auth.totpInfo`: keep `<strong>…</strong>` tags, translate only the text inside/around them.
- Do **not** translate brand/product names: `Nels`, `TOTP`, `Google Authenticator`, `1Password`, `Authy`, `OR` may localize (`auth.dividerOr`).
- Translations are machine-generated; flag the PR for native-speaker review.

- [ ] **Step 1: Write `es.json`** (complete worked example — the quality bar for the other four)

```json
{
  "header": { "tagline": "Presupuestos inteligentes" },
  "language": { "label": "Idioma" },
  "theme": { "groupLabel": "Tema", "light": "Claro", "system": "Sistema", "dark": "Oscuro" },
  "common": {
    "cancel": "Cancelar",
    "copyCode": "Copiar código",
    "copiedToClipboard": "¡Copiado al portapapeles!"
  },
  "auth": {
    "heroTitle": "Nels piensa en tu dinero para que tú no tengas que hacerlo.",
    "heroSubtitle": "Tu fiera de los presupuestos con IA, protegida por autenticador.",
    "emailLabel": "Correo electrónico",
    "signIn": "Iniciar sesión",
    "registerSubmit": "Registrarse con app de autenticación",
    "toggleToSignIn": "¿Ya tienes una cuenta? Inicia sesión",
    "toggleToRegister": "¿Nuevo en Nels? Crea una cuenta",
    "dividerOr": "O",
    "totpInfo": "Nels usa <strong>códigos de autenticación TOTP</strong>. ¡Sin contraseña! Regístrate una vez con una app de autenticación (Google Authenticator, 1Password, Authy, etc.) e inicia sesión con el código rotativo de 6 dígitos.",
    "regSuccessTitle": "¡Registro completado!",
    "regSuccessBody": "Tu cuenta está registrada. Aquí tienes tu token de sesión. Cópialo si necesitas autenticarte en otros dispositivos o mediante la API.",
    "continueToDashboard": "Continuar al panel",
    "modalTitleRegister": "Configura tu autenticador",
    "modalTitleLogin": "Introduce tu código",
    "modalDescRegister": "Escanea este código QR con una app de autenticación (Google Authenticator, 1Password, Authy, etc.), o añade la clave manualmente. Luego introduce el código actual de 6 dígitos para terminar.",
    "modalDescLogin": "Abre tu app de autenticación e introduce el código actual de 6 dígitos para Nels.",
    "qrAlt": "Código QR del autenticador TOTP",
    "copySecretAria": "Copiar clave secreta al portapapeles",
    "codeAria": "Código de autenticación de 6 dígitos",
    "verifyCreate": "Verificar y crear cuenta",
    "verifySignIn": "Verificar e iniciar sesión"
  },
  "chat": {
    "statusReady": "Nels listo",
    "welcomeNamed": "¡Hola {name}! Soy Nels, tu asistente personal de presupuestos. Puedo registrar transacciones, fijar límites de presupuesto, cambiar entre tus presupuestos y darte consejos personalizados. ¿Qué te gustaría hacer?",
    "welcomeAnon": "¡Hola! Soy Nels, tu asistente personal de presupuestos. Puedo registrar transacciones, fijar límites de presupuesto, cambiar entre tus presupuestos y darte consejos personalizados. Por cierto, ¿cómo te llamas?",
    "analyzing": "Nels está analizando el contexto...",
    "inputPlaceholder": "Pide a Nels que registre algo o genere análisis...",
    "sendAria": "Enviar"
  },
  "alerts": {
    "enterEmail": "Introduce tu correo electrónico.",
    "scanQr": "Escanea el código QR con tu app de autenticación.",
    "startRegisterFailed": "No se pudo iniciar el registro.",
    "startLoginFailed": "No se pudo iniciar la sesión.",
    "enterCode": "Introduce el código de 6 dígitos de tu app de autenticación.",
    "regSuccess": "¡Registro completado!",
    "invalidCode": "Código no válido. Revisa tu app e inténtalo de nuevo.",
    "fetchBudgetsFailed": "No se pudo obtener la lista de presupuestos.",
    "chatFailed": "No se pudo enviar el mensaje. Inténtalo de nuevo."
  },
  "sidebar": {
    "navAria": "Historial de conversaciones",
    "recentAria": "Conversaciones recientes",
    "newConversation": "Nueva conversación",
    "closeAria": "Cerrar barra lateral",
    "emptyState": "Aún no hay conversaciones",
    "groupToday": "Hoy",
    "groupYesterday": "Ayer",
    "groupEarlier": "Anteriores",
    "conversationActionsAria": "Acciones de conversación",
    "rename": "Renombrar",
    "delete": "Eliminar",
    "accountMenuAria": "Menú de cuenta",
    "copyPasscode": "Copiar código de acceso",
    "copied": "¡Copiado!",
    "signOut": "Cerrar sesión"
  }
}
```

- [ ] **Step 2: Write `fr.json`, `de.json`, `it.json`, `pt.json`**

Produce each as a complete catalog with the identical key structure shown for `en.json`/`es.json`, translating every value into French, German, Italian, and Portuguese respectively, following the translation rules above (preserve keys, `{name}`, `<strong>` tags, and brand names). Anchor terms for consistency:
- "budgeting assistant" → fr "assistant de budget", de "Budget-Assistent", it "assistente di budget", pt "assistente de orçamento".
- "Sign In" → fr "Se connecter", de "Anmelden", it "Accedi", pt "Entrar".
- "New conversation" → fr "Nouvelle conversation", de "Neue Unterhaltung", it "Nuova conversazione", pt "Nova conversa".
- Header tagline "Smart Budgeting" → fr "Budget intelligent", de "Cleveres Budget", it "Budget intelligente", pt "Orçamento inteligente".

- [ ] **Step 3: Verify all catalogs parse and have matching keys**

Run (from `frontend/`):
```bash
node -e "const en=require('./src/lib/i18n/locales/en.json');const keys=o=>Object.entries(o).flatMap(([k,v])=>typeof v==='object'?keys(v).map(s=>k+'.'+s):[k]);const ek=keys(en).sort();for(const l of ['es','fr','de','it','pt']){const c=require('./src/lib/i18n/locales/'+l+'.json');const ck=keys(c).sort();const miss=ek.filter(k=>!ck.includes(k));const extra=ck.filter(k=>!ek.includes(k));console.log(l, miss.length?('MISSING '+miss):'', extra.length?('EXTRA '+extra):'', (!miss.length&&!extra.length)?'OK':'')}"
```
Expected: each language prints `OK`. Fix any `MISSING`/`EXTRA` before continuing.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/lib/i18n/locales/es.json frontend/src/lib/i18n/locales/fr.json frontend/src/lib/i18n/locales/de.json frontend/src/lib/i18n/locales/it.json frontend/src/lib/i18n/locales/pt.json
git commit -m "feat(i18n): add es/fr/de/it/pt translation catalogs"
```

---

## Task 4: Bootstrap i18n at app startup

**Files:**
- Modify: `frontend/src/main.js`

- [ ] **Step 1: Import the i18n module before mounting**

Open `frontend/src/main.js`. Add the i18n import as the **first** import (its side effect calls `init()` and registers messages before any component renders):

```js
import "./lib/i18n/index.js";
```
Place it above the existing `import './app.css'` / app-mount lines. Do not change the rest of the file.

- [ ] **Step 2: Verify the build now succeeds end-to-end**

Run (from `frontend/`): `pnpm build`
Expected: build succeeds. The i18n store is initialized; all six catalogs resolve.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/main.js
git commit -m "feat(i18n): initialize i18n before app mount"
```

---

## Task 5: Language picker component

**Files:**
- Create: `frontend/src/lib/LanguagePicker.svelte`

- [ ] **Step 1: Write the component**

Create `frontend/src/lib/LanguagePicker.svelte`:

```svelte
<script>
  import { Globe, Check } from "lucide-svelte";
  import { locale, _ } from "svelte-i18n";
  import { LOCALES, setLocale } from "./i18n/index.js";

  // Active base code (region stripped) for highlighting the current choice.
  let active = $derived(($locale || "en").slice(0, 2));
  let activeLabel = $derived(
    LOCALES.find((l) => l.code === active)?.label ?? "English",
  );

  function choose(code) {
    setLocale(code);
    // Close the daisyUI dropdown by blurring the focused menu item.
    if (typeof document !== "undefined") document.activeElement?.blur();
  }
</script>

<div class="dropdown dropdown-end">
  <button
    tabindex="0"
    type="button"
    class="btn btn-ghost btn-xs h-6 min-h-0 px-1.5 gap-1 text-base-content/70"
    aria-label={$_("language.label")}
    title={$_("language.label")}
  >
    <Globe class="w-3.5 h-3.5" />
    <span class="text-[11px] font-semibold uppercase">{active}</span>
  </button>
  <ul
    class="dropdown-content menu bg-base-200 border border-base-300 rounded-xl shadow-2xl w-44 mt-2 p-1 z-50"
  >
    {#each LOCALES as opt}
      <li>
        <button
          type="button"
          class="flex items-center justify-between gap-2 text-sm {active ===
          opt.code
            ? 'text-primary font-semibold'
            : 'text-base-content/80'}"
          aria-current={active === opt.code}
          onclick={() => choose(opt.code)}
        >
          {opt.label}
          {#if active === opt.code}
            <Check class="w-4 h-4 shrink-0" />
          {/if}
        </button>
      </li>
    {/each}
  </ul>
</div>
```

- [ ] **Step 2: Verify it compiles**

Run (from `frontend/`): `pnpm build`
Expected: build succeeds (component compiles even though not yet mounted).

- [ ] **Step 3: Commit**

```bash
git add frontend/src/lib/LanguagePicker.svelte
git commit -m "feat(i18n): add header language picker component"
```

---

## Task 6: Localize App.svelte and wire locale to the backend

**Files:**
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Add imports**

In the `<script>` block of `frontend/src/App.svelte`, after the existing component imports (near `import ThemePicker from "./lib/ThemePicker.svelte";`), add:

```js
import LanguagePicker from "./lib/LanguagePicker.svelte";
import { _ } from "svelte-i18n";
import { currentLocale } from "./lib/i18n/index.js";
```

- [ ] **Step 2: Mount the picker in the header**

Replace the RIGHT-cluster block (around `App.svelte:531-533`):

```svelte
    <!-- RIGHT: theme picker (always visible — auth and chat screens).
         Account actions live in the sidebar footer (see Sidebar.svelte). -->
    <ThemePicker />
```
with:
```svelte
    <!-- RIGHT: language + theme pickers (always visible — auth and chat screens).
         Account actions live in the sidebar footer (see Sidebar.svelte). -->
    <div class="flex items-center gap-1.5">
      <LanguagePicker />
      <ThemePicker />
    </div>
```

- [ ] **Step 3: Replace the desktop wordmark tagline**

Replace `Smart Budgeting` (the `<span>` around `App.svelte:509-511`) text content with `{$_("header.tagline")}`. Keep all classes intact, e.g.:
```svelte
        >{$_("header.tagline")}</span
```

- [ ] **Step 4: Replace auth-screen literals**

In the auth card and reg-success card, swap each hardcoded string for its key. Exact mapping (text → key):
- `Nels thinks about your money so you don't have to.` → `{$_("auth.heroTitle")}`
- `Your authenticator-secured AI budgeting fiend.` → `{$_("auth.heroSubtitle")}`
- `Email Address` → `{$_("auth.emailLabel")}`
- The submit button line `{isRegFlow ? "Register with Authenticator App" : "Sign In"}` → `{isRegFlow ? $_("auth.registerSubmit") : $_("auth.signIn")}`
- The divider `OR` → `{$_("auth.dividerOr")}`
- The toggle `{isRegFlow ? "Already have an account? Sign In" : "New to Nels? Register a new account"}` → `{isRegFlow ? $_("auth.toggleToSignIn") : $_("auth.toggleToRegister")}`
- The TOTP info `<p>` (contains `<strong>`): replace its inner markup with `{@html $_("auth.totpInfo")}`. (The catalog value carries the `<strong>` tags; content is static and trusted.)
- `Registration Successful!` → `{$_("auth.regSuccessTitle")}`
- The reg-success body paragraph → `{$_("auth.regSuccessBody")}`
- `Continue to Dashboard` → `{$_("auth.continueToDashboard")}`
- `aria-label="Copy code"` → `aria-label={$_("common.copyCode")}`
- `Copied to clipboard!` (both occurrences) → `{$_("common.copiedToClipboard")}`

- [ ] **Step 5: Replace TOTP modal literals**

- `{isRegFlow ? "Set Up Your Authenticator" : "Enter Your Code"}` → `{isRegFlow ? $_("auth.modalTitleRegister") : $_("auth.modalTitleLogin")}`
- Register description block → `{$_("auth.modalDescRegister")}`
- Login description block → `{$_("auth.modalDescLogin")}`
- `alt="TOTP authenticator QR code"` → `alt={$_("auth.qrAlt")}`
- `aria-label="Copy secret key to clipboard"` → `aria-label={$_("auth.copySecretAria")}`
- `aria-label="6-digit authentication code"` → `aria-label={$_("auth.codeAria")}`
- `{isRegFlow ? "Verify & Create Account" : "Verify & Sign In"}` → `{isRegFlow ? $_("auth.verifyCreate") : $_("auth.verifySignIn")}`
- `Cancel` → `{$_("common.cancel")}`

- [ ] **Step 6: Replace chat-screen literals**

- `Nels ready` → `{$_("chat.statusReady")}`
- Welcome named branch → `{$_("chat.welcomeNamed", { values: { name: user.name } })}`
- Welcome anon branch → `{$_("chat.welcomeAnon")}`
- `Nels is analyzing context...` → `{$_("chat.analyzing")}`
- `placeholder="Ask Nels to log something or generate insights..."` → `placeholder={$_("chat.inputPlaceholder")}`
- On the send `<button>` (wrapping `<Send …/>`, `App.svelte:953-959`) add `aria-label={$_("chat.sendAria")}`.

- [ ] **Step 7: Replace alert strings**

In the `<script>` handlers, replace each `triggerError(...)`/`triggerSuccess(...)` literal:
- `"Please enter your email address."` → `$_("alerts.enterEmail")`
- `"Scan the QR code with your authenticator app."` → `$_("alerts.scanQr")`
- `"Failed to start registration."` → `$_("alerts.startRegisterFailed")`
- `"Failed to start login."` → `$_("alerts.startLoginFailed")`
- `"Enter the 6-digit code from your authenticator app."` → `$_("alerts.enterCode")`
- `"Registration successful!"` → `$_("alerts.regSuccess")`
- `"Invalid code. Please check your app and try again."` → `$_("alerts.invalidCode")`
- `"Failed to fetch budgets list."` → `$_("alerts.fetchBudgetsFailed")`

To call `$_` from `<script>` (not just markup), import the raw getter at the top of `<script>`:
```js
import { get } from "svelte/store";
import { _ as i18n } from "svelte-i18n";
const t = (key, opts) => get(i18n)(key, opts);
```
Then use `t("alerts.enterEmail")` etc. in handlers. (Markup keeps using `$_`.)

- [ ] **Step 8: Send the locale to the backend on /chat**

In the `/chat` POST body (`App.svelte:303-307`), add `locale`:
```js
        body: JSON.stringify({
          message: userMessage,
          budget_id: activeBudget ? activeBudget.id : null,
          conversation_id: activeConversationId,
          locale: currentLocale(),
        }),
```
If the chat catch-block currently has no user-facing error, set one: `triggerError(t("alerts.chatFailed"));`

- [ ] **Step 9: Send the locale to /suggested-question**

In `fetchSuggestedQuestion` (the `fetchApi("/suggested-question")` call, ~`App.svelte:272`), append the query param:
```js
      const res = await fetchApi(`/suggested-question?locale=${currentLocale()}`);
```

- [ ] **Step 10: Verify build + visual**

Run (from `frontend/`): `pnpm build`
Expected: build succeeds with no unused-import or syntax errors.

Then `pnpm dev`, open the app, and confirm: the header shows the language picker; switching to Español/Deutsch changes auth + chat chrome; no raw keys (e.g. `auth.signIn`) are visible.

- [ ] **Step 11: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "feat(i18n): localize App.svelte and send locale to backend"
```

---

## Task 7: Localize Sidebar.svelte

**Files:**
- Modify: `frontend/src/lib/Sidebar.svelte`

- [ ] **Step 1: Add the i18n import**

In the Sidebar `<script>`, add:
```js
import { _, locale } from "svelte-i18n";
```

- [ ] **Step 2: Localize the day-group labels**

The `groups` derivation uses hardcoded bucket labels (`"Today"`, `"Yesterday"`, `"Earlier"`) both as object keys and display text. Keep the internal bucket keys as-is (they are code identifiers), but render translated labels. Change the markup that prints `{group.label}` to map the internal label to a key:
```svelte
          {group.label === "Today"
            ? $_("sidebar.groupToday")
            : group.label === "Yesterday"
              ? $_("sidebar.groupYesterday")
              : $_("sidebar.groupEarlier")}
```

- [ ] **Step 3: Localize the remaining literals**

- `aria-label="Conversation history"` → `aria-label={$_("sidebar.navAria")}`
- `aria-label="Recent conversations"` → `aria-label={$_("sidebar.recentAria")}`
- Both `title="New conversation"` and `aria-label="New conversation"`, plus the `<span>New conversation</span>` → `{$_("sidebar.newConversation")}` (and the attrs use `={$_("sidebar.newConversation")}`)
- `aria-label="Close sidebar"` → `aria-label={$_("sidebar.closeAria")}`
- `No conversations yet` → `{$_("sidebar.emptyState")}`
- `aria-label="Conversation actions"` → `aria-label={$_("sidebar.conversationActionsAria")}`
- `Rename` → `{$_("sidebar.rename")}`
- `Delete` → `{$_("sidebar.delete")}`
- `aria-label="Account menu"` → `aria-label={$_("sidebar.accountMenuAria")}`
- `Copy Passcode` → `{$_("sidebar.copyPasscode")}`
- `Copied!` → `{$_("sidebar.copied")}`
- `Sign Out` → `{$_("sidebar.signOut")}`

In `titleFor`, the fallback `"New conversation"` is used as a display title for untitled threads. Make it locale-aware:
```js
  function titleFor(c) {
    return c.title && c.title.trim() ? c.title : get(i18n)("sidebar.newConversation");
  }
```
Add at the top of `<script>`: `import { get } from "svelte/store";` and `import { _ as i18n } from "svelte-i18n";` (reuse the existing `_` import — you can `import { _, locale } from "svelte-i18n";` and alias via `const i18n = _;`). Keep it simple: `const tt = (k) => get(_)(k);` then `return ... : tt("sidebar.newConversation");`.

- [ ] **Step 4: Make relative-time dates locale-aware**

In `relativeTime`, the long-form fallback uses `toLocaleDateString(undefined, …)`. Replace `undefined` with the active locale so month names localize:
```js
    return new Date(iso).toLocaleDateString(get(locale) || "en", {
      month: "short",
      day: "numeric",
    });
```
(`get` and `locale` are imported in Steps 1/3.)

- [ ] **Step 5: Verify build + visual**

Run: `pnpm build` → succeeds. In `pnpm dev`, open the chat screen with a non-English locale and confirm sidebar labels (New conversation, Today/Yesterday/Earlier, Rename/Delete, Sign Out) are translated.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/Sidebar.svelte
git commit -m "feat(i18n): localize Sidebar.svelte with locale-aware dates"
```

---

## Task 8: Localize ThemePicker.svelte

**Files:**
- Modify: `frontend/src/lib/ThemePicker.svelte`

- [ ] **Step 1: Localize option labels and the group aria-label**

Add `import { _ } from "svelte-i18n";` to the `<script>`. Change the `OPTIONS` array so labels are resolved at render time instead of hardcoded — replace the static `label` strings with i18n keys and resolve in markup:

```js
  const OPTIONS = [
    { value: "light", key: "theme.light", Icon: Sun },
    { value: "system", key: "theme.system", Icon: Monitor },
    { value: "dark", key: "theme.dark", Icon: Moon },
  ];
```
In the markup, replace `aria-label={opt.label}` and `title={opt.label}` with `aria-label={$_(opt.key)}` and `title={$_(opt.key)}`. Replace the group `aria-label="Theme"` with `aria-label={$_("theme.groupLabel")}`.

- [ ] **Step 2: Verify build**

Run: `pnpm build` → succeeds. Hovering the theme buttons in a non-English locale shows translated tooltips.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/lib/ThemePicker.svelte
git commit -m "feat(i18n): localize ThemePicker labels"
```

---

## Task 9: Backend — locale-aware chat replies

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Add `locale` to `ChatRequest`**

In `backend/src/rag.rs`, extend the struct (`rag.rs:15-20`):
```rust
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub budget_id: Option<Uuid>,
    pub conversation_id: Option<Uuid>,
    pub locale: Option<String>,
}
```

- [ ] **Step 2: Add a locale→language helper**

Add near the top of `rag.rs` (after the structs, before `chat_endpoint`):
```rust
// Map a base locale code to an English language name for prompt instructions.
// en / None / unknown -> None (assistant replies in English, the default).
fn language_name(locale: Option<&str>) -> Option<&'static str> {
    match locale.map(|s| &s[..s.len().min(2)]) {
        Some("es") => Some("Spanish"),
        Some("fr") => Some("French"),
        Some("de") => Some("German"),
        Some("it") => Some("Italian"),
        Some("pt") => Some("Portuguese"),
        _ => None,
    }
}
```

- [ ] **Step 3: Build a `language_context` and inject it into the system prompt**

In `chat_endpoint`, near where `name_context` is built (`rag.rs:297-303`), add:
```rust
    // Language instruction: localize the user-facing reply text only; the JSON
    // action keys/params are machine identifiers and MUST stay unchanged.
    let language_context = match language_name(payload.locale.as_deref()) {
        Some(lang) => format!(
            "IMPORTANT: Write the user-facing 'response' text in {}. \
             Do NOT translate JSON field names, the 'action' value, or any action_params keys — \
             those stay exactly as specified in English.",
            lang
        ),
        None => String::new(),
    };
```
Then thread `language_context` into the `system_instructions` `format!` (`rag.rs:532`): add one more `{}\n\n\` line in the template and pass `language_context` as the corresponding argument (place it after the `name_context` argument). Verify the number of `{}` placeholders matches the number of arguments.

- [ ] **Step 4: Verify the backend compiles**

Run (from `backend/`): `cargo check`
Expected: compiles with no errors. (A warning-free build is ideal; an unused-variable warning means an argument wasn't wired in — fix it.)

- [ ] **Step 5: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(i18n): localize AI chat replies via locale in system prompt"
```

---

## Task 10: Backend — locale-aware suggested questions

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Import the `Query` extractor**

In `rag.rs:1-2`, extend the axum import to include `Query`:
```rust
use axum::{
    extract::{State, Path, Extension, Query},
    ...
};
```
(Keep the rest of the existing `use axum::{…}` block unchanged.)

- [ ] **Step 2: Add a query struct and read it in the handler**

Above `suggested_question` (`rag.rs:1473`), add:
```rust
#[derive(Deserialize, Default)]
pub struct SuggestedQuestionQuery {
    pub locale: Option<String>,
}
```
Change the handler signature to accept the query and pass the language through:
```rust
pub async fn suggested_question(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Query(params): Query<SuggestedQuestionQuery>,
) -> Result<Json<SuggestedQuestionResponse>, (StatusCode, String)> {
```
At the call site (`rag.rs:1502`), pass the language name:
```rust
    let question = generate_suggested_question(&context, &api_key, language_name(params.locale.as_deref()))
        .await
        .unwrap_or_else(|| DEFAULT_SUGGESTED_QUESTION.to_string());
```

- [ ] **Step 3: Localize the suggestion prompt**

Change `generate_suggested_question` (`rag.rs:1511`) to accept the language and append an instruction:
```rust
async fn generate_suggested_question(context: &str, api_key: &str, lang: Option<&'static str>) -> Option<String> {
```
After building `prompt` (`rag.rs:1518-1525`), add:
```rust
    let prompt = match lang {
        Some(l) => format!("{}\n\nWrite the question in {}.", prompt, l),
        None => prompt,
    };
```
(If `prompt` is currently `let prompt = format!(...)` without `mut`, this shadow-rebind is fine.)

- [ ] **Step 4: Verify the backend compiles**

Run (from `backend/`): `cargo check`
Expected: compiles cleanly.

- [ ] **Step 5: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(i18n): localize suggested questions via locale query param"
```

---

## Task 11: End-to-end verification

**Files:** none (verification only)

- [ ] **Step 1: Full frontend build**

Run (from `frontend/`): `pnpm build`
Expected: succeeds.

- [ ] **Step 2: Full backend build**

Run (from `backend/`): `cargo build`
Expected: succeeds.

- [ ] **Step 3: Manual smoke test**

Start backend and `pnpm dev`. Then:
- Switch the header language picker to **Español**, **Français**, **Deutsch** — confirm auth screen, header, chat welcome, sidebar all translate with no raw keys, and the picker shows the active code.
- Reload the page — confirm the chosen language persists (localStorage `nels_lang`) and `<html lang>` matches.
- Sign in and send a chat message with a non-English locale selected — confirm the assistant's reply text returns in that language while any budget action still executes.
- Confirm the suggested-question chip (when present) is in the selected language.

- [ ] **Step 4: Final review note**

The five non-English catalogs are machine-generated. Add a line to the PR description requesting native-speaker review of `frontend/src/lib/i18n/locales/{es,fr,de,it,pt}.json` before release.

---

## Spec Coverage Check

- 6 locales + fallback → Task 2 (`LOCALES`, `en.json`), Task 3 (translations).
- Header picker beside ThemePicker → Task 5, Task 6 Step 2.
- Persistence + `<html lang>` + browser default → Task 2 (`getStoredLocale`/`setLocale`), Task 4.
- Synchronous catalog registration (no flash) → Task 2 (`addMessages`).
- All UI strings extracted (App, Sidebar, ThemePicker, incl. aria/placeholder/alt/title) → Tasks 6, 7, 8.
- Locale-aware dates → Task 7 Step 4.
- AI replies localized; action keys English → Task 9.
- Suggested questions localized → Task 10.
- Frontend→backend wiring (`/chat` body, `/suggested-question` query) → Task 6 Steps 8-9.
- Compatibility: absent/unknown locale = English → Task 9 Step 2, Task 10 (`language_name` → None).
- Verification (build + manual) → Task 11.
