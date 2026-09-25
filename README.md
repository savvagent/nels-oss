# Nels: Multitenant Budget RAG Co-Pilot

Nels is a personal budgeting co-pilot built with a modern, high-performance stack: a Rust backend leveraging Axum and SQLx with pgvector, and a fast Svelte 5 frontend styled with Tailwind CSS v4 and daisyUI v5. 

It enables users to manage, share, analyze, and forecast their budgets through conversational AI prompts.

---

## Technical Stack

- **Backend**: Rust (Axum, SQLx with PostgreSQL + `pgvector`, and `totp-rs` for TOTP authenticator-app authentication).
- **Frontend**: Svelte 5 (Vite, `@tailwindcss/vite` v4, `daisyui` v5, lucide-svelte icons). The authenticated product app.
- **Marketing site**: SvelteKit 2 (Svelte 5, Tailwind v4, daisyUI v5), fully prerendered to static HTML, deployed to Cloudflare Pages at `nels.money`.
- **Admin console**: Svelte 5 (Vite PWA, Tailwind v4, daisyUI v5). A standalone, installable admin app whose TOTP login is gated on `users.is_admin`; deployed to Cloudflare Pages (project `nels-admin`).
- **Database**: PostgreSQL with `pgvector` extension for semantic search / RAG embeddings.

---

## Core Scenarios & Features

1. **AI-Driven Budget Setup**: Set up budgets, specify limits, time frames (monthly, quarterly, yearly), classify income, savings, or expenses, and define category spending limits.
2. **Budget Tracking & Notifications**: Log expenses with automated AI parsers. Query current spending against limits.
3. **Budget Analysis & Insights**: Ask Nels for spending patterns, breakdown reports, and recommendations (e.g., "What percentage of my budget is spent on food?").
4. **Sharing & Collaboration**: Share budgets via email with distinct permission levels (view or edit), and audit collaboration changes via the budget activity history.
5. **Forecasting & Planning**: Request automated spending forecasts and personalized saving recommendations tailored to future financial goals.
6. **Per-Budget Rollover**: Toggle rollover on any budget so unused funds carry forward. When enabled, the previous period's unused remainder (base amount − spent) is added to the current period; an overspend does **not** carry — the carried amount clamps at 0. When disabled (the default), each period simply uses the base amount. Nels surfaces the base, carried-over, and effective amounts in chat so the distinction is always clear.
7. **Project-Based Budgets**: Create a budget scoped to a project rather than a recurring calendar period (e.g. "create a project budget for the kitchen remodel"). Project budgets have no recurring period — they are excluded from monthly rollover/reset and track spend across the full project span, from creation until you close them. When the project ends, close it ("close the kitchen project") to make it read-only; existing time-based budgets are unaffected.
8. **Archiving Budgets**: Archive any budget you no longer want cluttering your main list ("archive the 2024 vacation budget"). Archived budgets are hidden from the main list, but their data and historical reports are fully preserved — view them anytime and unarchive to bring one back. Archiving is distinct from closing a project: it just hides the budget, it does not make it read-only, and it is driven through chat per the chat-first precedent.
9. **Command Palette / Slash-Commands**: Type `/` in the chat input to surface a small autocomplete palette of quick actions. `/clear` resets the current chat view and starts a fresh conversation (non-destructive — earlier threads remain in the sidebar). `/issues-list` lists the project's open GitHub issues and `/issues-create Title | optional body` files a new one. The GitHub-backed commands call an authenticated **server-side** endpoint (the token is never exposed to the browser) and redact obvious secrets/tokens/financial identifiers before filing. See the GitHub-integration env vars below; when unset, those two commands degrade gracefully with a clear "not configured" message.

---

## Getting Started

### Prerequisites

