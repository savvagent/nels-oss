# Local-first Automerge spike (epic #566)

Throwaway, timeboxed code. **Not** a workspace member, not built by CI, not
deployed, and never imported by `backend/` or `frontend/`.

- Plan: [`docs/superpowers/plans/2026-09-24-local-first-automerge-spike.md`](../../docs/superpowers/plans/2026-09-24-local-first-automerge-spike.md)
- Findings: [`docs/superpowers/specs/2026-09-24-local-first-spike-findings.md`](../../docs/superpowers/specs/2026-09-24-local-first-spike-findings.md)

```bash
cd spikes/local-first-automerge && cargo test
```

## Harness

| Piece | What it does |
|---|---|
| `SimClock` | Per-device, settable clock. Commits are stamped with it; nothing calls `Utc::now()`. Devices can be skewed across a period boundary. |
| `Device` | A set of Automerge replicas keyed by doc id (`budget/<id>`, `txns/<id>/<YYYY-MM>`, `private/<user>`), with per-peer sync state and a deterministic actor id. `change(doc, msg, \|d\| …)` is the write path; `save` / `load_saved` model disk, `heads_with_prefix` feeds the summary cache. |
| `Relay` | Trusted plaintext transport for Week 1: every Automerge sync message crosses it as an `Envelope`; counts messages/bytes. It can't enforce roles (see `secure`). |
| `secure::SecureNet` | Week 2a. Blind relay (`SecureRelay`) holding an append-only log of signed, encrypted per-author change batches; owner-signed `Roster` (roles, actors, wrapped doc keys); per-device `SecureClient` that re-verifies everything on receive. `push` / `pull` / `sync` / `set_members` (removal rotates the key and compacts via snapshots). |
| `Harness` | Owns the devices and relay. `go_offline` / `go_online` / `sync_all` / `sync_in_order`. Only syncs a doc between devices that have both joined it. |
| `snapshot::render` / `conflicts` | Canonical text of the visible state for convergence asserts, plus the number of concurrent values behind a key. |

## Domain model

| Piece | What it does |
|---|---|
| `period` | Period math copied from `backend/src/budget.rs` (not imported). |
| `model` | The write path. Owns document layout: deterministic genesis per doc, flat transaction columns, tombstones, deterministic bank-row ids, batch import, provider revisions (`apply_bank_revision` + `moved` index), and the `TxnLayout` switch (per-month vs single doc, 2b). See its module doc for why each rule exists. |
| `read` | Derived reads: category folding, fund balance, current period, rollup resolution, after-close detection, delete/edit conflict flags. `ledger` (totals, no conflict flag) vs `transactions` (with it); `SummaryCache` (device-local per-doc spend, keyed by heads); `docs_for_period`. |
| `workload` | 2b: deterministic 5-year household generator (3 devices, bank batches, rules, edit churn, periodic sync). |

## Tests

| File | What |
|---|---|
| `tests/harness.rs` | Harness self-tests (Day 1). |
| `tests/automerge_semantics.rs` | Raw Automerge behaviors the model depends on. |
| `tests/week1_red.rs` | Week 1 🔴 scenarios (5, 8, 9, 10) and #12. |
| `tests/week1.rs` | Week 1 scenarios 1–4, 6, 7, 11, 13, 14. |
| `tests/week2a.rs` | Week 2a: why sync messages can't be role-checked, relay + receiver enforcement (scenario 13), attacks from a compromised relay, removal/rotation, demotion. `--release -- --ignored --nocapture` prints re-key cost on a 5-year household. |
| `tests/week2b.rs` | Week 2b: generator shape, both layouts derive identical numbers, current period needs only the budget + current month doc, fund balances from the summary cache, a week offline, and Plaid revisions that move a row across months. `cargo test --release --test week2b -- --ignored --nocapture` prints the per-month vs single-doc measurement table. |
