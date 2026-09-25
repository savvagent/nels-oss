# Local-First Automerge Spike — Findings

**Epic:** #566 · **Plan:** [`../plans/2026-09-24-local-first-automerge-spike.md`](../plans/2026-09-24-local-first-automerge-spike.md) · **Code:** `spikes/local-first-automerge/`

**Status:** Complete. Day 1, Week 1 (**exit gate passed**), the per-constraint disposition, and Week 2a–2d are done. **Recommendation: go with changes** (see the end of this doc).

## Setup (Day 1)

- Standalone crate `spikes/local-first-automerge/`, its own `[workspace]` so it is not a member of anything. CI's change detection only matches `backend/`, `frontend/` and `marketing/`, so the crate is outside every gate.
- `automerge` 0.12.0 (Rust core). `ed25519-dalek` 2 and `chacha20poly1305` 0.10 are in place for Week 2.
- The harness is self-tested (`tests/harness.rs`, 6 tests): state reaches a newly joined device, offline edits on both sides converge after reconnect, concurrent writes to one key leave a detectable conflict with the same winner on both sides, three devices converge to the same state under two different sync orders, a `private/` doc never reaches a device that has not joined it, and each device's clock is independent and stamps its commits.

Observations so far:

- A concurrent write to one map key keeps **both** values: `get` returns a deterministic winner and `get_all` returns every one. So "surface the loser" (scenario 2) is something the read path *can* do; it isn't lost at merge time.
- Sync is pairwise with per-peer state. On reconnect the harness keeps only the durable part of each peer's state (shared heads) and drops in-flight bookkeeping, the way a real client recovers after a dropped link.

## Automerge behaviors that shaped the model

Pinned as tests in `tests/automerge_semantics.rs`, so an Automerge upgrade that changes any of them fails loudly.

1. **Two devices creating "the same" container create two.** If two offline devices each lazily `put_object(ROOT, "transactions", Map)`, the merge keeps both objects under one key and `get` shows only the winner, so **everything the other device wrote inside its object disappears from view**. This is the most dangerous behavior found, because nothing errors. It would hit every month rollover, since both devices create next month's transactions doc offline.
   - *Fix, verified:* every document starts from a **deterministic genesis change** (fixed actor, time 0, fixed message) that creates all of its containers. Identical changes hash identically, so the second copy is a no-op and both devices write into one shared object (`Device::join_with_genesis`).
   - *Rule:* **no object is ever created by more than one device.** Categories can be nested objects because only their creator makes them. Bank-imported transactions cannot, because two devices import the same row, so transactions are stored as flat **columns** (`date`, `amount`, `category`, … each a map `txn_id → scalar`), and per-(category, period) history uses flat `"<cat>/<period>"` keys.
