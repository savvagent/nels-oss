# Comparison Pages — Plan 2: AI-Native Rivals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish the five `ai-native` deep dives — `chatgpt`, `cleo`, `era`, `origin`, `copilot` — with verified facts and six-locale copy.

**Architecture:** Plan 1 built the route, template, and data module. Each task here re-verifies one competitor's facts against its own site, writes the sixteen prose keys in six locales, flips `published: true`, and lets the derived prerender/sitemap wiring pick the page up automatically.

**Tech Stack:** SvelteKit 2, svelte-i18n, vitest, pnpm.

**Reference spec:** `docs/superpowers/specs/2026-07-27-competitor-comparison-pages-design.md` §1, §7.
**Depends on:** Plan 1 (all six tasks merged).

## Global Constraints

- Marketing uses **pnpm**: `cd marketing && pnpm test && pnpm run build && pnpm check`.
- **Re-verify every figure against the vendor's own pricing page before writing copy.** The values seeded in Plan 1 came partly from third-party reporting and are a starting point, not truth. If a figure has changed, update `competitors.js` **and** bump `verifiedOn` in the same commit.
- **Every page must name something the competitor does better** (`compare.{slug}.strength`). Non-negotiable — it is both the honesty rule and the legal posture (spec §7).
- No competitor logos, screenshots, or trademark styling. Outbound competitor links keep `rel="nofollow noopener"`.
- No prices in locale JSON — they live in `competitors.js` and interpolate.
- Meta titles ≤60 chars, meta descriptions ≤155 chars (`docs/marketing/nels-seo-strategy.md` §6).
- Six locales; non-English carries the existing `_meta.machineTranslated` convention.
- No self-attribution in commits.

---

## The sixteen required keys per competitor

Every published competitor MUST define all of these, or the copy gate (shipped in Plan 1) fails:

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

---

## File Structure

- **Modify:** `marketing/src/lib/compare/competitors.js` — flip `published`, correct facts, bump `verifiedOn`.
- **Modify:** `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json` — 16 keys × 5 competitors.
- **Modify:** `marketing/src/lib/compare/i18n-parity.test.js` — only if Task 1 finds gaps; the gate itself shipped in Plan 1.

---

### Task 1: Verify and extend the published-copy gate

> **This gate already exists.** Plan 1's final review found that publishing a competitor with no
> copy produced a fully key-leaking page while the build stayed green, so the guard was landed in
> Plan 1 instead (`marketing/src/lib/compare/i18n-parity.test.js`, commit `dfbf07e`). It loops
> published competitors × six locales over the sixteen required keys plus `strength`, and is
> vacuous only while nothing is published. Do not rewrite it.

**Files:**
- Modify: `marketing/src/lib/compare/i18n-parity.test.js` (only if gaps are found)

**Interfaces:**
- Consumes: `COMPETITORS` from Plan 1.
- Produces: confirmation that the gate arms correctly, plus the meta-title/description length and no-hardcoded-price checks if they are not already present.

This task is now a verification step, not a build step. Confirm the existing gate actually fires
before you write any copy, and add only what is genuinely missing.

- [ ] **Step 1: Read the gate that already exists**

Open `marketing/src/lib/compare/i18n-parity.test.js` and read the `published competitors have
complete copy` block. Confirm it covers, for every `published: true` competitor across all six
locales, these sixteen keys plus the competitor's own `strength` key:

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

Note what it does NOT yet assert, for Step 3.

- [ ] **Step 2: Prove the gate actually fires**

Temporarily set one competitor's `published: true` in `competitors.js` WITHOUT adding any copy, then run:
`cd marketing && pnpm vitest run src/lib/compare/i18n-parity.test.js`
Expected: FAIL with roughly a dozen `expected 'undefined' to be 'string'` errors. Revert the flag immediately — do not commit it. If it does NOT fail, stop and fix the gate before writing any copy.

- [ ] **Step 3: Add only what is missing**

Check whether the gate also asserts meta-title ≤60 chars, meta-description ≤155 chars, and no hardcoded `$` price in the per-competitor locale blocks. Add whichever are absent; commit only if you changed something.

---

### Task 2: `/compare/chatgpt` — Finances in ChatGPT

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Task 1 gate; the page template from Plan 1 Task 4.
- Produces: `compare.chatgpt.*` (16 keys × 6 locales); `chatgpt.published = true`.

