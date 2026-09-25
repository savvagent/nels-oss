# Local-First Household Sync with Automerge — Spike Plan

**Status:** Proposed (epic #566). Timebox **2 weeks**. Throwaway code, not merged into `backend/` or `frontend/`.

**Why:** Prospective users repeatedly raise two trust objections: they will not hand a new company their financial data, and they distrust AI. The candidate answer is a local-first Nels — data stored where the *user* chooses (device only, Nels Sync, self-hosted), end-to-end encrypted so any sync server holds only ciphertext, open source, bring-your-own-model. Household sharing is **essential**, so multi-user sync is the load-bearing technical risk. Automerge is the candidate engine. This spike exists to find out, cheaply and before any rewrite, whether Nels's domain survives CRDT merge semantics and whether E2E-encrypted multi-member sharing with roles is buildable.

**The spike answers questions; it does not build the product.** Every task below ends in an observation written to the findings doc, not in production code.

---

## Questions the spike must answer

1. **Invariants.** For each rule Postgres enforces today (uniqueness, CHECKs, `FOR UPDATE`, compare-and-set markers), does a CRDT-friendly data model make the conflict impossible, or can it be resolved acceptably at read time? Is any rule *unresolvable*?
2. **Derived-over-stored.** Can the two materialized, exactly-once jobs (fund advancement #228, auto-renew #51) be replaced with values derived on read from merge-safe facts?
3. **Permissions.** Can owner / edit / view roles be enforced when the sync server cannot read document contents? Can a member be removed so they cannot read *future* data?
4. **Document topology.** What document boundaries keep load time and size acceptable with years of bank-imported transactions, and keep private data (retirement, bank tokens) out of shared documents?
5. **Erasure.** Can "delete my account" / "delete this transaction" actually remove data, given CRDT history retention?
6. **Version skew.** Can two household members on different app versions share a budget without one clobbering the other's fields?

## Non-goals

- No UI, no Tauri shell, no mobile build.
- No bank-linking relay (it stays server-side regardless of storage choice; designed separately).
- No local LLM / BYO-key work.
- No SQLite port of the schema. The spike models data directly as Automerge documents.
- No user-chosen cloud-drive adapters (iCloud/Drive/Dropbox). Only an in-process relay and a small network relay.

---

## Setup (Day 1)

- New standalone crate at `spikes/local-first-automerge/` — **not** a workspace member, not referenced by CI or deploy workflows.
- Dependencies: `automerge` (Rust core, current stable), `ed25519-dalek`, `chacha20poly1305` (or `aes-gcm`, already familiar from `TOTP_ENC_KEY`), `serde`, `uuid`, `chrono`.
- A test harness that runs **N simulated devices in one process**, each with its own `Automerge` doc and `sync::State`, and a `Relay` that forwards sync messages. The harness must support: take a device offline, apply edits, bring it back, sync to convergence, then assert on the merged state. Every scenario in Week 1 is a test in this harness.
- A deterministic "clock" injected into every device so period boundaries (month rollover) are testable without waiting.

---

## Data model under test

Model one shared budget as documents, following the rule **store facts, derive numbers**:

```
budget/<budget_id>              (shared document)
  meta:        { name, time_frame, budget_type, rollover_enabled, closed_at, archived_at, currency, strategy }
  members:     { <member_pubkey>: { role, added_at } }
  categories:  { <category_id>: { name, category_type, category_limit, is_fund, rollover_enabled, linked_budget_id } }
  limit_history: { <category_id>: { <period_start>: limit } }     # replaces fund_balance + fund_advanced_through
  rollup:      { parent_id? }

txns/<budget_id>/<YYYY-MM>       (shared document, one per period)
  transactions: { <txn_id>: { amount, description, category_id?, date, provider_transaction_id?, currency } }

private/<user_id>               (per-person document, never shared)
  retirement profile, assets, bank provider tokens (encrypted), preferences
```

Key choices to validate:
- **Maps keyed by stable IDs everywhere, never lists**, so concurrent inserts don't reorder or duplicate.
- **Bank-imported transactions keyed by `provider_transaction_id`** (a deterministic `txn_id` derived from `(external_account_id, provider_transaction_id)`), so two devices importing the same bank row converge on one entry instead of duplicating.
- **Fund balance derived on read**: `balance = Σ over completed periods since enable (limit_history[p] − spent_in(p))`. No stored running balance, no advancement job, no marker. This would also fix the §12 known limitation (no historical per-period limit), because `limit_history` *is* that ledger.
- **Auto-renew derived on read**: the next renewal boundary is a pure function of `time_frame` and today (`budget::next_period_boundary` already is). No stored `next_renewal_at`.

---

## Week 1 — Merge semantics (the go/no-go core)

Each scenario: two (or three) devices go offline, make the listed edits, reconnect, sync. Record **what Automerge produced**, **what the derived read shows the user**, and **whether that is acceptable**. Scenarios marked 🔴 are ones where today's schema *rejects* one of the writes.

| # | Scenario | Today's guard | Hypothesis to test |
|---|---|---|---|
| 1 | Both devices import the same bank transaction | `(external_account_id, provider_transaction_id)` unique | Deterministic ID → converges to one entry |
| 2 | Both edit the **same** transaction's amount | last write wins (single server) | Automerge picks one deterministically; acceptable, but should the loser be surfaced? |
| 3 | A edits a transaction's amount, B edits its category | independent columns | Both edits survive (field-level merge) |
| 4 | A deletes a transaction, B edits it | row gone | **Observe** which wins; decide policy |
| 5 🔴 | Both create a category named "Groceries" | `unique_category_name_per_budget` | Two categories with distinct IDs; test read-time resolution: auto-merge by normalized name vs. show both + prompt |
| 6 | A deletes a category, B assigns a transaction to it | FK | Dangling `category_id` → read path treats as Uncategorized; verify no crash, no lost transaction |
| 7 | Both rename the same category | single write | One name wins; acceptable |
| 8 🔴 | A closes a project budget, B adds a transaction offline | `ensure_not_closed` (409) | Transaction lands after close. Policy options: accept + flag "added after close", or read path excludes. Pick one |
| 9 🔴 | A links X→Y as rollup, B links Y→X | `validate_rollup_link` + CHECK | Cycle exists after merge. Read path must detect and break deterministically (e.g., lowest ID wins); verify totals don't loop or double-count |
| 10 🔴 | Fund category: both devices cross a month boundary offline and "advance" | compare-and-set on `fund_advanced_through` | With derived balance there is nothing to advance → both devices compute the same balance. **Must pass** |
| 11 | A edits a fund's limit mid-period, B adds a late transaction for the previous period | none (known §12 gap) | Derived balance reflects the late transaction; limit change applies only to the current period's `limit_history` entry |
| 12 | Auto-renew budget crosses a boundary with both devices offline | due-predicate guarded UPDATE | Derived boundary → identical on both devices, no double renewal |
| 13 | Viewer (role = view) edits locally and syncs | `check_permission` | Covered in Week 2; here, just confirm the edit *would* merge if not rejected at the relay |
| 14 | Three devices, three-way concurrent edits on 1–9 | — | Convergence is order-independent: all three end byte-identical after sync in any order |

**Week 1 exit gate.** Every 🔴 scenario has an acceptable resolution, and #10 and #12 pass outright. If any 🔴 scenario has no resolution you would ship to a user, stop and write it up; that is a no-go or a redesign signal, and Week 2 is not worth running.

---

## Week 2 — Permissions, encryption, scale, erasure, versioning

### 2a. E2E-encrypted relay with enforced roles (Days 6–8)
- Each member has an ed25519 identity keypair. Each budget document has a symmetric **document key** shared with members (wrapped per member's public key).
- Every outbound change is packaged as an envelope: `{ signer_pubkey, doc_id, key_epoch, signature, ciphertext }`. The relay sees only the envelope header.
- The relay keeps a plaintext **membership list per doc**: `pubkey → role`, signed by the owner. It **rejects** envelopes from non-members and from `view` members. Test: scenario #13 is rejected at the relay and never reaches other devices.
- Also enforce on receive: devices verify signatures and roles themselves, so a compromised relay cannot inject writes.
- **Removal / revocation:** owner removes a member → new document key epoch → re-encrypt a fresh snapshot → distribute new key to remaining members only. Test: removed member receives nothing after the epoch change; measure the cost of the re-key on a large document.
- **Desk research (half day):** current status of Ink & Switch's Keyhive/Beelay and any production-ready group-key library. Record whether to adopt, wait, or build the simple scheme above.
- Record honestly in findings: a removed member keeps everything they already synced. That can't be undone and must be stated in-product.

### 2b. Scale and topology (Day 9)
- Generate a realistic household: 5 years × ~150 transactions/month (~9,000), 30 categories, 2 members, 3 devices, with realistic edit churn (recategorization, amount corrections).
- Measure for (a) one monolithic doc and (b) the per-period `txns` topology above: saved size, cold load time, time to merge a week of offline edits, sync bytes on the wire.
- Confirm reads of the current period touch only the current `txns` doc plus the budget doc, and that derived fund balances can be computed from per-period docs (or a small per-period spend summary) without loading all history.

### 2c. Erasure (Day 10, morning)
- Delete a transaction; confirm it is still recoverable from the doc's history (expected).
- Prototype **history compaction**: fork a fresh document from current state (no history), re-key, have all members switch to it, and have the relay drop the old doc's blobs. Measure the cost, and note what cannot be erased (copies already on other members' devices).
- Sketch the "delete my account" flow for a member of a shared household vs. the owner.

### 2d. Version skew (Day 10, afternoon)
- Device A runs "schema v1"; device B runs "v2" and adds a new field (e.g., a category `color`) and a new category property that v1 doesn't know.
- Confirm v1 reads without error, edits other fields without clobbering B's unknown fields, and that both converge.
- Write down the resulting schema-evolution rules (additive only; no renames; no type changes; how to deprecate).

---

## Deliverables

1. `spikes/local-first-automerge/` — the harness and every scenario as a runnable test.
2. `docs/superpowers/specs/<date>-local-first-spike-findings.md` containing:
   - The Week 1 table filled in: observed result, chosen resolution, acceptable yes/no.
   - A **per-constraint disposition list** covering every UNIQUE, CHECK, FK, `FOR UPDATE`, and advisory lock in `backend/migrations/` and the handlers: *made impossible by the model* / *resolved at read time (how)* / *enforced at relay* / *unresolved*.
   - Permissions/encryption design as built, with the Keyhive recommendation.
   - Scale measurements and the recommended document topology.
   - Erasure and version-skew rules.
   - A **go / go-with-changes / no-go** recommendation, with the estimated size of the real migration.

## Decision criteria

- **Go:** every 🔴 scenario has a shippable resolution; roles are enforceable without the relay reading plaintext; member removal works for future data; a 5-year household loads the current period in well under a second on desktop hardware.
- **Go with changes:** one or two invariants need product-level changes (e.g., closed budgets become "soft closed"), or revocation needs a library that isn't ready yet but has a credible interim scheme.
- **No-go:** an invariant with real money consequences (fund balances, rollup totals) cannot be made convergent, or role enforcement requires the server to read plaintext. In that case, fall back to the cheaper trust moves (open source + bring-your-own-key AI on the current server architecture) and revisit.

## Risks to the spike itself

- **Scope creep into building the product.** Stop at observations; no UI, no Tauri, no SQLite.
- **Testing a toy model.** Scenarios must use Nels's real rules (fund, rollover, rollup, close, archive), not a generic todo list, or the spike proves nothing.
- **Optimism about crypto.** Anything in 2a that is "we'd use library X later" is recorded as a risk, not a pass.