2. **A map-key delete beats a concurrent edit inside that key's object**, and the edit is silently gone. So deletes are **tombstones** (a `deleted` column), not key deletes. (Deleting each column's key would be worse: a concurrent edit to one column would resurrect a half-deleted transaction.)
3. **Counters sum concurrent increments.** A stored fund balance that each device "advances" at a boundary is applied twice. Scenario 10 keeps this as a negative control.
4. **Same changes ≠ same bytes.** Replicas with identical change sets have identical heads and identical reads but **different `save()` output** (encoding follows local apply order; a load/save round trip doesn't normalize it). "Byte-identical convergence" is therefore asserted as equal heads plus equal canonical state. Content-addressed storage or dedupe must key on heads, not on file hashes.

## Data model as built

`src/model.rs` (write path) and `src/read.rs` (derived reads). Money is integer cents.

```
budget/<id>        genesis maps: meta, periods, members, categories,
                   limit_history {"<cat>/<period>": cents}, fund_history {"<cat>/<period>": bool},
                   close_seen {"<txns doc>": "<heads>[;late=<ids>]"} (2c), renewals {"<period>": device}, rollup {parent_id},
                   moved {"<month>/<doc>": true}                                  (2b)
txns/<id>/<YYYY-MM>  genesis columns: date, amount, description, provider_id,   (immutable facts)
                                      revision,                                (provider modification, 2b)
                                      amount_edit, category, note,             (user edits)
                                      deleted                                  (tombstone)
```

- **Bank facts are immutable and user edits live in their own columns.** Import writes `amount`; a correction writes `amount_edit`; the read shows `amount_edit ?? amount`. Two devices importing the same row write identical facts, so their conflict is harmless, and an import can never race a user edit.
- **`periods` index.** A device only syncs docs it has joined, so the budget doc lists every month doc that exists and members join them. Two devices creating the same month write the same key and value.
- **Period math is copied from `backend/src/budget.rs`** (`next_period_boundary` and friends), not imported.

## Week 1 — Merge semantics

All 14 scenarios are tests: `tests/week1_red.rs` (🔴 + #10, #12) and `tests/week1.rs`. Every test also asserts convergence: the same docs, the same heads and the same state on every device.

| # | Scenario | Observed | Resolution | Acceptable? |
|---|---|---|---|---|
| 1 | Same bank transaction imported twice | Deterministic id (UUIDv5 of account + provider txn id) → one key. Facts written twice with equal values: a 2-way conflict that nobody sees. A category set on A and an amount fix on B *before* sync both survive. | Deterministic id + immutable fact columns. | **Yes** |
| 2 | Same transaction amount edited twice | One deterministic winner; `get_all` keeps both. | Read exposes `amount_edit_conflict: [45_00, 55_00]`; the UI can show "edited on two devices", and any later edit resolves it. | **Yes** |
| 3 | Amount vs. category edit | Different columns → both survive. | None needed. | **Yes** |
| 4 | Delete vs. edit | With key-delete: delete wins, edit silently lost (behavior 2). With tombstone: both survive. | Tombstone; **delete wins** in totals, but the read flags `edited_concurrently_with_delete` (causal, from the change graph) so the UI can offer "restore". An edit the deleter had already seen isn't flagged. | **Yes**. The flag walks the whole change graph by brute force (forks per change); fine at spike scale, production needs an indexed version. |
| 5 🔴 | Duplicate category name | Two categories, distinct ids, no CRDT conflict. | **Fold at read time**: names equal after trim+lowercase fold into the lowest id; spend from both counts once under it; the other limit is kept for a "which limit?" prompt. Reversible with no merge record: renaming the duplicate un-folds it. | **Yes**. Normalization is stricter than today's exact-match UNIQUE. |
| 6 | Delete category vs. assign to it | `category_id` dangles. | Read resolves a dangling id to Uncategorized (same as today's `ON DELETE SET NULL`). The transaction and its money stay in totals. | **Yes** |
| 7 | Concurrent rename | One name wins, conflict retained. | None needed (optionally surface). | **Yes** |
| 8 🔴 | Close project budget vs. offline add | The offline device's adds/edits land after the close. | **Accept and flag.** `close_budget` records, for each transactions doc, the heads the closer had seen, so the read reports exactly which rows were added *or changed* without seeing the close. This uses causal history, not device clocks, so skew can't hide a row. A month doc the closer never held counts in full. Totals include them (dropping real spending would make the closed total wrong). Once a device sees the close it refuses writes locally, like today's 409. | **Yes, with a product change**: "closed" becomes "closed, with N late entries to review" rather than a hard wall. |
| 9 🔴 | Rollup cycle | X→Y and Y→X both merge (different docs). Also the case today's guard also covers: X→Y + Y→Z merge into a 2-level chain. | `resolve_rollup` rebuilds a valid **single-level** rollup under `validate_rollup_link`'s rules, taking links in child-id order; the rest are reported as ignored with the rule broken. Totals: every dollar counted once (asserted). | **Yes**. Which link wins is arbitrary but identical on every device; the UI must show "this link is inactive because …". |
| 10 🔴 | Fund advancement across boundary | **Derived:** both devices compute the same balance before and after sync, whatever their clocks inside the new period. **Stored-counter control:** double advance ($200 instead of $100). | `fund_balance = Σ completed periods where fund on (limit_in(p) − spent_in(p))` from `fund_history` + `limit_history` + transactions. No balance, marker or job. #228's freeze on disable and resume on re-enable fall out of `fund_history`. | **Yes, passes outright** |
| 11 | Fund limit edit vs. late prior-period txn | The limit edit writes the *current* period's `limit_history` entry; the late transaction lands in February's spend. | Balance for Feb = 400 − (300 + 100); March uses 600. This also closes the §12 known gap (no historical per-period limit), because `limit_history` is that ledger. | **Yes** |
| 12 | Auto-renew across boundary | Current period is a pure function of `time_frame` + clock → identical on both devices. Both record the renewal under the *period key* → **one** entry (a harmless 2-value conflict on who noticed first). Project and closed budgets never renew. | Derived boundary; exactly-once events keyed by a deterministic id. | **Yes, passes outright** |
| 13 | Viewer edit (merge only) | Merges like any other write. Roles are data in the doc and don't stop anything. | Enforced at the relay and on receive (2a): rejected with `NotAWriter`, never reaches another device. | **Yes** (2a) |
| 14 | Three-way concurrent edits | 3 devices, 16 concurrent edits spanning #1–#11, 2 budgets; synced in all **6** pair orderings. | Every ordering: all devices converge (heads + state) and the full derived view (categories, folds, transactions, rollup totals, fund balance) is identical across orderings. | **Yes** |

### Week 1 exit gate: passed

Every 🔴 scenario has a resolution a user would accept, and #10 and #12 pass outright. Two carry product changes: closed budgets become soft-closed with late entries to review (#8), and rollup conflicts are shown as inactive links (#9). Neither involves money disappearing or being double-counted.

The main design finding isn't a scenario result: **Automerge's container-creation behavior (behavior 1) would have silently lost data at every month rollover** in the naive model. The genesis-change and single-creator rules fix it, but they are rules every future schema change must follow. That argues for one small, reviewed write-path module that owns document layout, rather than letting features write to documents directly.

Pattern that generalizes: **anything the server did "exactly once" becomes either derived on read (fund balance, current period) or a write to a deterministic key (bank row id, renewal period)**, so repeating it is harmless.

## Per-constraint disposition

Source: every UNIQUE, CHECK, FK, lock, trigger and compare-and-set (CAS) guarded write in the final schema (all 51 migrations applied) and in non-test `backend/src`, restricted to household financial data. Server-operational tables (sessions, auth, billing, usage, audit, notifications) stay server-side and are out of scope. The inventory found **no member/invitation tables** (sharing is `budget_shares`, keyed by email), only **one advisory lock** (`retirement.rs:1514`), and several enums with **no DB CHECK at all** (`category_type`, `permission_level`, `goal_type`, `goals.status`, `time_frame`).

Dispositions:
- **Impossible:** the document model can't express the violation.
- **Read-time:** both writes merge and the read path resolves them deterministically.
- **Write-path:** validated locally before commit, and tolerated on read (a peer on another version, or a malicious one, can still send anything; see Week 2).
- **Relay:** enforced on envelopes (Week 2).
- **Server:** stays on the server, which still exists for bank linking.

| Constraint(s) | Today | Disposition | How |
|---|---|---|---|
| FKs `budget_id → budgets ON DELETE CASCADE` (categories, transactions, shares, goals, ignore rules, link sessions) | cascade | **Impossible** | These rows *are* the budget's docs. Deleting a budget means dropping `budget/<id>` + `txns/<id>/*` (see Erasure, 2c) |
| `unique_category_name_per_budget`, the chat upsert on `(budget_id, name)`, default-category seeding `ON CONFLICT DO NOTHING` | UNIQUE / upsert | **Read-time** | Fold by normalized name (scenario 5). Seeding uses deterministic ids (`uuidv5(budget, "default:"+name)`), so concurrent seeding converges on one key |
| `unique_goal_name_per_budget` | UNIQUE | **Read-time** | Same fold as categories (not built; identical mechanism) |
| `categories_one_mirror_per_source_idx` + mirror `ON CONFLICT DO NOTHING` | partial UNIQUE | **Impossible** | Mirror category id = `uuidv5(parent, "mirror:"+source)`: one key per source |
| `transactions.category_id ON DELETE SET NULL`, `goals.linked_category_id SET NULL` | FK | **Read-time** | Dangling id reads as Uncategorized / unlinked (scenario 6) |
| `transactions.matched_transaction_id SET NULL`, `external_account_id SET NULL` | FK | **Read-time** | Dangling reference = no match / manual-looking row; tombstones mean the target usually still exists anyway |
| `transactions_external_account_provider_tx_idx` + bank sync `ON CONFLICT DO NOTHING` (6 providers) | partial UNIQUE | **Impossible** | Deterministic txn id from `(account, provider txn id)` (scenario 1) |
| Plaid `modified` upsert (`plaid.rs:465`) overwrites amount/date/description | upsert | **Write-path + read-time** (2b) | The row stays in the doc of its *first-seen* date; a modification writes one encoded `revision` value carrying the server feed's sequence number. Reads use the highest sequence, a replayed older revision is refused at write time, and a `moved` index lets a reader of the new month find the row. See Scale and topology |
| `transactions_external_account_pairing_check` | CHECK | **Write-path** | Import sets both; manual sets neither |
| `budgets_rollup_not_self_check`, rollup CAS `WHERE rollup_parent_id IS NULL OR = $1` (steal guard), unlink CAS | CHECK / CAS | **Read-time** | `resolve_rollup` rebuilds a valid single-level rollup (scenario 9). The "steal a child" race becomes an LWW on `rollup.parent_id` plus read-time validation |
| `rollup_parent_id ON DELETE SET NULL` | FK | **Read-time** | Link to a missing parent is ignored ("parent budget not present") |
| `budgets_closed_only_project_check`, `budgets_auto_renew_only_time_based_check` | CHECK | **Write-path + read-time** | Read ignores `closed_at` on a time-based budget and `auto_renew` on a project one (`record_renewal` already checks type) |
| `closed_at / archived_at = COALESCE(...)` (first wins) | CAS | **Read-time** | Concurrent closes → LWW on one of two near-equal timestamps; after-close detection uses `close_seen`, not the timestamp (scenario 8) |
| `next_renewal_at` due-predicate CAS | CAS | **Impossible** | No marker; period derived, renewal log keyed by period (scenario 12) |
| Fund enable/disable CAS, `fund_balance += …` CAS with `fund_advanced_through` | CAS | **Impossible** | No stored balance or marker; `fund_history` + `limit_history` + spend (scenarios 10, 11) |
| `categories_is_fund_expense_only_check` | CHECK | **Write-path + read-time** | `fund_balance` returns `None` for non-expense categories |
| Enum CHECKs (`budget_type`, `amount_mode`, `strategy`, `source`, `review_status`, currency regex) | CHECK | **Write-path** | Unknown values from a newer version are preserved and shown as a fallback, not rejected (see Version skew, 2d) |
| Review/dismiss/merge-duplicate CAS (`review_status`, `excluded_from_budget`) | idempotent CAS | **Impossible** | Idempotent flag writes; concurrent equal writes are harmless |
| `unique_default_budget_per_user` + switch-default CAS; `users.active_budget_id` fill-if-empty CAS | partial UNIQUE / CAS | **Impossible** | A single `active_budget` key in the user's `private/` doc: one value per key by construction |
| `budget_shares` (unique per email, permission upsert, no permission CHECK) | UNIQUE / upsert | **Relay** | Owner-signed roster `{pubkey: role}` outside the CRDT, one entry per device key; roles enforced at the relay and on receive (2a) |
| `linked_accounts_provider_account_id_key` (global), `linked_accounts` upserts and status CASes, `basiq_users`, all `*_link_sessions` single-use claims | UNIQUE / CAS | **Server** | Bank linking stays server-side (non-goal). Global uniqueness across users can't be local-first by definition |
| `securities` UNIQUE + metadata upsert | UNIQUE | **Server** | Shared reference data, not household data |
| `transactions` embedding backfill `FOR UPDATE SKIP LOCKED` | lock | **Gone** | A server worker queue; local-first embeddings (if any) are per-device and never shared |
| `trg_transactions_updated_at` | trigger | **Impossible** | Change history *is* the edit log; "last user edit" = latest change touching the user-edit columns |
| retirement `pg_advisory_xact_lock` + `FOR UPDATE` read-merge-write + member upsert | lock / upsert | **Impossible** | Retirement lives in the per-user `private/` doc; field-level merge replaces read-merge-write, and a single-creator genesis replaces the create race |
| `retirement_profiles` value CHECKs (SS ages 62–70, anchor pairings, finite, ≥ 0) | CHECK | **Write-path** | Pairing CHECKs (months ↔ anchor, benefit ↔ source) can be broken by a concurrent partial edit; store each pair as **one** value (a small map written atomically, or an encoded scalar) so it can't tear |
| `assets` composite same-user FKs, `asset_holdings` UNIQUE (asset, security), `asset_balance_history` UNIQUE (asset, day) + upsert | FK / UNIQUE | **Impossible** | Per-user `private/` doc (same user by construction); holdings keyed by security id and snapshots keyed by day, so an upsert becomes a put to a deterministic key |

**Unresolved: none.** No constraint with money consequences lacks a disposition. Two items needed Week 2 tests: the Plaid date-modification case (**done in 2b**), and the "tear" risk for paired fields (a general rule: **fields that must change together are stored as one value**; 2b's `revision` is one instance, and 2d tests the retirement shape: `paired_fields_tear_unless_stored_as_one_value`).

**Side finding (backend, outside the spike), #569:** `transactions.external_account_id ... ON DELETE SET NULL` combined with `transactions_external_account_pairing_check` makes a hard `DELETE FROM linked_accounts` fail for any account with imported transactions (reproduced locally). This is latent: every such delete in `backend/src` is test cleanup today, and deleting a user or budget still works because the cascade removes the transactions too.

## Permissions and encryption (2a)

Built in `src/secure.rs`, tested in `tests/week2a.rs` (13 tests plus 1 measurement). Every test syncs **only** through the secure relay, so nothing reaches a device unless the relay accepted it and the device verified it itself. Scenario 13 now passes: a viewer's edit is rejected at the relay and never reaches anyone.

### Main finding: Automerge's sync protocol can't go through an enforcing blind relay

The plan assumed Week 1's transport could be wrapped in signed envelopes. It can't, for two reasons, both pinned as tests:

1. **A viewer must send sync messages to read.** The protocol opens with the reader's heads and bloom filter. A relay that drops everything a viewer sends also stops the viewer from receiving (`a_viewer_must_send_sync_messages_just_to_read`).
2. **A sync message carries changes its sender didn't write.** B's message to C includes A's changes. A signature on the message says nothing about who wrote each change inside it (`a_sync_message_carries_changes_its_sender_did_not_author`).

So the relay is an **append-only log of sealed per-author change batches**, not a pipe for sync messages. Every envelope is a write, so "reject writes from viewers" is exact. The cost: we give up Automerge's bloom-filter sync between devices. A device catches up by log cursor (`seq >= since`) and publishes its own changes since its last publish. This is the same shape as Ink & Switch's Beelay/Subduction, which also replaced the sync protocol for this reason.

### Design as built

- **Identity:** each **device** has an ed25519 signing key and an X25519 key for receiving wrapped keys. Keys are per device, not per member, because two devices of one member must be distinct Automerge actors. *Production needs a member key that certifies device keys*; not built.
- **Roster (outside the CRDT):** one per sharing group (a budget and all of its month docs). It holds `version`, `epoch`, `owner`, and `pubkey → {role, Automerge actor, X25519 key, doc key wrapped for this member}`, signed by the owner. Clients pin the owner key at invite time (out of band, e.g. a QR code). **Week 1's `members` map inside the budget doc can't carry authority:** any writer, or a viewer with a patched client, can write to it. It becomes display data at most.
- **Envelope:** `{group, doc_id, signer, roster_version, epoch, kind, nonce, ciphertext, signature}`. XChaCha20-Poly1305 under the epoch's doc key, with the header as AAD. The ed25519 signature covers header + nonce + ciphertext, so the relay can verify it without the key. Doc keys are wrapped with X25519 + HKDF-SHA256 + XChaCha20-Poly1305, bound to (group, epoch).
- **Relay checks** (on headers only): the group exists, the signer is on the current roster, the signer is a writer (the owner, for snapshots), `roster_version` is the current one (freshness), the epoch matches, and the signature is valid. Fetch requires a signed request from a current member.
- **Receiver checks** (all of the relay's, repeated against the roster version the envelope claims, which the receiver has verified back to the pinned owner): the doc id belongs to the group, AEAD decrypts, and **every change in the batch was authored by the actor bound to the signer's key**. That last check is what stops one writer from forging changes as another.

| Attack | Stopped by | Test |
|---|---|---|
| Viewer publishes an edit (scenario 13) | relay `NotAWriter` | `s13_…_rejected_at_the_relay…` |
| Viewer's edit injected by a compromised relay. The viewer holds the doc key, so the envelope is perfectly valid except for the role | receiver `NotAWriter` | `receivers_refuse_a_viewer_write…` |
| Writer B signs changes authored as A's actor | receiver `ActorMismatch` | `receivers_refuse_a_writer_forging…` |
| Tampered ciphertext; envelope re-pointed at another doc | signature | `receivers_refuse_tampered_and_misdirected…` |
| Writer in budget X writes budget Y's doc | `DocNotInGroup` (relay + receiver) | same |
| Non-member reads or writes | relay `NotAMember` | `a_non_member_can_neither_write_nor_read` |
| Editor forges a roster promoting a viewer | relay + receiver `NotOwner` | `only_the_pinned_owner_can_change_the_roster` |
| Removed member reads new data via a leaky relay | no key for the new epoch (`BadCiphertext`) | `a_removed_member_gets_nothing…` |

Mutation-checked: disabling the receiver role check, the actor binding, or the relay freshness check each fails at least one test.

### Membership changes

- **Removal** starts a new key epoch. The owner catches up, publishes a roster wrapping a fresh key for the remaining members, then publishes an owner-signed **snapshot** (`save()`) of every doc under the new key. A snapshot supersedes that doc's earlier envelopes, so the relay drops the old-epoch blobs (compaction). The removed device gets `NotAMember` from an honest relay and can't decrypt anything a dishonest one leaks.
- **An offline editor during a rotation** publishes, gets `StaleRoster`, fetches the new roster, unwraps the new key, reseals and succeeds. Its offline edit arrives and everyone converges.
- **Demotion** (edit → view) and **adding a member** keep the epoch; there's nothing to hide from them. Envelopes are verified against the roster version they were sealed under, so a device joining later still accepts the demoted member's earlier writes.

**Re-key cost** (release build, 5-year household: 9,000 transactions across 62 docs, `rotation_cost_on_a_five_year_household`): owner side (save + encrypt + sign + publish every doc) **11 ms**, snapshots **70 KiB**. Remaining member applies the snapshots in **67 ms**. The relay then stores 82 KiB. Wrapping the key for 10 members takes **0.8 ms**, so rotation cost is dominated by snapshots, not by member count. Cheap enough to rotate on every removal.

### What this does NOT give you (recorded, not passed)

1. **A removed member keeps everything they already synced.** That can't be undone and must be stated in-product ("removing someone stops future sharing; they keep what they had").
2. **The relay is trusted for freshness.** A removed or demoted member can seal an envelope under an old roster version where they still had rights. An honest relay refuses it (`StaleRoster`), but a receiver can't tell a back-dated write from a genuinely concurrent one without a trusted sequencer or causal membership ops. Keyhive handles this with a causal capability graph; our simple scheme does not. Impact: a malicious relay colluding with an ex-member could inject that member's writes. It still can't read new data or forge anyone else's writes.
3. **A viewer's rejected edit stays on the viewer's device**, forking its copy of that doc (asserted in the s13 test). The client must refuse writes before commit when the local role is `view` (read-only UI), or roll the doc back.
4. **The relay knows the social graph and traffic shape**: group ids, doc ids (which include the month), signer keys, roles, sizes and timing. Doc ids could be made opaque (a keyed hash); roles can't be hidden from a relay that enforces them.
5. **Crypto is hand-assembled from audited primitives** (dalek, RustCrypto) but the protocol itself is unreviewed. Per the plan's risk rule, that's a **risk, not a pass**: shipping it needs an external review.

### Desk research: Keyhive / Beelay (September 2026)

- **Keyhive** (Ink & Switch; Rust `keyhive_core`, `beekem` concurrent TreeKEM, Wasm/TS bindings) is actively developed but explicitly **pre-alpha**: "DO NOT use this release in production… Expect bugs, inconsistencies, and unstable APIs"; **not audited**. Its roles (relay, read, edit, admin) are exactly Nels's owner/edit/view plus a blind relay, and it solves gap 2 above with causal delegation and revocation.
- **Beelay** has been succeeded by **Subduction** (auth-enabled sync over E2E-encrypted data), which confirms the main finding: encrypted sync needs its own protocol.
- **ARK** (`@automerge/automerge-repo-keyhive`) integrates Keyhive with automerge-repo; it is **alpha**, JS/TS only, with an API guide as of Aug 2026.
- `@localfirst/auth` (+ `auth-provider-automerge-repo`) is an older JS alternative: team membership and E2EE without a server.

**Recommendation: build the simple scheme above as the interim, and plan to adopt Keyhive once it's audited and stable.** Don't adopt now (pre-alpha, unaudited, APIs moving), and don't build our own causal-capability system (that's Keyhive's whole research program). The interim scheme is good enough for a household product if the relay is operated by Nels: it gives confidentiality and write-integrity against the relay, and the one thing it trusts the relay for (freshness) matches today's server model. Keep the envelope and roster formats versioned so migrating to Keyhive is a protocol swap, not a data migration: the Automerge docs themselves don't change. Against the decision criteria, this is **go-with-changes**: roles are enforced without the relay reading plaintext, and removal works for future data, but revocation-proper waits on a library that isn't ready, with a credible interim scheme.

Sources: [Keyhive README](https://github.com/inkandswitch/keyhive/blob/main/README.md), [Keyhive notebook](https://www.inkandswitch.com/keyhive/notebook/), [ARK API guide](https://automerge.org/docs/keyhive/ark-api-guide/), [This Month in Automerge: Aug '26](https://automerge.org/blog/2026-august/), [@localfirst/auth-provider-automerge-repo](https://www.npmjs.com/package/@localfirst/auth-provider-automerge-repo).

## Scale and topology (2b)

Built in `src/workload.rs` (a deterministic household generator) and `tests/week2b.rs` (8 tests plus 1 measurement: `cargo test --release --test week2b -- --ignored --nocapture`).

**The household.** 5 years, 3 devices (Alice's laptop and phone, Bob's phone), 30 categories, 3 funds. Each member's bank account is imported by one of their devices once a day as one batch. About 75% of bank rows are categorized by a rule in a second change, and the rest by a person a few days later. On top of that: recategorizations (one in five of them 45 days to a year later, which touches old month docs), amount corrections, deletes, notes, manual entries from the phones, and two limit changes a month. Devices sync every 3 days. That produced **8,974 transactions** (7,763 bank + 1,211 manual) and about 12,000 Automerge changes. The generator never has two devices write the same key concurrently, so both layouts perform identical operations and resolve identically (`both_layouts_derive_identical_numbers`). Conflicts were Week 1's subject.

**Measured** (release build, AMD Ryzen 7 5825U laptop, one run):

| Measure | Per-month docs | Single doc |
|---|---|---|
| Docs / Automerge changes | 61 / 12,131 | 2 / 11,795 |
| Saved size, all history | **452 KiB** (52 B/txn) | 967 KiB (110 B/txn) |
| Budget doc | 4.4 KiB | 2.9 KiB |
| Largest transactions doc | 8.4 KiB | 964 KiB |
| Cold start, load **all** docs + current screen + fund balances | 178 ms (load 101 ms) | 223 ms (load 126 ms) |
| Build the summary cache (one-off) | 19 ms for 60 docs | n/a |
| **Cold start, budget doc + current month + cached summaries** | **5.6 ms** (load 2.8 ms) | n/a |
| Add one transaction + commit, at year 5 | 67 µs | 74 µs |
| Sync one edit to 2 peers, at year 5 | 7.9 ms, 0.9 KiB | 6.3 ms, 0.9 KiB |
| A week offline, merge to quiescence | 24 ms; 40 KiB in 391 msgs | 44 ms; 13 KiB in 19 msgs |
| A week offline, raw changes to / from Bob (secure-log payload) | 6.6 / 1.8 KiB | 6.1 / 1.6 KiB |
| Reconnect with nothing new | 28.8 KiB in 372 msgs | 1.6 KiB in 12 msgs |
| New device, full initial sync | 290 ms; 547 KiB | 414 ms; 1,029 KiB |
| Generating the whole history (3 devices, ~600 syncs) | 6.8 s | 22.1 s |

What this shows:

- **Size is a non-issue.** Five years of a busy household is under half a megabyte on disk with full history. Per-month docs came out at *half* the size of one big doc. We didn't dig into why (the generator is seeded, so this is one history, not a distribution).
- **Per-month docs make the current period O(1 month).** A cold start that loads only the budget doc and this month's doc renders the current screen in under 6 ms, whatever the history length. Loading everything is still well under a second (178 ms), so even the naive path meets the decision criterion. But it grows linearly, and the single doc has to load all of it every time.
- **Fund balances need history, but only as a summary.** A fund's balance sums every completed period, so from scratch it touches all 60 docs. Instead, each device keeps a **local, never-synced cache** of per-doc spend (`read::SummaryCache`): per `(period, raw category id)`, keyed by the doc's heads. Equal heads give an equal summary, so an entry can be out of date but never wrong, and it is rebuilt only for docs whose heads changed. Tests: a device holding only the budget doc plus the cache gets the same balances as a from-scratch computation; a late recategorization in an old month re-summarizes exactly **one** doc; renaming a category re-summarizes **none**, because summaries keep raw ids and folding happens at read. A real client stores each doc's heads next to it on disk, so checking freshness doesn't need the doc loaded. The same cache serves any multi-period number (reports, trends).
- **Totals must not use the per-row conflict flag.** Scenario 4's `edited_concurrently_with_delete` walks the change graph and forks the doc once per change, for every tombstone: quadratic in history. It was split out before the first measurement, so its cost at this scale wasn't measured. It is now computed only for rows shown to a user (`read::transactions`). Totals use `read::ledger`, which reads each column in one pass instead of doing a `get` per row per column; that cut the one-off summary build from 230 ms to 19 ms. Production needs an indexed version of the flag, as scenario 4 already noted.
- **Writes don't care about history size**: about 70 µs either way.
- **The single doc's costs are in whole-doc operations.** Sync after a multi-day batch, save and load all scale with the doc, which is why generating the history took 3.3x longer. Its one advantage, a cheaper no-op reconnect (1.6 KiB vs 28.8 KiB), comes from the Automerge sync protocol doing a handshake per doc: 61 docs × 3 pairs. **2a's secure relay replaces that protocol** with a single log-cursor fetch per group, so the advantage disappears on the transport we would actually ship. The payload that transport carries for a week offline is the raw change set: 6.6 KiB to Bob and 1.8 KiB from him.
- **A new device** needs about 0.5 MB over the sync protocol. Through the 2a relay it would fetch the owner's latest snapshots instead (≈ saved size, 452 KiB), then the log tail.

**Recommended topology: per-month transactions docs** (`txns/<budget>/<YYYY-MM>`) plus one budget doc, a device-local summary cache for multi-period numbers, and no single-doc option. The layout is a budget-level setting (`meta.txn_layout`) only so the spike could compare them.

### Provider modifications that move a row across months (disposition follow-up)

A Plaid `modified` can change a row's date, amount and description; today `plaid.rs` overwrites them in place. With per-month docs, a date change can move the row into another month. Moving the row between docs would put one id in two docs (double counting) and strand concurrent user edits in the old doc. Built and tested instead (`tests/week2b.rs`, `plaid_modified::*`):

- **A row stays in the doc of its first-seen date forever.** The imported facts stay immutable. A modification writes one **encoded `revision` value** (`seq|date|amount|description`) in that doc, so a date and an amount from two different modifications can't tear apart. `seq` is the record's position in the **server's per-account feed**, which is ordered and the same for every device, because bank linking stays on the server. The server also supplies `first_seen`, so every device picks the same doc.
- **Reads take the highest `seq`**, not Automerge's LWW winner, among concurrent revisions. Tested with both actor assignments.
- **Write-if-newer.** A device that has already seen seq 3 refuses to apply a replayed seq 2. A causal overwrite would leave only the stale value, which no read rule could undo. Replaying the original import is harmless: it rewrites identical facts.
- **A user's `amount_edit` still beats the provider's revised amount.**
- **`moved` index.** When a revision lands in another month, the budget doc records `"<new month>/<first-seen doc>"`. `read::docs_for_period` returns the month's own doc plus those it names. Tested: reading February from February's doc alone misses the row; with the index the total is right. Two devices applying the same revision write the same key. A stale entry after a later revision moves the row back only costs one extra doc load.

Mutation-checked: removing write-if-newer, reading the LWW winner instead of the highest `seq`, or skipping the `moved` write each fails exactly one of the three tests.

**Input for 2d:** adding the `revision` column changed the transactions genesis change, which is only safe because the spike has no stored data. In production a doc type's genesis is **frozen forever**: two devices with different genesis versions would create a container twice (behavior 1). New containers need another mechanism; 2d has to settle which.

## Erasure (2c)

Built in `src/erasure.rs` plus compaction support in `src/secure.rs`, tested in `tests/week2c.rs` (10 tests plus 1 measurement: `cargo test --release --test week2c -- --ignored --nocapture`). Like 2a, everything after setup syncs only through the secure relay, because erasure has to reach the relay's copy too.

### The problem, pinned

A deleted transaction survives **twice**: in the current state, since a tombstone sits beside the row's facts (layout rule 4), and in every earlier state, recoverable with `fork_at` (`a_deleted_transaction_is_still_in_the_doc_twice_over`). **2a's rotation snapshot doesn't help.** It compacts the relay's log, but the snapshot is a `save()` with full history, so a member added after a rotation receives the deleted row with it (`a_rotation_snapshot_is_not_erasure`). Deleting and erasing are two different operations, and only the first one exists so far.

### Design as built: compaction

- **A compacted doc is the doc's genesis plus one owner change that writes its current state.** Deleted rows are reduced to their `deleted` marker. Everything else keeps its winning value, and `revision` keeps the value reads use (highest `seq`), not the LWW winner.
- **The tombstone marker stays forever.** Without it, a device that still had the row, or a bank re-import, would bring it back as live. Import now **skips rows this device holds a tombstone for**, because otherwise a server feed replay writes the erased description straight back (`a_bank_reimport_does_not_write_an_erased_row_back`, which was red before the fix). A device that hasn't seen the delete yet still imports; the next compaction strips those facts again.
- **Conflicts are settled by compaction.** Week 1's conflict flags (`amount_edit_conflict`, `edited_concurrently_with_delete`) read the losing values from history, so they don't survive it. The winner the UI showed becomes the only value.
- **Anything that names change hashes must be rewritten.** `close_seen` stores the closer's heads, and compaction removes every hash. The owner settles `changed_after_close` first, while the hashes still resolve, and writes the answer down: `"<new heads>;late=<id>,…"` (`a_closed_budget_keeps_its_after_close_answer_through_compaction`). Device-local caches keyed by heads (`read::SummaryCache`) need nothing: new heads just mean a rebuild. **Rule for every future field:** anything that stores a change hash needs a compaction rewrite.
- **Protocol.** The owner catches up, builds every doc, and calls `SecureRelay::compact` with a new roster version. The roster gains a signed `compacted: doc → version` map, carried forward by every later roster. In one step the relay adopts the roster, drops every stored envelope for those docs, and stores the compacted ones. The owner passes the log cursor it fetched up to, and the relay refuses (`CompactionRaced`) if anything arrived for those docs since. Without that check it would delete a member's accepted write that the compaction doesn't include (`a_compaction_that_races_a_publish_is_refused_then_retried`). Receivers refuse anything for a compacted doc that was sealed under an earlier roster version (`PreCompaction`). An honest relay has dropped those envelopes, but a compromised relay or a restored backup could replay them (`pre_compaction_envelopes_are_refused_even_from_a_compromised_relay`). A compacted doc must be exactly genesis plus one change by the owner's actor, or it is refused as `Malformed`.
- **Members replace their copy and rebase their own unpublished edits as values.** Automerge changes can't be re-applied: each depends on its history by hash. So a device turns its own unpublished changes into state patches (`diff` from the heads of everything else to its current heads), swaps in the compacted doc, and writes the patches as one new change. Objects are resolved by key path, because compaction re-creates every non-genesis object (categories) under a new id. An editor who was offline during compaction publishes, gets `StaleRoster`, fetches, rebases, and republishes. Everyone converges, and the editor's original changes are in no one's history: the doc holds genesis, the compaction and one rebase change (`an_offline_edit_is_rebased_onto_the_compaction_not_lost`). `push` now recomputes its batch after a stale-roster refresh, since the refresh can replace the doc underneath it.
- **Erasure beats a concurrent edit.** A rebased write to a row that the compacted doc has erased is dropped. Week 1's scenario 4 keeps an edit that raced a delete and flags it. After compaction it can't: re-applying it would write the note back into a doc that was supposed to have forgotten the row (`a_rebased_edit_to_an_erased_row_is_dropped`). A rebased provider `revision` also keeps write-if-newer (implemented, not separately tested).
- **No re-key.** Compaction hides nothing from current members, because they had it all, so it keeps the key epoch and only bumps the roster version. Removing a member is what needs a new epoch (2a).

| Test | Shows |
|---|---|
| `compaction_erases_a_deleted_row_from_every_member_and_the_relay` | The description and a note on the deleted row are gone from every device's history (each doc: genesis + 1 change). The numbers users see are unchanged. The relay holds one envelope per doc. Writing still works afterwards. |
| `a_member_who_joins_after_compaction_gets_no_history` | Unlike a rotation snapshot. |
| `an_offline_edit_is_rebased_onto_the_compaction_not_lost` | Offline note, recategorization and new row all land; nothing is dropped. |
| `a_rebased_edit_to_an_erased_row_is_dropped` | Erasure wins; the note never reaches anyone. |
| `pre_compaction_envelopes_are_refused_even_from_a_compromised_relay` | A replayed old log is refused on receive. |
| `a_compaction_that_races_a_publish_is_refused_then_retried` | Relay unchanged on refusal; the retry includes the write. |
| `a_closed_budget_keeps_its_after_close_answer_through_compaction` | `close_seen` rewrite. |
| `a_bank_reimport_does_not_write_an_erased_row_back` | Import skips tombstoned rows. |

Mutation-checked: removing the erased-row check in rebase, the tombstone stripping, the receiver's `PreCompaction` check, the relay's race check, the `late=` read, the import skip, or the own-changes-only filter in rebase each fails at least one test.

**Cost** (release build, the 2b household: 8,974 transactions, 78 deleted, 61 docs, synced through the secure relay):

| Measure | Value |
|---|---|
| Owner compacts **all** 61 docs (build + save + seal) | 518 ms |
| Owner compacts **one** month doc (the largest) | 9 ms |
| Saved size, all docs | 452 KiB → 329 KiB |
| Relay storage for the group | 2,985 KiB → 342 KiB |
| A member replaces all 61 docs | 61 ms |

- The time is Automerge writing about 54,000 ops into fresh docs, about 8 ms per month doc. **Per-month docs make erasing one transaction a one-doc job.** The protocol already works per doc, but the spike's `compact` does the whole group. Compacting one closed month doc would rewrite `close_seen` with an ordinary budget-doc change instead of a budget-doc compaction; not built.
- **The relay needs periodic compaction for storage anyway.** A per-author change log is 6.6× the saved size after five years (2,985 KiB), and compaction brings it back to roughly the saved size.

### What compaction cannot erase

1. **Copies other people already have.** Compaction is honored by honest clients. A device that never reconnects, a patched client, a backup, an export or a screenshot keeps what it had. That matches today's server model (a browser that rendered the row, DB backups), but it has to be stated in-product: *"Deleted items are removed from Nels and from your household's devices when they next sync. Anyone who already saw them may have kept a copy."*
2. **Old ciphertext outside the relay's live log.** A relay backup taken before compaction still holds the old envelopes, and every member holds the key for that epoch. Receivers refuse replays, but confidentiality of those bytes rests on the relay deleting its backups on a schedule. Relay backup retention has to be part of the erasure policy.
3. **Server-side bank data.** Bank linking stays on the server, which keeps its own copy of the provider feed. Erasing an imported row needs a server-side delete of that feed record too, or a replay could deliver it to a device that hasn't seen the tombstone.
4. **Conflict information** (above). By design.

Also: compaction removes the other members' **actor ids** from the docs, since the owner writes the whole state. Old roster versions still list their keys; once every stored envelope is sealed under a version at or after the compaction, the relay can prune older rosters. Not built.

### Recommended: rotation should publish compacted docs

A member removal is the moment erasure matters most. Today's `set_members` snapshot ships full history to whoever joins next. It should publish compacted docs instead, so one mechanism serves both purposes: `Snapshot` becomes `Compacted`, and the offline-editor path already rebases. Cost: one whole-group compaction per removal, about 0.5 s on five years. Not built, because the 2a tests pin the current snapshot behavior.

### "Delete my account" (sketch, not built)

- **Solo user:** wipe local replicas and keys (fully in the client's control). The relay deletes the group log and rosters. The server deletes what it still holds: the account row, sessions, subscription, linked accounts and the bank feed (today's `delete_user_data`).
- **Member of someone else's household:** the owner removes them (2a rotation, which per the recommendation above also compacts). Their device wipes its replicas and keys. **Their household entries stay, as household records**: the same asymmetry the server has today, where budget-scoped rows a user wrote in someone else's budget survive their deletion (AGENTS §10). If they want specific entries gone, they delete those first and the owner compacts. After compaction nothing in the docs is attributed to their actor.
- **Owner of a shared household:** members pin the owner's key, so ownership can't just move to another member. Either the group is **dissolved** (the relay deletes it; members keep their local copies and can start a new group they own from them), or ownership transfer is a separate feature where members re-pin a new owner key out of band. Recommend dissolve for v1, and make the account-deletion screen say members keep their copies.

### Erasure rules

1. **Delete and erase are different operations.** Delete is a tombstone: instant, syncs everywhere, and reversible in principle. Erase is compaction: owner-only, on request (account deletion, "permanently delete") and periodic (for relay storage).
2. **Tombstone markers are forever;** everything else about an erased row goes.
3. **Import skips tombstoned rows.**
4. **No field may store a change hash without a compaction rewrite.** Local caches keyed by heads are exempt; they rebuild.
5. **Unpublished edits rebase as values, and writes to erased rows are dropped.**
6. **Relay compaction is atomic and cursor-checked;** receivers refuse pre-compaction envelopes.
7. **Erasure is honest-client erasure,** with the boundary stated in-product.

**Verdict for 2c: go.** Erasure works for every device that runs our client and reconnects, including the relay. The boundary (copies already made) is the same as today's server model. What is new is the need for product copy, and for relay backup retention to be part of the erasure policy.

## Version skew (2d)

Built in `src/schema.rs`, with upgrade handling in `src/secure.rs` and `src/erasure.rs`; tested in `tests/week2d.rs` (10 tests). Device `a` runs v1, meaning this crate's `model`/`read` and nothing else. `b` and `c` run v2, which adds a category `color` (a new field in an existing object), a `tags` column on transactions (a new **container**), and a feature that changes what existing numbers mean. Everything syncs through the secure relay.

### Safe with no machinery: new fields and new keys

A v2 field inside an existing object is stored by Automerge and never read by v1. v1's field-level writes leave it alone: v1 renames the category and changes its limit, and b's `color` survives (`a_v1_client_preserves_a_v2_field_it_does_not_know`). New flat keys in an existing map and new values in an existing column behave the same way. **Unknown enum values** are the same case. v1 reads a budget whose `strategy` is `fifty_thirty_twenty` without error. The UI's job is to fall back (show it as the default strategy, or "set in a newer version") and **never write the unknown value back as something else**.

The one way to break this is to **replace an object instead of writing its fields**. A v1 "save the category form" that re-creates the category map with the fields it knows deletes b's `color` (`rewriting_a_whole_object_clobbers_fields_the_writer_does_not_know`). That is behavior 2 again, applied across versions.

### New containers: frozen genesis, schema upgrade changes

2b flagged that a doc type's genesis can never change. 2d found it is worse than hidden writes: **two devices with different genesis versions can never sync that doc again.** Both genesis changes are by the genesis actor at seq 1 with different content, and Automerge refuses the merge with `DuplicateSeqNumber`, permanently (`two_genesis_versions_can_never_sync_again`).

So a new container comes from a **schema upgrade change** (`schema::canonical_upgrade`). It depends on genesis only, has time 0, creates its containers in sorted order, and is authored by an actor **named after its own content**: `nels-schema-v2-<first 8 bytes of sha256(doc type, version, containers)>`. Everything about it follows from its content, so devices that upgrade independently build the identical change and end up with one container (`independent_upgrades_create_one_container_and_v1_keeps_up`).

Unlike genesis, **an upgrade has to travel.** A v2 device's later changes depend on it, even plain ones like adding a transaction, so a v1 device that lacked it would queue every one of them forever. The relay's actor binding would reject it, since nobody's key is bound to a schema actor. So:

- **Every device publishes upgrades it holds** alongside its own changes. A duplicate is harmless.
- **Receivers check an upgrade structurally, since there is no signer binding.** The change may only create new, empty root maps, and it must be byte-for-byte the canonical change for the keys it creates. A v1 receiver can't know what v2 is *supposed* to add, but it can check that. This refuses two attacks:
  - A "schema" change that re-creates `date`, which would hide every row (behavior 1): `a_forged_upgrade_that_recreates_a_container_is_refused`.
  - A change **squatting on the genuine v2 actor** with other content. The test confirms that once a doc holds the squatter, the real upgrade can never be applied: a permanent denial of service from one malicious write. The content-derived actor name is what makes this checkable (`an_upgrade_squatting_on_the_real_v2_actor_is_refused`).
- **Compaction keeps upgrades and copies containers it doesn't know.** A v1 owner compacting a doc with a v2 `tags` column keeps it, and still strips erased rows from it. That works because every transactions column is keyed by transaction id (`a_v1_compaction_keeps_v2_columns_and_strips_erased_rows_from_them`). Before 2d, compaction copied only the columns it knew, so **a v1 owner's compaction would have erased v2 data.** A device rebasing onto a compaction first grafts on any upgrades the compactor never saw, and those count as unpublished (`a_rebase_keeps_an_upgrade_the_compactor_never_saw`). Getting that last part wrong was a real bug in the first version: the device marked the upgrade as published, so nobody else could apply its rebased writes.

### Meaning changes: a minimum-client gate

Some additions change what existing numbers mean. Example: splitting one transaction across categories, where a v1 client summing `amount` per category would show wrong totals. Automerge can't help with that. The budget doc carries **add-only** `requires/<n>` keys in `meta`. A v2 client writes `requires/2` the first time it uses such a feature. A client older than the highest marker refuses to compute money or write, and shows "update Nels to see this budget" (`schema::check_client`, tested in `a_v1_client_refuses_a_budget_that_requires_v2`). Add-only keys matter: an LWW `min_version` value could be lowered by a concurrent write from an older device, but a key can only be added.

### Paired fields

This closes the disposition's open item. Two devices edit a (benefit, quoted-at-age) pair concurrently: #466's Social Security shape. X switches to the figure quoted at 62; Y updates only the benefit, which it believes is quoted at 67. **Stored as two keys, one actor ordering tears the pair** into a combination nobody wrote (Y's 67-figure labelled as quoted at 62). **Stored as one value it can't tear** (`paired_fields_tear_unless_stored_as_one_value`).

Mutation-checked: not publishing upgrades, skipping the canonical rebuild, skipping the receive check, copying only known columns in compaction, not keeping upgrades in the compacted doc, or not grafting local upgrades in rebase each fails at least one test.

### Schema-evolution rules

1. **Additive only.** New fields, keys, columns and enum values. Never rename a field (the old client keeps writing the old name, and the edit is invisible to the new one). Never change a field's type. Never reuse a retired name.
2. **Field-level writes only.** Never replace an object to "save" it.
3. **Genesis is frozen forever.** New containers come from canonical schema upgrade changes, and each version's upgrade content is frozen too. The content-derived actor keeps two different upgrades from colliding, but a container name must never be created by two upgrades (the second would be refused as not new).
4. **New transactions columns are keyed by transaction id**, so older compactors can strip erased rows from them.
5. **Unknown enum values are preserved and shown with a fallback,** never coerced and written back.
6. **A feature that changes what existing numbers mean writes a `requires/<n>` marker.** Older clients go read-only with an update prompt. Use it sparingly: every marker locks older clients out of that household.
7. **Fields that must change together are one value.**
8. **Deprecating a field:** stop writing it, keep reading it with a fallback for as long as any client might still write it, and never delete or reuse it. Its data can be dropped at a later compaction.

**Verdict for 2d: go.** Mixed versions work without clobbering under these rules. Two things need machinery: new containers (schema upgrades, which also need relay and compaction support) and meaning changes (the minimum-client gate). The squatting finding is a strong argument for keeping **one small, reviewed write-path module that owns document layout**, the same conclusion Week 1 reached.

## Recommendation

**Go with changes.** Against the plan's decision criteria:

| Criterion | Result |
|---|---|
| Every 🔴 scenario has a shippable resolution | **Met.** Two need product changes: soft-closed budgets with late entries to review (#8), and rollup links that can show as inactive (#9). |
| Roles enforceable without the relay reading plaintext | **Met,** with an interim scheme (2a). The relay is trusted for freshness only. |
| Member removal works for future data | **Met** (2a). A removed member keeps what they already had; this has to be stated in-product. |
| 5-year household loads the current period well under a second | **Met by a wide margin:** 5.6 ms with per-month docs and a local summary cache; 178 ms loading everything (2b). |
| No-go: an invariant with money consequences can't be made convergent | **Not hit.** Every constraint has a disposition. Fund balances and rollup totals are derived and pass outright. |
| No-go: role enforcement requires the server to read plaintext | **Not hit.** |

**"With changes" means:**

1. **Product changes:** soft close (#8); inactive rollup links (#9); read-time folding of duplicate category names, which is stricter than today's UNIQUE (#5); in-product statements that removal and erasure can't recall copies already made (2a, 2c); and an "update Nels to see this budget" state (2d).
2. **Crypto:** ship the interim roster/envelope scheme (2a), and get an external review before launch; the protocol is hand-assembled from audited primitives but is itself unreviewed. Plan to move to Keyhive once it is audited. Keep envelope and roster formats versioned so that move is a protocol swap, not a data migration.
3. **Architecture rules the whole team has to hold:** the layout rules (Week 1, plus 2c rule 4 and the 2d rules), and one reviewed write-path module that owns them. Every failure the spike found was silent or permanent (hidden writes, torn pairs, `DuplicateSeqNumber`), so this can't be left to per-feature review.

### What local-first does NOT change: be precise in the trust claim

- **Bank-linked data is plaintext at Nels regardless.** Bank linking stays on the server (plan non-goal): the server fetches from Plaid, GoCardless and the others, and holds the feed. E2E encryption protects everything users *add*: budgets, categories, manual entries, notes, edits, retirement profile and goals. It does not protect imported bank transactions from Nels. The pitch has to say that, or be scoped to users who don't link a bank.
- **AI is the other plaintext path.** Chat builds context from the user's data. Unless the model call is bring-your-own-key or runs locally, that context passes through Nels in plaintext at request time. It isn't stored, but "end-to-end encrypted" would still overclaim. Chat has to become client-side (context building plus the action executor) with a BYO key, a local model, or a clearly disclosed Nels proxy.
- **Key loss is data loss.** The spike didn't cover device loss or recovery. A user who loses every device loses everything unless there is a recovery key or an escrowed, encrypted backup. This is a product decision with real UX weight, and the largest open question left.

### Estimated size of the real migration

Rough, from the current codebase: about 72k lines of backend Rust including inline tests (rag.rs 20k, budget.rs 15k), 93 REST routes, 41 chat actions, 51 migrations, and a 12k-line Svelte frontend with 18 components. **About 35–55 engineer-weeks** before launch hardening, i.e. roughly 8–13 months for one engineer:

| Work | Estimate |
|---|---|
| Client data layer: Automerge in the PWA (JS or Wasm), the doc model and derived reads (port the spike's `model`/`read`, plus the rest of `budget.rs`'s domain rules), IndexedDB storage, offline | 8–12 wk |
| Sync service: relay, roster, device enrollment and pairing, member keys certifying device keys (2a gap), compaction jobs and relay backup retention (2c) | 6–9 wk |
| External crypto and protocol review, plus fixes | 2–4 wk, plus calendar time |
| Chat: client-side context building and an executor for the 41 actions; model access by BYO key, local model or proxy | 6–10 wk |
| Bank linking bridge: providers stay server-side, and devices import from a server feed carrying `seq`/`first_seen` (2b); server-side erasure of feed records (2c) | 3–5 wk |
| Retirement and assets: move to the per-user private doc; `retirement_projection.rs` is already pure, so it ports to Wasm directly | 2–3 wk |
| Features that are server jobs today: limit notifications, insights and reports move client-side or are dropped | 2–4 wk |
| Migrating existing users: one-time export from Postgres into docs, key setup at first login, a period of running both | 3–5 wk |
| Key recovery and device-loss UX | 3–5 wk |

**Recommended sequencing:**

1. **Now:** open source plus BYO-key AI on the current architecture. That was the cheaper fallback, and it is also step one of local-first either way.
2. Extract the domain rules into a pure core that both the server and a future client can run (Rust compiled to Wasm; `retirement_projection.rs` is the precedent).
3. Build the client store and sync for **new** households behind a flag, with the external review running in parallel.
4. Migrate existing households once recovery UX and the review are done.

Revisit step 3 if trust objections turn out to be mostly about AI and open source rather than storage. Step 1 answers those on its own, for a small fraction of the cost.
