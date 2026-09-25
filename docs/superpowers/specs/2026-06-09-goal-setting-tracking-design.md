# Pillar 4 — Goal Setting & Tracking (Design Spec)

**Date:** 2026-06-09
**Status:** Approved, ready for implementation planning
**Scope:** Add financial goals (savings & debt payoff), hybrid progress tracking,
AI-assisted plan/motivation, all surfaced through the existing conversational
interface. Backend: new `goals.rs` module + migration + `rag.rs` integration.
Frontend: chat-only (one discoverability prompt-chip).

## 1. Goals & Non-Goals

**Goals**
- Users create financial goals via conversation: "Save $3000 for a vacation by
  December" or "Pay off my $5000 credit card".
- Track progress toward each goal through a contributions ledger, optionally
  augmented by a linked savings category (hybrid model).
- The AI reports progress, computes the monthly amount needed to hit a target
  date, and offers encouragement / milestone celebration.
- Goals are scoped to a budget, so existing shared collaborators (view/edit)
  see and contribute to them.

**Non-Goals (YAGNI)**
- No dedicated goals UI (dashboard, forms, charts) — chat-only for now. REST
  routes exist for testing and future UI but are not consumed by the frontend.
- No stored milestone records — milestones (25/50/75/100%) are derived and used
  only for AI messaging.
- No reminders/notifications (that is Pillar 2 work).
- No automatic transfer of money or integration with real accounts.

## 2. Decisions (from brainstorming)

| Decision | Choice | Rationale |
|---|---|---|
| Progress tracking | **Hybrid** | Dedicated `goal_contributions` ledger plus an optional `linked_category_id`; progress sums both. |
| Goal types | **Savings & debt payoff** | Both named in Goal.md; identical math, distinguished by a `goal_type` label. |
| Scope | **Per-budget** | Inherits the app's budget-based multitenant sharing. |
| Frontend | **Chat-only** | Consistent with the chat-first app; no new components beyond one prompt-chip. |

## 3. Data Model (new migration `*_goals.sql`)

### `goals`
| column | type | constraints |
|---|---|---|
| `id` | UUID | PRIMARY KEY |
| `budget_id` | UUID | NOT NULL REFERENCES budgets(id) ON DELETE CASCADE |
| `name` | VARCHAR(255) | NOT NULL |
| `goal_type` | VARCHAR(20) | NOT NULL — `'savings'` \| `'debt'` |
| `target_amount` | DOUBLE PRECISION | NOT NULL |
| `target_date` | DATE | NULL |
| `linked_category_id` | UUID | NULL REFERENCES categories(id) ON DELETE SET NULL |
| `status` | VARCHAR(20) | NOT NULL DEFAULT `'active'` — `'active'` \| `'archived'` |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Constraint: `UNIQUE (budget_id, name)` (mirrors `categories`).
Index: `goals_budget_id_idx` on `budget_id`.

### `goal_contributions`
| column | type | constraints |
|---|---|---|
| `id` | UUID | PRIMARY KEY |
| `goal_id` | UUID | NOT NULL REFERENCES goals(id) ON DELETE CASCADE |
| `user_id` | UUID | NULL REFERENCES users(id) ON DELETE SET NULL |
| `amount` | DOUBLE PRECISION | NOT NULL |
| `note` | TEXT | NULL |
| `contributed_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Index: `goal_contributions_goal_id_idx` on `goal_id`.

`status` is stored only as `active`/`archived`. "Achieved" is **derived** at read
time (`current >= target`) to avoid write-time status synchronization.

## 4. Progress Computation (computed at read time, nothing cached)

```
contributions_sum = COALESCE(SUM(goal_contributions.amount), 0)
category_sum      = linked_category_id IS NOT NULL
                      ? COALESCE(SUM(transactions.amount WHERE category_id = linked_category_id), 0)
                      : 0
