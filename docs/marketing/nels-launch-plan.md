# Nels Launch Plan

**Product:** Nels — AI Budgeting Co-Pilot
**Offer:** Nels Pro — $3/mo (US) or $36/yr, **7-day free trial, no credit card**, passwordless sign-in, includes automatic bank-account syncing (#339)
**Marketing site:** nels.money · **App:** app.nels.money · **Brand color:** #aa3bff

> **Scope of this doc.** This is the *time-sequenced go-to-market plan* for the launch moment. It complements — and does not repeat — [`nels-seo-strategy.md`](./nels-seo-strategy.md), which owns evergreen positioning, personas, keyword/SEO, blog roadmap, and the standing distribution plan. When this plan says "the hero message" or "persona Sarah," those are defined there.

## 0. Working assumptions (confirm / adjust)

| Decision | Assumption used here | Change if… |
|---|---|---|
| Launch type | **Public GA** with a 2–3 week waitlist warm-up | You'd rather stay invite-only beta longer |
| Anchor channel | **Product Hunt** launch day = "Launch Day" / Week 0 | You don't want a PH launch |
| Primary audience | Existing tech-forward budgeter/couple personas; AI–early-adopter communities as the beachhead | You want to lead with families or mainstream budgeters |
| Conversion goal | Trial start → active paid (post 7-day trial) | Pricing/trial changes |
| Timeline | **Date-relative** (Week -4 … Week +4); drop a real date into "Launch Day" | You have a fixed date |

---

## 1. Launch goals & success metrics

Pick the numbers that match ambition; these are indie-launch reference targets, not commitments.

| Metric | Launch Day | Week +4 (T+30) | Why it matters |
|---|---|---|---|
| Waitlist signups (pre-launch) | 300–750 by Week 0 | — | Warm audience to convert on day 0 |
| Product Hunt rank | Top 5 of the day | — | Peak referral + credibility halo |
| New trial starts | 150–400 | 800–1,500 cumulative | Top of funnel |
| Trial → paid conversion | — | 8–15% | The number that decides viability |
| Activation (created ≥1 budget + logged ≥1 expense via chat) | 55%+ of signups | 60%+ | Proves the core loop lands |
| Day-7 retention | — | 35%+ | Early signal the habit sticks |

**Instrumentation must be live before Launch Day** (see §8 gate): signup source tagging (UTM), trial-start event, activation events (budget created, first expense logged), checkout started/completed. Analytics approach is defined in SEO strategy §11 — extend it with these launch-specific events.

---

## 2. Launch messaging — the hook

Lead with the single sharpest wedge, not a feature list.

- **One-liner (the tweet):** "Budget by texting an AI. No spreadsheets, no passwords, no jargon. Nels is a budgeting co-pilot you actually talk to."
- **Product Hunt tagline:** "The budgeting app you talk to — AI co-pilot for your money."
- **Three proof pillars** (each already true in-product):
  1. **Conversational** — log expenses, set budgets, ask "what % went to food?" in plain language.
  2. **Private & passwordless** — TOTP sign-in, no password to leak; your data isn't the product.
  3. **Built for two** — share budgets with a partner/advisor with view/edit/owner roles and a full audit log.
- **Objection pre-empt for launch copy:** we're newer, so some insights keep improving — but the fundamentals (multi-budget, tracking, sharing, goals, forecasting, rollover, project budgets) are shipping today.

Keep all launch copy consistent with the tone/voice guide in SEO strategy Appendix.

---

## 3. Channel priority

**Tier 1 (own the launch moment):** Product Hunt, waitlist email, personal/founder X/Twitter, Hacker News (Show HN), r/personalfinance + r/ynab-adjacent communities.
**Tier 2 (amplify):** finance/AI newsletters, Indie Hackers, LinkedIn, relevant Discord/Slack communities, Reddit AMA-style follow-up.
**Tier 3 (compounding, starts pre-launch, pays off after):** SEO/blog (owned by SEO strategy §10), YouTube/short-form demo, micro-influencers in personal-finance.

Rule: Tier 1 gets a written runbook and a named owner. Tier 2/3 get a checklist.

---

## 4. Launch timeline (date-relative)

### Week -4 — Foundation
- [ ] Lock the launch date; work backward from it.
- [ ] Stand up / verify **waitlist capture** on nels.money (email + source tag).
- [ ] Confirm analytics events fire end-to-end (see §1) in production.
- [ ] Draft all launch assets (§8); begin PH gallery + demo video.
- [ ] Build the outreach list: 20–30 newsletters/communities/creators with contact + angle.
- [ ] Seed the founder's network privately ("launching in a few weeks, want early access?").

### Week -3 — Warm-up
- [ ] Open the waitlist publicly; start teaser posting (build-in-public: 1 screenshot/insight per week).
- [ ] Recruit **10–20 launch-day supporters** (people who'll genuinely comment/upvote, not vote rings).
- [ ] Line up Product Hunt (create the upcoming/"coming soon" page to collect followers).
- [ ] First outreach wave to Tier 2 newsletters (they plan 2–4 weeks out).

### Week -2 — Dry run
- [ ] Full product QA of the trial→paid path (Stripe: `trialing` → `active`, no-card trial, cancel, customer portal). See [`docs/billing/stripe-setup.md`](../billing/stripe-setup.md).
- [ ] Finalize PH assets, first comment, and maker comment. Schedule.
- [ ] Write the 3 waitlist emails (T-2 tease, Launch Day, T+2 "last chance to be early").
- [ ] Draft HN "Show HN" post + honest comment answering "why another budgeting app?"
- [ ] Load-check: can the backend (Fly nels-api) and app absorb a spike? Confirm scaling headroom.

### Week -1 — Load & lock
- [ ] Freeze non-critical deploys after mid-week (deploys are release-gated — a feature PR merge ≠ deploy; only the release-please PR ships). Ensure the launch build is what's live.
- [ ] Confirm status/monitoring + an on-call human for Launch Day.
- [ ] Pre-write social thread, LinkedIn post, and 5–7 reply snippets for common questions.
- [ ] Send waitlist "we launch in X days" email.

### Week 0 — **Launch Day**
See the Product Hunt runbook (§5). Also: waitlist launch email at go-live, Show HN, founder thread, community posts, newsletters go out. Founder spends the day *replying*, not broadcasting.

### Week +1 to +4 — Sustain (§11)
Post-launch content, retargeting, conversion follow-ups, first retrospective.

---

## 5. Product Hunt launch runbook (anchor moment)

PH resets ~12:01 AM Pacific; earlier in the day = more hours to accumulate votes.

**T-2 weeks:** create the "Coming Soon" page; drive waitlist followers to it.
**Day-before checklist:**
- [ ] Gallery images (5–7): hero, chat-logging a expense, a budget report, sharing/roles, mobile view.
- [ ] 60–90s demo video: "I just spent $12 on lunch" → categorized → "how am I doing on food?" → answer.
- [ ] Tagline + description written; first comment (maker's story: why you built Nels) drafted.
- [ ] Offer for PH audience decided (e.g. extended trial or a launch-week perk — keep it honest and simple).

**Launch Day cadence:**
1. Go live 12:01 AM PT. Post the maker's first comment immediately.
2. Notify waitlist + supporters that it's live (link directly, ask for honest feedback — never "please upvote").
3. Founder replies to *every* comment within minutes for the first hours.
4. Midday + evening nudges to different time zones (waitlist segments, communities).
5. End of day: thank-you post regardless of rank.

**Do not:** buy votes, run vote rings, or use fake accounts — PH penalizes it and it torches credibility. Genuine engagement only.

---

## 6. Channel playbooks (condensed)

- **Hacker News (Show HN):** Title "Show HN: Nels – budget by talking to an AI (Rust + pgvector)." Lead comment: the technical story (why Rust/Axum, pgvector RAG, passwordless TOTP) — HN rewards substance and candor. Expect hard questions on privacy and "why not a spreadsheet"; answer plainly.
- **Reddit:** r/personalfinance is strict about promotion — lead with value, disclose you're the maker, or use the weekly self-promo threads. Softer fit: r/ynab refugees, r/SideProject, r/artificial. One authentic post > five spammy ones.
- **X/Twitter:** Founder build-in-public thread with the demo GIF as the hook. Tag no one gratuitously; let the product carry it.
- **Waitlist email (3 sends):** tease (T-2) → live (Day 0, single clear CTA to start the free trial) → "be an early user" (T+2). No-card trial is the CTA's friction-remover — say so.
- **Newsletters/communities (Tier 2):** personalized 3-sentence pitch + the one-liner + a 30s Loom. Ask for a mention, not a favor.
- **LinkedIn:** the "why I built this" narrative angle for the couples/household-manager persona.

---

## 7. Launch-week content calendar

| Day | Owned (blog/site) | Social | Community |
|---|---|---|---|
| Launch Day | Launch announcement post | Founder thread + demo | PH, Show HN, waitlist email |
| +1 | — | "24 hrs in" recap | Reddit self-promo threads |
| +2 | "How Nels categorizes expenses with AI" | Feature clip: sharing/roles | Newsletter mentions land |
| +3 | — | Feature clip: forecasting/goals | Indie Hackers milestone post |
| +5 | Comparison: Nels vs spreadsheets/legacy apps (per SEO §10) | Testimonial/quote card | LinkedIn narrative post |

Blog topics should slot into the existing SEO blog roadmap (strategy §10), not compete with it.

---

## 8. Readiness gate — assets & checks (must be ✅ before Week -1)

**Assets**
- [ ] PH gallery (5–7 images) + 60–90s demo video
- [ ] og-image / social cards current (exists: `marketing/static/og-image.png` — verify it reflects launch messaging)
- [ ] Launch blog post, 3 waitlist emails, HN post, founder thread, LinkedIn post
- [ ] 5–7 canned reply snippets for FAQs (privacy, pricing, "why another app," data export/deletion)

**Product & infra**
- [ ] Trial→paid path verified in prod (Stripe no-card trial, cancel, portal) — [`stripe-setup.md`](../billing/stripe-setup.md)
- [ ] Signup, activation, and checkout analytics events firing (§1)
- [ ] Data privacy/deletion page current ([`docs/data-privacy-deletion.md`](../data-privacy-deletion.md)) — privacy is a launch talking point, so it must hold up
- [ ] Backend/app scaling headroom confirmed; monitoring + on-call human for Launch Day
- [ ] The launch build is actually the deployed build (release-gated deploy landed)

---

## 9. Pricing & conversion mechanics

- The **7-day, no-card trial** is the core acquisition lever — hammer "no credit card" in every CTA; it removes the biggest signup objection.
- Trial ends → collect payment. Confirm the in-trial experience nudges toward setup (a user who hasn't created a budget won't convert). Consider a Day-5 "your trial ends in 2 days + here's what you've tracked" email.
- Annual is now the same effective rate as monthly ($36/yr = 12 × $3/mo, since #339 repriced monthly to $3 for US) — do not frame it as a discount; lead with the convenience of one yearly charge instead.
- Track **where** trials come from (UTM) so post-launch spend goes to the channels that actually converted, not just the ones that drove raw signups.

---

## 10. Risks & contingencies

| Risk | Mitigation |
|---|---|
| Traffic spike overwhelms backend | Load-check Week -2; confirm Fly scaling; have a status page + calm messaging ready |
| PH launch underperforms | It's one channel — HN/Reddit/newsletters/waitlist are independent; don't stake everything on rank |
| "Why another budgeting app?" skepticism | Pre-written honest answer leading with conversational + passwordless + sharing; don't overclaim maturity |
| Privacy questions (finance data + AI) | Point to passwordless auth + data deletion page; be specific about what the AI sees and stores |
| Trial signups don't activate | Day-1 onboarding nudge + Day-5 trial-ending email; measure activation, not just signups |
| Deploy confusion (feature merged ≠ shipped) | Freeze + verify the release-gated build is live before Week -1 |

---

## 11. Post-launch (Week +1 → +4)

- [ ] Retarget non-converters and lapsed trials with a "what you missed" email.
- [ ] Publish 2–3 launch-week blog posts feeding the SEO engine (strategy §10).
- [ ] Collect + publish first testimonials/quotes.
- [ ] Reddit/Indie Hackers "results of my launch" recap post (these perform well and drive a second wave).
- [ ] **T+30 retrospective:** channel-by-channel signup + conversion, cost per paid user, what to double down on. Feed learnings back into the SEO strategy's standing distribution plan (§12 there).

---

## 12. Owners & tools

Fill in before Week -4.

| Area | Owner | Tool |
|---|---|---|
| Overall launch coordination | | this doc |
| Product Hunt | | PH |
| Waitlist + email | | (email tool TBD) |
| Social / build-in-public | | X, LinkedIn |
| Analytics & reporting | | (per SEO §11) |
| Product/infra readiness & on-call | | Fly, monitoring |

---

*This plan is intentionally date-relative. Drop the real launch date into "Launch Day / Week 0" and every other week shifts with it.*
