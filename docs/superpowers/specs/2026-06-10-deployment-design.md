# Nels Deployment Design — Cloudflare Pages + Fly.io + Fly Managed Postgres

Date: 2026-06-10
Status: Approved

## Goal

Deploy Nels to production: the Svelte 5 SPA on Cloudflare Pages, the Rust/Axum
API on Fly.io, backed by an existing Fly Managed Postgres (MPG) cluster with
pgvector enabled. Use platform subdomains initially (`*.pages.dev`,
`*.fly.dev`) and GitHub Actions for CI/CD.

## Topology

```
Browser
  │  https://<nels>.pages.dev        (static SPA)
  ▼
Cloudflare Pages  ── build-time VITE_API_BASE ──▶  https://nels-api.fly.dev/api/*
  │
  ▼  fetch /api/*
Fly.io app (Rust/Axum)  ── private 6PN / TLS ──▶  Fly Managed Postgres (PG16 + pgvector)
```

## Key facts (verified against the codebase)

- Frontend is a **plain Svelte 5 + Vite SPA** (not SvelteKit) → static `dist/`.
- API base is hardcoded `http://localhost:3000/api` in `App.svelte:65`.
- Backend binds a hardcoded `0.0.0.0:3000` (`main.rs:155`); CORS is `Any`.
- Auth uses `Authorization: Bearer` tokens (not cookies), so permissive CORS is
  functionally safe.
- Session/auth state is **DB-backed** (`main.rs:72`), not in-memory — backend is
  effectively stateless (the AGENTS.md "in-memory" caveat is stale).
- `sqlx` uses runtime `.bind()` queries (no `query!` macros) → **no DB needed at
  Docker build time, no `.sqlx` offline cache required.**
- `sqlx` feature `runtime-tokio-rustls` → TLS to MPG works out of the box.
- Migrations auto-run on boot (`sqlx::migrate!`).
- No `/health` route exists yet.

## Components & changes

### 1. Frontend → Cloudflare Pages
- `App.svelte`: `const API_BASE = import.meta.env.VITE_API_BASE ?? "http://localhost:3000/api"`.
- Add `frontend/public/_redirects`: `/* /index.html 200` (SPA fallback).
- Built and deployed via GitHub Actions + Wrangler (path **B**), not Cloudflare's
  native Git integration. Build env `VITE_API_BASE=https://nels-api.fly.dev/api`.

### 2. Backend → Fly.io (new files in `backend/`)
- `Dockerfile`: multi-stage `rust:1-bookworm` builder → `debian:bookworm-slim`
  runtime with `ca-certificates`. No DB access at build time.
- `.dockerignore`: exclude `target/`, `.env`, etc.
- `fly.toml`: `internal_port = 8080`, `force_https`, `auto_stop_machines`,
  `min_machines_running = 1`, HTTP health check on `/health`.
- Code:
  - bind `0.0.0.0:PORT`, reading `PORT` env (default 3000); fly.toml sets `PORT=8080`.
  - add `GET /health` → `200 OK`.
  - CORS: read optional `CORS_ALLOWED_ORIGINS` env; when unset, keep permissive
    `Any` (safe given bearer-token auth) so nothing breaks initially.

### 3. Database → Fly Managed Postgres
- pgvector already enabled on the existing cluster.
- `DATABASE_URL` set as a Fly **secret** (private/flycast connection string,
  `sslmode=require`).
- Auto-migrate-on-boot is safe at **1 machine**. Scaling >1 later will require
  moving migrations to a release/deploy step to avoid races — out of scope now,
  flagged for the future.

### 4. CI/CD → GitHub Actions (`.github/workflows/`)
- `deploy-backend.yml`: push to `main` filtered to `backend/**` → `flyctl deploy`.
  Secret: `FLY_API_TOKEN`.
- `deploy-frontend.yml`: push to `main` filtered to `frontend/**` → build SPA and
  `wrangler pages deploy frontend/dist`. Secrets: `CLOUDFLARE_API_TOKEN`,
  `CLOUDFLARE_ACCOUNT_ID`. Build env `VITE_API_BASE`.

## Secrets / config summary

| Where | Key | Value |
|-------|-----|-------|
| Fly secret | `DATABASE_URL` | MPG connection string (TLS) |
| Fly secret | `GEMINI_API_KEY` | Google Gemini key |
| Fly env (fly.toml) | `PORT` | `8080` |
| Fly env (optional) | `CORS_ALLOWED_ORIGINS` | Pages origin once known |
| GH Actions secret | `FLY_API_TOKEN` | Fly deploy token |
| GH Actions secret | `CLOUDFLARE_API_TOKEN` | Pages edit token |
| GH Actions secret | `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id |
| Pages build env | `VITE_API_BASE` | `https://nels-api.fly.dev/api` |

## First-deploy order

1. Confirm MPG cluster + pgvector; obtain `DATABASE_URL`. **(done — cluster exists)**
2. Backend: `PORT`/`/health`/CORS code changes + `Dockerfile`/`fly.toml`/
   `.dockerignore`; `fly launch --no-deploy`; set secrets; `fly deploy`; verify
   `/health` and that migrations ran.
3. Frontend: `VITE_API_BASE` change + `_redirects`; first `wrangler pages deploy`;
   verify a register/login round-trip against the Fly API.
4. Wire both GitHub Actions workflows + repo/Cloudflare secrets; confirm a push
   redeploys each side independently.

## Risks / notes

- **pgvector on MPG** — resolved (supported; enabled on the cluster).
- **CORS with credentials** — N/A; bearer-token auth, no cookies.
- **TOTP time drift** — Fly machines are NTP-synced; no action needed.
- **Migration races** — only when scaling beyond 1 machine (future).