current_amount    = contributions_sum + category_sum
remaining         = MAX(target_amount - current_amount, 0)
percent           = target_amount > 0 ? MIN(current_amount / target_amount * 100, 100*) : 0
is_achieved       = current_amount >= target_amount
months_left       = target_date set & future ? ceil(days_until / 30), min 1 : null
monthly_needed    = (target_date set & future & !is_achieved) ? remaining / months_left : null
```
\* `percent` may be reported >100 in text if desired, but `is_achieved` is the gate.

Savings vs debt use the **same math**; `goal_type` only changes AI phrasing
("saved" vs "paid off").

**Known caveat (documented behavior):** when a goal has a `linked_category_id`,
both direct contributions and that category's transactions count toward
`current_amount`. Users should not double-log the same money. This is the
intended hybrid behavior, not a bug.

## 5. Backend Module `goals.rs` (mirrors `budget.rs` conventions)

DB structs in `db.rs`: `Goal`, `GoalContribution`. A `GoalWithProgress`
response struct (the `Goal` fields plus computed `current_amount`, `remaining`,
`percent`, `is_achieved`, `monthly_needed`).

Reuses existing `budget::check_permission` (view to read, edit to mutate) and
`budget::log_audit`.

### Routes (registered in `main.rs`, nested under a budget)
| Method | Path | Handler | Permission |
|---|---|---|---|
| POST | `/api/budgets/:id/goals` | `create_goal` | edit |
| GET | `/api/budgets/:id/goals` | `list_goals` (with progress) | view |
| GET | `/api/budgets/:id/goals/:goal_id` | `get_goal` (progress + contributions) | view |
| PUT | `/api/budgets/:id/goals/:goal_id` | `update_goal` | edit |
| DELETE | `/api/budgets/:id/goals/:goal_id` | `delete_goal` | edit |
| POST | `/api/budgets/:id/goals/:goal_id/contributions` | `create_contribution` | edit |
| GET | `/api/budgets/:id/goals/:goal_id/contributions` | `list_contributions` | view |

Reusable helpers (called by both REST handlers and `rag.rs` actions):
- `insert_goal(...) -> Goal`
- `insert_contribution(...) -> GoalContribution`
- `list_goals_with_progress(db, budget_id) -> Vec<GoalWithProgress>`
- `find_goal_by_name(db, budget_id, name) -> Option<Goal>`

Mutations write `audit_logs` (actions `CREATE_GOAL`, `UPDATE_GOAL`,
`DELETE_GOAL`, `ADD_GOAL_CONTRIBUTION`; AI variants prefixed `AI_`).

## 6. Conversational AI (`rag.rs`)

### Context injection
Add a `GOALS` block to the budget context the model receives, one line per
active goal: name, type, target, current, percent, remaining, and
`monthly_needed` when applicable. This lets the AI answer status questions
("how's my vacation fund?") with a `NONE` action.

### New actions (Gemini JSON prompt + offline keyword matcher + `match` arms)
- `CREATE_GOAL` — fields: `goal_name`, `goal_type` (`savings`|`debt`),
  `target_amount`, optional `target_date`, optional `linked_category`.
  Executor calls `insert_goal`, logs `AI_CREATE_GOAL`.
- `ADD_GOAL_CONTRIBUTION` — fields: `goal_name`, `amount`, optional `note`.
  Executor resolves the goal via `find_goal_by_name` (error if not found),
  calls `insert_contribution`, logs `AI_ADD_GOAL_CONTRIBUTION`, and returns a
  response noting the new progress percent.

The existing action struct gains the new optional fields; the Gemini system
prompt and the offline matcher both learn the two new actions. Offline matcher
patterns (modest, fallback-quality): "save … for", "pay off", "goal", and
"put/added $X toward/to <goal>".

### Motivation / milestones
The system prompt instructs the AI to be encouraging and to celebrate when a
goal crosses 25/50/75/100%, using the injected percentages. Offline mode emits
a simple "You're now at X% of '<goal>'." line on contribution. No milestone
persistence.

## 7. Frontend (`App.svelte`)
No new components. Add one example prompt-chip (e.g. "Save $3000 for a vacation
by December") so the feature is discoverable. This is the only frontend change.

## 8. Testing & Verification
1. `cargo check` clean.
2. Migration applies (tables `goals`, `goal_contributions` exist).
3. REST round-trip via curl: create goal → add two contributions → `GET goals`
   shows correct `current_amount`, `percent`, `is_achieved`; link a category and
   confirm category transactions add into `current_amount`.
4. Permission check: a view-only collaborator can `GET` but not `POST`.
5. Chat round-trip in offline mode: a `CREATE_GOAL` message creates a goal; an
   `ADD_GOAL_CONTRIBUTION` message records progress and the reply reports the
   percent.
6. Clean up any test rows from the dev DB afterward.

## 9. Files Touched
- **New:** `backend/migrations/<ts>_goals.sql`, `backend/src/goals.rs`,
  this spec.
- **Modified:** `backend/src/db.rs` (Goal, GoalContribution structs),
  `backend/src/main.rs` (module + routes), `backend/src/rag.rs` (context +
  actions), `backend/src/budget.rs` (only if `check_permission`/`log_audit`
  need visibility tweaks — they are already `pub`), `frontend/src/App.svelte`
  (one prompt-chip), `Goal.md` (mark Pillar 4 status).
