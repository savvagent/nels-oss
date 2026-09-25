# Comparison Pages — Plan 3: Traditional Apps and Planning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish the six remaining deep dives — `ynab`, `monarch`, `simplifi`, `everydollar` (traditional) and `range`, `boldin` (planning) — completing the comparison set.

**Architecture:** Identical mechanics to Plan 2: re-verify facts, write sixteen prose keys in six locales, flip `published: true`. The two `planning` pages differ in argument — they are framed as category differences, not head-to-heads.

**Tech Stack:** SvelteKit 2, svelte-i18n, vitest, pnpm.

**Reference spec:** `docs/superpowers/specs/2026-07-27-competitor-comparison-pages-design.md` §1, §7.
**Depends on:** Plan 1 (all tasks) and Plan 2 Task 1 (the published-copy gate).

## Global Constraints

- Marketing uses **pnpm**: `cd marketing && pnpm test && pnpm run build && pnpm check`.
- **Re-verify every figure against the vendor's own pricing page before writing copy.** Seeded values are a starting point, not truth. Correct `competitors.js` and bump `verifiedOn` in the same commit.
- **Every page must name something the competitor does better** (`compare.{slug}.strength`).
- No competitor logos, screenshots, or trademark styling. Outbound competitor links keep `rel="nofollow noopener"`.
- No prices in locale JSON.
- Meta titles ≤60 chars, descriptions ≤155 chars.
- Six locales; non-English carries `_meta.machineTranslated`.
- No self-attribution in commits.

## The sixteen required keys per competitor

```
compare.{slug}.seo.title          compare.{slug}.seo.description
compare.{slug}.headline           compare.{slug}.intro
compare.{slug}.whoForTitle        compare.{slug}.whoFor
compare.{slug}.strengthTitle      compare.{slug}.strength
compare.{slug}.verdictTitle       compare.{slug}.verdict
compare.{slug}.faq.q1             compare.{slug}.faq.a1
compare.{slug}.faq.q2             compare.{slug}.faq.a2
compare.{slug}.faq.q3             compare.{slug}.faq.a3
```

The Plan 2 Task 1 gate enforces every one of these, in every locale, for any competitor with `published: true`. See Plan 2 Task 2 Step 2 for a fully worked example of the JSON block.

---

## File Structure

- **Modify:** `marketing/src/lib/compare/competitors.js` — flip `published`, correct facts, bump `verifiedOn`.
- **Modify:** `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json` — 16 keys × 6 competitors.

---

### Task 1: `/compare/ynab` — YNAB

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Plan 2 Task 1 gate; the page template from Plan 1 Task 4.
- Produces: `compare.ynab.*` (16 keys × 6 locales); `ynab.published = true`.

Highest "vs" search volume in the set — the SEO strategy names "YNAB vs AI budgeting" (~50–150/mo) and "YNAB alternative" explicitly.

- [ ] **Step 1: Re-verify the facts**

Check `https://www.ynab.com/pricing`. Confirm monthly and annual prices and the trial length. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: YNAB and Nels share a methodology — give every dollar a job. The difference is the interface and the price. YNAB asks you to sit down and assign; Nels lets you say "spent 40 on groceries" and keeps the same envelope discipline underneath.

This is the page where Nels' `envelopeBudgeting: true` matters most, because it is the one competitor that also has it. Do not imply YNAB lacks it.

`strength` must acknowledge: YNAB's method is more mature and rigorously taught, its educational material and community are far deeper, and its rule set has a longer track record than Nels' newer insights.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched. Do not translate "Nels" or "YNAB".

- [ ] **Step 4: Publish**

Set `ynab.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs YNAB comparison page"
```

---

### Task 2: `/compare/monarch` — Monarch Money

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Plan 2 Task 1 gate.
- Produces: `compare.monarch.*` (16 keys × 6 locales); `monarch.published = true`.

- [ ] **Step 1: Re-verify the facts**

Check `https://www.monarchmoney.com/pricing`. Confirm monthly and annual prices and whether shared/household access is still included at the base tier rather than charged per seat. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: Monarch contests Nels' household-sharing differentiator directly — this is the page where "we share better" is the *weakest* claim in the set, so lead elsewhere. Lead on conversational logging, passwordless sign-in, the audit log, and price.

Also capture "Mint alternative" intent (~80–200/mo per the SEO strategy): many searchers land here from Mint's shutdown, and Monarch is the default destination they're considering.

`strength` must acknowledge Monarch's household sharing is genuinely strong and its investment tracking exceeds Nels'.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `monarch.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Monarch Money comparison page"
```

---

### Task 3: `/compare/simplifi` — Quicken Simplifi

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Plan 2 Task 1 gate.
- Produces: `compare.simplifi.*` (16 keys × 6 locales); `simplifi.published = true`.

