# Automated Semantic Versioning — Design

**Date:** 2026-06-18
**Status:** Approved (pending spec review)

## Goal

Automatically bump versions for the three Nels subprojects using Semantic
Versioning, driven by the Conventional Commit history that the repo already
follows. Each subproject is versioned independently and bumps only when its own
files change. Releases produce a version-file bump, a per-package changelog, a
git tag, and a GitHub Release.

## Context

Mixed-language monorepo, three independently-deployed components:

| Component | Language / stack | Version file | Current version |
|-----------|------------------|--------------|-----------------|
| backend   | Rust (axum/sqlx) | `backend/Cargo.toml`    | 0.1.0 |
| frontend  | Svelte 5 (Vite, pnpm) | `frontend/package.json` | 1.0.0 |
| marketing | SvelteKit 2 (pnpm) | `marketing/package.json` | 0.0.1 |

Each deploys via its own path-filtered workflow on push to `main`
(`.github/workflows/deploy-{backend,frontend,marketing}.yml`). Commits are
squash-merged from PRs and already use Conventional Commit prefixes
(`feat`, `fix`, with issue/area scopes such as `feat(#89)` / `fix(settings)`).
There is no existing release tooling, no git tags, and no changelog.

## Approach: release-please (manifest mode)

Use Google's [`release-please`](https://github.com/googleapis/release-please)
in **manifest mode**, which natively supports multiple independently-versioned
packages across different languages in one repo.

### How it works

1. A workflow runs `release-please` on every push to `main`.
2. release-please scans commits since each package's last release, attributing
   each commit to a package by the **file paths it changed**.
3. For each package with releasable commits it maintains an always-open
   **release PR** that bumps the version file and updates the changelog. Bump
   level is derived from Conventional Commit types affecting that package:
   - `fix:` → **patch**
   - `feat:` → **minor**
   - `feat!:` / `fix!:` / `BREAKING CHANGE:` footer → **major**
4. Merging a package's release PR cuts the release: the version-file bump and
   changelog land on `main`, and release-please creates the git **tag** and
   **GitHub Release**.

Nothing is released until a release PR is merged, so version cutting stays an
explicit human action while the bump math and changelog are automatic.

### Independent versioning

The commit-to-package attribution is by changed path, so a `feat` touching only
`frontend/**` bumps only `frontend`. Scopes in commit messages (e.g. `(#89)`,
`(settings)`) are ignored for bump decisions — only the `feat`/`fix`/`!` type
prefix and changed paths matter, so the existing commit style works unchanged.

## Files to add

### `release-please-config.json`

Declares the three packages and per-package release types.

```json
{
  "$schema": "https://raw.githubusercontent.com/googleapis/release-please/main/schemas/config.json",
  "packages": {
    "backend":   { "release-type": "rust", "component": "backend" },
    "frontend":  { "release-type": "node", "component": "frontend" },
    "marketing": { "release-type": "node", "component": "marketing" }
  },
  "separate-pull-requests": true,
  "tag-separator": "-",
  "bootstrap-sha": "759748da688ce3e37676b3a655a428dd1325174e"
}
```

- `release-type: rust` updates `backend/Cargo.toml`; `node` updates the two
  `package.json` files. Each also generates a per-package `CHANGELOG.md`.
- `separate-pull-requests: true` → one release PR per package (clean, matches
  independent deploys) rather than a single combined PR.
- `tag-separator: "-"` produces tags like `backend-v0.1.0`,
  `frontend-v1.0.0`, `marketing-v0.0.1` (component + `-v` + version).
- `bootstrap-sha` pins the starting commit to **current `main` HEAD**
  (`759748d`) so the first run only considers commits *after* this point —
  giving a **clean changelog start** rather than backfilling all history.

### `.release-please-manifest.json`

Seeds current versions so the first run continues from them rather than 0.0.0.

```json
{
  "backend": "0.1.0",
  "frontend": "1.0.0",
  "marketing": "0.0.1"
}
```

### `.github/workflows/release-please.yml`

```yaml
name: release-please

on:
  push:
    branches: [main]

permissions:
  contents: write
  pull-requests: write

jobs:
  release-please:
    runs-on: ubuntu-latest
    steps:
      - uses: googleapis/release-please-action@v4
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
          config-file: release-please-config.json
          manifest-file: .release-please-manifest.json
```

Uses the default `GITHUB_TOKEN` (no extra secrets). The `contents: write` and
`pull-requests: write` permissions let it open/maintain release PRs and create
tags + Releases.

