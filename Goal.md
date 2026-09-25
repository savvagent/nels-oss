We're building a budgeting app. It will be AI driven. it will allow the user to do the following through a conversational interface:

1. Manage Budgets
- Set up one or more budgets: The user can specify their income, expenses, and savings goals. The AI will help them create a personalized budget based on their financial situation.
- Share budgets: Users can share their budgets with family members or financial advisors for collaborative planning and feedback.
2. Track Expenses
- Log expenses: Users can easily log their daily expenses by simply telling the AI what they spent money on. The AI will categorize the expenses and provide insights into spending habits.
- Set reminders: The AI can send reminders to log expenses or notify users when they are approaching their budget limits.
3. Analyze Spending
- Generate reports: The AI can generate detailed reports on spending patterns, highlighting areas where the user can save money or adjust their budget.
- Provide insights: The AI can offer insights and suggestions based on the user's spending habits, such as recommending cheaper alternatives or suggesting ways to reduce expenses.
4. Goal Setting and Tracking
- Set financial goals: Users can set specific financial goals, such as saving for a vacation or paying off debt. The AI will help them create a plan to achieve these goals and track their progress.
- Provide motivation: The AI can offer encouragement and motivation to help users stay on track with their financial goals, celebrating milestones and providing positive reinforcement.

---

## Implementation Status (as of 2026-06-09)

Stack: Rust/Axum + SQLx + pgvector backend; Svelte 5 frontend; TOTP passwordless auth; Gemini AI (with offline pattern-matching fallback). Single git monorepo.

| Pillar | Status | Notes |
|--------|--------|-------|
| 1. Manage Budgets | ✅ Done | Multi-budget CRUD, default budget, auto-categories on signup, email sharing with view/edit/owner permissions, audit log. |
| 2. Track Expenses | ✅ Done | Conversational logging + AI auto-categorization, plus budget-limit notifications (category & budget at 80%/100%, deduped) and recurring reminders (daily/weekly/monthly) fired by a 60s scheduler into an in-app feed. REST: /api/notifications, /api/reminders. Chat: CREATE_REMINDER + inline limit alerts. |
| 3. Analyze Spending | 🟡 Partial | Conversational analysis with budget/transaction context + pgvector semantic memory. **Structured aggregation API live:** `GET /api/budgets/:id/report` with preset (`this_month`/`last_month`/`this_year`/`all_time`) + custom time ranges, by-category / income-expense-savings summary / spending-trend / top-transactions sections, and an AI narrative (deterministic offline template fallback; `narrative=false` skips the LLM). View-permission enforced via `check_permission`. **Charts UI still outstanding.** |
| 4. Goal Setting & Tracking | ✅ Done (chat-only) | Goals (savings & debt) per budget with hybrid progress (contributions ledger + optional linked category). REST routes + conversational CREATE_GOAL / ADD_GOAL_CONTRIBUTION actions; AI reports progress and celebrates milestones. No dedicated UI yet (chat-only). |

### Known architecture issues
- ✅ **Auth persistence — FIXED (2026-06-09).** Sessions and in-progress register/login challenges now live in PostgreSQL (`sessions` table, 30-day TTL; `auth_flows` table, 10-min TTL) instead of in-memory HashMaps, so a backend restart no longer logs users out or kills auth flows. Expired rows are purged hourly by a background task. Verified end-to-end across a real restart.
- CORS is wide open (`allow_origin(Any)`).
- Sessions have no sliding-window refresh (fixed 30-day expiry from issue time).

### Suggested next work
1. ~~Fix auth persistence~~ ✅ done.
2. ~~Build Pillar 4 (Goals)~~ ✅ done (chat-only; no dedicated UI).
3. ~~Round out Pillar 2 (reminders/notifications)~~ ✅ done. ~~Pillar 3 structured reports/aggregation API~~ ✅ done. Remaining: Pillar 3 charts UI + chat integration of the reports endpoint.