- [ ] **Step 1: Re-verify the facts**

Check `https://www.quicken.com/simplifi`. Simplifi discounts aggressively and its advertised annual rate often differs from the renewal rate — record the **list** price and set `pricing.note: 'promo'` if the advertised figure is introductory. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: Simplifi is the closest competitor on price, so cost is not the argument — the interface is. Nels is conversational and passwordless; Simplifi is a conventional dashboard backed by Quicken's long history.

`strength` must acknowledge Quicken's decades of track record and Simplifi's reporting depth.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `simplifi.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Quicken Simplifi comparison page"
```

---

### Task 4: `/compare/everydollar` — EveryDollar

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Plan 2 Task 1 gate.
- Produces: `compare.everydollar.*` (16 keys × 6 locales); `everydollar.published = true`.

- [ ] **Step 1: Re-verify the facts**

Check `https://www.ramseysolutions.com/ramseyplus/everydollar`. Confirm what the free tier includes (historically: manual entry only, with bank syncing reserved for the paid tier — which is why the seeded record has `bankSync: 'partial'`) and the current Premium price. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: EveryDollar has a real free tier, but free means manual entry. Nels' conversational logging is the honest middle ground between manual typing and paying for full bank sync — you talk, it records.

Be respectful of the Ramsey method and its adherents. Users arrive here committed to a financial philosophy; disparaging it loses the sale outright.

`strength` must acknowledge the free tier and the surrounding Ramsey ecosystem and coaching, which Nels has no equivalent to.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `everydollar.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs. All four `traditional` pages are now live.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs EveryDollar comparison page"
```

---

### Task 5: `/compare/range` — Range

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Plan 2 Task 1 gate.
- Produces: `compare.range.*` (16 keys × 6 locales); `range.published = true`.

**This page is framed as a category difference, not a head-to-head.**

- [ ] **Step 1: Re-verify the facts**

Check `https://www.range.com`. Range does not publish tier prices, so the record carries `model: 'flat-fee'` with `note: 'undisclosed'` and named-but-unpriced tiers (Premium / Platinum / Titanium). Confirm the tier names, the "0% AUM, flat fee" claim, and that human CFP access is still included. If prices have been published, record them and drop the note.

- [ ] **Step 2: Write the English copy**

The angle: Range serves high-income households ($200k+) who need tax planning, equity compensation advice, and estate work from human CFPs. Nels is a $3–5/mo household budget. These are not substitutes, and the copy must say so plainly.

The verdict should be **"you may want both, and if you're choosing, choose by what you need"** — someone with a complex tax situation is not served by a budgeting app, and someone who wants to stop overspending on groceries does not need a wealth manager. A page that pretends Nels competes with a CFP damages credibility with exactly the sophisticated reader who found it.

`strength` is easy and should be generous: human fiduciary advice, tax planning, and estate work are things Nels does not do at all.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `range.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Range comparison page"
```

---

### Task 6: `/compare/boldin` — Boldin

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Plan 2 Task 1 gate.
- Produces: `compare.boldin.*` (16 keys × 6 locales); `boldin.published = true`.

**Also framed as a category difference.**

- [ ] **Step 1: Re-verify the facts**

Check `https://www.boldin.com/retirement/pricing/`. Seeded baseline: free Basic; PlannerPlus $144/yr; Boldin Advisors $3,200 flat fee with CFP access. Confirm all three and note that `boldin.com/pricing` 301-redirects to the `/retirement/pricing/` path — `sourceUrl` must point at the final URL, not the redirect. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: Boldin projects decades — Monte Carlo simulations, Roth conversion timing, withdrawal sequencing. Nels executes this month. The natural line is that Boldin tells you whether you can retire at 62; Nels is how you don't overspend in July. Complementary, different time horizons.

Note the rename: Boldin was formerly NewRetirement. Mention it once in the copy — people still search the old name.

`strength` must acknowledge the retirement projection engine and tax planning depth, neither of which Nels attempts (`retirementProjection: false` in Nels' own row).

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `boldin.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build && pnpm check`
Expected: PASS. All eleven deep dives now prerender — 66 URLs, plus the hub's 6 = 72 total comparison URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Boldin comparison page"
```

---

## Definition of Done

- [ ] `cd marketing && pnpm test && pnpm run build && pnpm check` all green.
- [ ] All eleven deep dives live in six locales; the sitemap lists 72 comparison URLs.
- [ ] The hub renders three segments and links every published competitor.
- [ ] `range` and `boldin` read as category comparisons, not head-to-heads.
- [ ] Every page names something the competitor does better.
- [ ] Every page carries a dated "verified as of" line linking to the vendor's own pricing page.