Do this one first — it is the sharpest competitive threat and the hardest to write well.

- [ ] **Step 1: Re-verify the facts**

Check `https://openai.com/index/personal-finance-chatgpt/` and `https://help.openai.com/en/articles/20001222-finances-in-chatgpt`. Confirm: which paid tiers include Finances, whether it is still US-only, whether it is still read-only, and the current ChatGPT Plus price. Update the `chatgpt` record and bump `verifiedOn` if anything moved.

Baseline as of 2026-07-27: launched for Pro 2026-05-15, Plus 2026-06-25; 12,000+ institutions via Plaid; dashboard of spending, subscriptions, upcoming payments; read-only — cannot move money, pay a bill, or place a trade; ChatGPT Plus $20/mo.

- [ ] **Step 2: Write the English copy**

Add to `en.json` under `compare`. The argument must rest on the five verifiable differences from spec §1, not on "we also have AI":

```json
"chatgpt": {
  "seo": {
    "title": "Nels vs ChatGPT for budgeting",
    "description": "ChatGPT can now read your accounts. Nels writes the budget you act on — shared, envelope-based, and $3/mo. An honest comparison."
  },
  "headline": "Nels vs. Finances in ChatGPT",
  "intro": "ChatGPT connects to your bank now, and it's genuinely good at explaining where your money went. It just can't hold the plan you're trying to follow — it reads your accounts, it doesn't run your budget.",
  "whoForTitle": "Who each one is for",
  "whoFor": "If you want to ask questions about money you've already spent, ChatGPT answers them well and you may already be paying for it. If you want a budget your household edits together and executes month to month, that's a different tool.",
  "strengthTitle": "What ChatGPT does better",
  "strength": "It's already on your phone, it answers open-ended financial questions far beyond budgeting, and its account coverage through Plaid is broader than ours. If you only want insight into past spending, you don't need Nels for that.",
  "verdictTitle": "The honest verdict",
  "verdict": "A dashboard is not a budget. ChatGPT shows you what happened; Nels holds what you decided should happen — envelope categories you allocate to, shared with your household, with an audit log of who changed what.",
  "faq": {
    "q1": "Can ChatGPT manage my budget?",
    "a1": "It can read your connected accounts and answer questions about your spending, but access is read-only — it can't move money, pay a bill, or place a trade, and it doesn't maintain envelope categories you allocate to.",
    "q2": "Is ChatGPT's finance feature free?",
    "a2": "No. It's part of the paid ChatGPT tiers and was US-only at launch. Nels is a fraction of that cost if budgeting is what you actually need.",
    "q3": "Can my partner and I share a budget in ChatGPT?",
    "a3": "No. Finances in ChatGPT is tied to your individual account. Nels is built for households, with shared budgets, per-person permissions, and an audit log."
  }
}
```

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched. Do not translate brand names ("Nels", "ChatGPT", "Plaid").

- [ ] **Step 4: Publish**

In `competitors.js`, set the `chatgpt` record's `published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS. The build now prerenders six new URLs (`/{locale}/compare/chatgpt`), and the sitemap includes them — both derived automatically.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs ChatGPT comparison page"
```

---

### Task 3: `/compare/cleo` — Cleo

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Task 1 gate.
- Produces: `compare.cleo.*` (16 keys × 6 locales); `cleo.published = true`.

- [ ] **Step 1: Re-verify the facts — this record is flagged `second-hand`**

`web.meetcleo.com` returned HTTP 403 to automated fetches during design, so the seeded tiers (Plus $5.99, Pro $8.99, Builder $14.99) come from third-party reporting. **Open the pricing page in a real browser** and confirm each tier. Then either correct the numbers and **remove** `pricing.note: 'second-hand'`, or keep the note if the page still can't be confirmed. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: Cleo is the closest UX analog — chat-first, genuinely fun, with a free tier. The distinction is that Cleo monetizes cash advances and credit-building, while Nels is a budgeting ledger. Be careful and factual about the cash-advance product; note express fees exist without characterizing them pejoratively.

Required keys, following the exact structure shown in Task 2 Step 2: `seo.title`, `seo.description`, `headline`, `intro`, `whoForTitle`, `whoFor`, `strengthTitle`, `strength`, `verdictTitle`, `verdict`, `faq.q1`–`q3`, `faq.a1`–`a3`.

`strength` must acknowledge, at minimum: Cleo has a real free tier, its chat personality is better, and it offers cash advances Nels does not.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `cleo.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Cleo comparison page"
```

---

### Task 4: `/compare/era` — Era

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Task 1 gate.
- Produces: `compare.era.*` (16 keys × 6 locales); `era.published = true`.

- [ ] **Step 1: Re-verify the facts**

Check `https://www.era.app`. Era publishes no pricing, so the record carries `model: 'undisclosed'` and `note: 'undisclosed'`. Confirm the three-product structure (Context / Agency / Thesis) is still current and that pricing is still unpublished. If Era has since published prices, replace the `undisclosed` model with real tiers and drop the note.