- [Podman & podman-compose](https://podman.io/) (for pgvector database container)
- [Rust & Cargo](https://rustup.rs/) (edition 2021)
- [Node.js & pnpm](https://pnpm.io/)

---

### Step 1: Start the Database

Launch the pre-configured PostgreSQL pgvector container:

```bash
podman-compose up -d
```

### Step 2: Configure & Start the Backend

1. Navigate to the `backend` folder:
   ```bash
   cd backend
   ```
2. (Optional) Create a `.env` file or export environment variables:
   ```bash
   # Defaults:
   DATABASE_URL=postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag
   GEMINI_API_KEY=your_gemini_api_key
   # Base64-encoded 32-byte key used to encrypt bank-provider access tokens at
   # rest (AES-256-GCM) — see backend/src/crypto.rs. No longer used for login;
   # login uses WebAuthn passkeys (nels#551). REQUIRED — the backend refuses to
   # start without it. Generate with: openssl rand -base64 32
   TOTP_ENC_KEY=
   # WebAuthn relying-party config for passkey login (nels#551, nels#560). Two
   # RPs are configured: the app and the admin console are different effective
   # domains. Without FLY_APP_NAME (local dev) both default to rp_id localhost
   # with the Vite dev origins (5173 app / 5174 admin); on Fly they default to
   # production, shown here. *_ORIGIN accepts a comma-separated list.
   # WEBAUTHN_APP_RP_ID=nels.money
   # WEBAUTHN_APP_ORIGIN=https://app.nels.money
   # WEBAUTHN_ADMIN_RP_ID=nels-admin.pages.dev
   # WEBAUTHN_ADMIN_ORIGIN=https://nels-admin.pages.dev
   # Optional. Number of one-time recovery codes issued at registration (and by
   # an admin-assisted credential reset). Defaults to 10.
   RECOVERY_CODE_COUNT=10
   # Optional. Drop pgvector embeddings on chat messages older than this many
   # days (readable message text is preserved). Defaults to 90.
   EMBEDDING_RETENTION_DAYS=90
   # Optional. Always keep embeddings for the most recent N messages of each
   # conversation, regardless of age. Defaults to 50.
   EMBEDDING_RETENTION_KEEP_RECENT=50
   # Optional. Retention for the audit_logs table. Rows older than this many
   # days are purged hourly. Defaults to 365.
   AUDIT_LOG_RETENTION_DAYS=365
   # Optional. Retention for the notifications table. READ notifications older
   # than NOTIFICATION_READ_RETENTION_DAYS and UNREAD notifications older than
   # NOTIFICATION_UNREAD_RETENTION_DAYS are purged hourly. Unread are kept
   # longer so unseen alerts are not lost. Default 30 (read) / 90 (unread).
   NOTIFICATION_READ_RETENTION_DAYS=30
   NOTIFICATION_UNREAD_RETENTION_DAYS=90
   # Optional. GitHub integration for the chat command palette (/issues-list,
   # /issues-create). If GITHUB_TOKEN is unset, those commands degrade
   # gracefully. Use a fine-grained PAT scoped to Issues: read & write on the
   # target repo only (least privilege). The token is used server-side only and
   # is NEVER sent to the client.
   GITHUB_TOKEN=
   # Optional. owner/repo the palette reads/creates issues in. Default savvagent/nels.
   GITHUB_REPO=savvagent/nels
   ```
   *Note: If `GEMINI_API_KEY` is not provided, the server automatically starts in a fully functional offline pattern-matching router for easier local testing.*
   `TOTP_ENC_KEY` is **required** — generate one with `openssl rand -base64 32`. In production it is set via `fly secrets set TOTP_ENC_KEY=... -a nels-api`.
   The `WEBAUTHN_*` vars are **optional** in every environment (sane defaults for both prod and local dev) — only set them if the app/admin console are ever deployed to different domains than the ones above. The origins listed there are also the only ones the auth endpoints accept (anything else is `400 Unrecognized origin`), so a new app domain needs adding here as well as to `CORS_ALLOWED_ORIGINS`.
   `GITHUB_TOKEN` is **optional** — the `/issues-list` and `/issues-create` palette commands need it to reach GitHub; without it they return a clear "not configured" message and the rest of the app is unaffected. In production set it via `fly secrets set GITHUB_TOKEN=... -a nels-api`.
3. Run migrations and start the server:
   ```bash
   cargo run
   ```
   Or, for development, use [`bacon`](https://dystroy.org/bacon/) to rebuild and
   restart the server automatically whenever a source file changes (so the
   running binary never goes stale):
   ```bash
   cargo install bacon   # one-time
   bacon                 # auto-restarting dev server
   ```

The backend server runs at `http://localhost:3000`. Database migrations are executed automatically on start.

---

### Step 3: Start the Frontend

1. Navigate to the `frontend` folder:
   ```bash
   cd frontend
   ```
2. Install dependencies:
   ```bash
   pnpm install
   ```
3. Run the Vite development server:
   ```bash
   pnpm run dev
   ```

The Svelte 5 application will be accessible at `http://localhost:5173`.

---

### Step 4: (Optional) Run the Marketing Site

The public marketing website (`nels.money`) is a separate SvelteKit app:

```bash
cd marketing
pnpm install
pnpm run dev   # http://localhost:5173 — use a different port if the frontend is already running
```

It is fully prerendered to static HTML (`pnpm run build` → `.svelte-kit/cloudflare`; `pnpm test` and `pnpm check` for unit tests and type-checking). Site copy and SEO strategy are documented in `docs/marketing/nels-seo-strategy.md`.

---

## Deployment

Each component auto-deploys on push to `main` via path-filtered GitHub Actions (`.github/workflows/`):

- **Frontend app** (`/frontend`) → Cloudflare Pages (project `nels`).
- **Marketing site** (`/marketing`) → Cloudflare Pages (project `nels-money`, custom domain `nels.money`).
- **Admin console** (`/admin`) → Cloudflare Pages (project `nels-admin`), release-gated like the frontend app.
- **Backend API** (`/backend`) → Fly.io app `nels-api` (region `iad`), live at `nels-api.fly.dev`.
- **Database** → Fly Managed Postgres cluster `savvagent-pg` (`pgvector` enabled).

The deployment design is documented under `docs/superpowers/specs/`.

---

## Developer Guide & Gotchas

- **Passkey Authentication** (nels#551): There are no passwords and no codes. Registration and login are WebAuthn/FIDO2 ceremonies — the browser's `navigator.credentials.create()`/`.get()` — backed by the device's platform authenticator (fingerprint, face, PIN) or a security key. The app (`app.nels.money`, RP ID `nels.money`; the `nels.pages.dev` alias redirects there) and admin console (`nels-admin.pages.dev`) are separate WebAuthn relying parties (different effective domains), so a passkey enrolled on one does not work on the other — see `backend/src/passkeys.rs`. Registration also issues a batch of one-time recovery codes (shown once); a lost device is recovered via `/auth/recovery/start`+`/finish` with one of those codes, or — as a last resort — an admin can clear a user's credentials via `POST /api/admin/users/:id/reset-credentials` and reissue fresh codes.
- **RAG & Vector Embeddings**: The semantic search for the budgeting agent uses Google Gemini's `text-embedding-004` which outputs exactly **768-dimension** vectors. This matches the `embedding vector(768)` field constraint in the database schema.

---

## License

Nels is free software, licensed under the [GNU Affero General Public License v3.0 only](LICENSE) (`AGPL-3.0-only`). You can run, study, change and share it. If you run a modified version as a network service, you must offer its users the source code of that version.

The license covers the code. It does not grant rights to the "Nels" name or logo.

To report a security issue, see [SECURITY.md](SECURITY.md).