## Deploys: unchanged, decoupled

The three deploy workflows are **not** modified. They continue to fire on every
push to `main` touching their paths. Known, accepted side effect: merging a
release PR edits that package's version file, which re-triggers its deploy — a
second, harmless deploy of the version-bumped build. Gating deploys on release
tags instead is explicitly **out of scope** for this change and can be a later
follow-up.

## Edge cases & decisions

- **Clean start (decided):** changelogs begin from `759748d` onward via
  `bootstrap-sha`; existing history is not reconstructed.
- **Commits with no releasable type** (e.g. `chore:`, `docs:`, `refactor:`
  with no `feat`/`fix`) do not trigger a bump — release-please simply doesn't
  open a release PR for that package. This is expected.
- **A merge touching multiple packages** opens/updates a release PR for each
  affected package independently.
- **`GITHUB_TOKEN`-created PRs and CI:** standard release-please caveat — PRs
  opened by the default token don't themselves trigger other `push`-on-`main`
  workflows until merged. Not a problem here since our deploys key off `push`
  to `main`, which a merge does produce.

## Follow-up (2026-06-19): release-gated deploys

Implemented after the initial rollout. The three deploy workflows no longer
trigger on path-filtered pushes to `main`. Instead they deploy exactly when a
release is cut for their package.

**Why not a `push: tags` trigger.** The intuitive approach — fire each deploy on
its `<component>-v*` tag — does not work here: release-please creates those tags
using the default `GITHUB_TOKEN`, and GitHub deliberately does **not** trigger
workflows from tags/commits pushed by `GITHUB_TOKEN` (recursion guard). A
`push: tags` deploy would therefore fire zero times on a real release. Avoiding
that without a long-lived PAT drives the design below.

**Mechanism.** Each deploy workflow becomes a **reusable workflow**
(`on: workflow_call`, plus `workflow_dispatch` for manual override). The
`release-please` workflow surfaces release-please's per-package
`<path>--release_created` outputs as job outputs and calls each deploy workflow
from within the same run, gated on that flag:

```yaml
# release-please.yml
release-please:
  outputs:
    backend_released:   ${{ steps.rp.outputs['backend--release_created'] }}
    frontend_released:  ${{ steps.rp.outputs['frontend--release_created'] }}
    marketing_released: ${{ steps.rp.outputs['marketing--release_created'] }}
deploy-backend:
  needs: release-please
  if: needs.release-please.outputs.backend_released == 'true'
  uses: ./.github/workflows/deploy-backend.yml
  secrets: inherit
# …frontend / marketing identical
```

Consequences:
- Merging code to `main` no longer deploys. A package deploys only when its
  release PR is merged — that run is what sets `<package>--release_created` and
  invokes the deploy.
- Exactly one deploy per release; the prior double-deploy on release-PR merges
  is eliminated.
- No PAT/extra secret — everything stays on `GITHUB_TOKEN`, with the deploy
  running inside the release-please workflow run rather than off a tag event.
- The deploy job inherits the run's commit (the release-PR merge on `main`), so
  it builds the released tree (with the bumped version files).
- `workflow_dispatch` is retained as a manual override (e.g. redeploy without a
  new release); a manual run deploys current `main` HEAD.
- Each deploy job carries `if: github.ref == 'refs/heads/main'`. On the
  release-gated path `github.ref` is the caller's ref (release-please runs on
  push to `main`), so the guard always passes; on a manual `workflow_dispatch`
  it ensures the prod secrets can only deploy from `main`, never an arbitrary
  branch/tag.

## Out of scope

- Publishing build artifacts to GitHub Releases.
- Publishing any package to a registry (npm/crates.io) — all three are private.
- Changing the Conventional Commit conventions already in use.

## Verification

Pre-merge (local / CI-checkable):
1. `release-please-config.json` and `.release-please-manifest.json` are valid
   JSON and the package keys match the three subproject directories.
2. The workflow YAML parses and references the config/manifest files by the
   correct names.
3. Manifest versions exactly match the current version files
   (0.1.0 / 1.0.0 / 0.0.1).

Post-merge (manual, requires the workflow on `main`):
4. After the next `feat`/`fix` merge, confirm release-please opens a release PR
   for the affected package with the correct bump and a changelog entry.
5. Merging that PR creates the expected tag (`<component>-v<version>`) and a
   GitHub Release.