- [ ] **Step 2: Write the English copy**

The angle: Era is infrastructure — an MCP server that hands your financial context to *any* AI agent — plus an app and an investing research tool. That is a broader, more technical proposition than a household budget. Nels is one focused product; Era is a platform.

Because pricing is undisclosed, the copy must not speculate about cost. The rendered `compare.note.undisclosed` caveat covers it.

Required keys as listed in Task 2 Step 2. `strength` must acknowledge Era's open agent interoperability (it works with Claude and ChatGPT rather than replacing them) and its investing research, neither of which Nels has.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `era.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Era comparison page"
```

---

### Task 5: `/compare/origin` — Origin

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Task 1 gate.
- Produces: `compare.origin.*` (16 keys × 6 locales); `origin.published = true`.

- [ ] **Step 1: Re-verify the facts — the seeded price is a promo**

Origin was advertising **$1 for the first year** at design time, which is not list price. Find the renewal/list price on `https://useorigin.com` and record **both**: put the list price in `tiers` and keep `note: 'promo'` only if the $1 offer is still running. A comparison table that quotes a promotional rate as if it were the standing price is exactly the kind of claim spec §7 exists to prevent.

- [ ] **Step 2: Write the English copy**

The angle: Origin bundles budgeting, investing, and planning with AI guidance and no human advisors — broader scope than Nels, at a higher list price. Nels does one job (the household budget) and does it conversationally.

Required keys as listed in Task 2 Step 2. `strength` must acknowledge Origin's investment tracking and planning scope, which Nels does not attempt.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `origin.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build`
Expected: PASS; six new prerendered URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Origin comparison page"
```

---

### Task 6: `/compare/copilot` — Copilot Money

**Files:**
- Modify: `marketing/src/lib/compare/competitors.js`
- Modify: `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json`

**Interfaces:**
- Consumes: the Task 1 gate.
- Produces: `compare.copilot.*` (16 keys × 6 locales); `copilot.published = true`.

- [ ] **Step 1: Re-verify the facts**

Check `https://copilot.money/pricing`. Confirm the monthly and annual prices and whether an Android client now exists — the seeded record assumes an Apple-centric product, and that claim must not outlive its accuracy. Bump `verifiedOn`.

- [ ] **Step 2: Write the English copy**

The angle: Copilot is a beautifully built native app with excellent automatic categorization, aimed at an individual on Apple hardware. Nels is cross-platform, conversational, and household-shared.

Required keys as listed in Task 2 Step 2. `strength` must acknowledge Copilot's native app quality and categorization accuracy.

Take care with the name: the page is about **Copilot Money**, not GitHub Copilot or Microsoft Copilot. Use the full product name in the headline and meta title to avoid ambiguity.

- [ ] **Step 3: Translate into the five other locales**

Machine-translate all sixteen keys into `es`, `fr`, `de`, `it`, `pt`. Keep `_meta` untouched.

- [ ] **Step 4: Publish**

Set `copilot.published: true`.

- [ ] **Step 5: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build && pnpm check`
Expected: PASS. All five `ai-native` pages now prerender — 30 URLs.

- [ ] **Step 6: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/i18n/locales/
git commit -m "feat(marketing): add Nels vs Copilot Money comparison page"
```

---

## Definition of Done

- [ ] `cd marketing && pnpm test && pnpm run build && pnpm check` all green.
- [ ] Five deep dives live at `/{locale}/compare/{chatgpt,cleo,era,origin,copilot}` in all six locales (30 URLs).
- [ ] Every page renders a "verified as of" line linking to the vendor's own pricing page.
- [ ] Every page names something the competitor does better.
- [ ] No page hardcodes a price in locale JSON.
- [ ] The hub links all five under "AI money assistants".
