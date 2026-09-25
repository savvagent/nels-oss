# FIFO Chat Queue + Auto-Send — Design Spec

Source: `savvagent/nels#230` — "Feature Request: Allow typing while AI is responding (FIFO queue + auto-send)"

## Brief (verbatim AC from the issue)

- While Nels is generating, the user can type in the textarea (it is not disabled and retains focus).
- Pressing send (or Enter) during generation queues the message, clears the input, and shows a "N queued" indicator.
- When the current response returns, queued items are sent automatically in FIFO order, one at a time.
- Each queued item's user bubble and Nels reply appear in the transcript in correct chronological order.
- Slash commands can be queued and execute correctly when dequeued (local commands run client-side; AI commands POST).
- A failed in-flight or queued request shows its error bubble and does not block remaining queued items.
- Mic/dictation, history-recall button, and arrow-key prompt recall are usable during generation.
- The user can cancel/clear queued items before they send.
- No duplicate `conversation_id` is created and replies never arrive out of order.

Out of scope (per issue): streaming responses / stop-generation, concurrent in-flight requests.

## Current state (verified fresh against `frontend/src/App.svelte` on this branch, `wc -l` = 2363 lines — issue line numbers are stale, real ones cited below)

- `isChatLoading` (`:274`) gates: textarea `disabled` (`:2144`), send button `disabled` (`:2201`,
  combined with `!chatInput.trim()`), mic button `disabled` (`:2167`), history-recall button
  `disabled` (`:2180`), the two arrow-key recall guards in `handleChatInputKeydown` (`:236`, `:247`),
  and is the sole gate at the top of `sendChatMessage` (`:1006`: `if (!chatInput.trim() ||
  isChatLoading) return;` — this line is what currently short-circuits Enter-to-send, since
  `handleChatInputKeydown` always calls `sendChatMessage()` unconditionally on Enter).
- `sendChatMessage` (`:1004-1145`) does, in order: re-pin scroll (`pinnedToBottom`/`showAllMessages`),
  intercept slash commands via `isCommand`/`runCommand` (`lib/commands.js`) with a `tick()`+refocus
  afterward (`:1024-1025`, needed only because the textarea was disabled during `runCommand`'s own
  internal `isChatLoading` toggles), else optimistically append a user bubble, `POST /chat` via
  `fetchApi`, append the AI reply (or an error bubble on catch), refresh budgets/notifications,
  handle assistant navigation signals, refresh the conversation list, and — in `finally` — clear
  `isChatLoading` and (again) `tick()`+refocus (`:1142-1143`), guarded off when a modal has focus.
- `activeConversationId` (`:71`) is set from `chatRes.conversation_id` after every successful POST
  and read back into the next POST's body — the lazy-conversation-creation invariant the issue's "No
  conversation race" note is protecting.
- `runCommand` (`:803-990`) itself toggles `isChatLoading` per-branch (network-bound commands like
  `/issues-list`, `/budgets-insights` does not, `/clear` does not) — this is pre-existing and
  unchanged by this feature; see Risks below.
- No pure-JS extraction yet for queue logic; existing precedent for this is `frontend/src/lib/
  chatWindow.js` (windowing) and `frontend/src/lib/budgetDisplay.js` (status formatting) — both
  small, framework-free, unit-tested modules that `App.svelte` imports, because there is no
  component-test harness in this repo (`vitest` runs `environment: "node"`, confirmed in
  `frontend/vitest.config.js` and every existing `*.test.js`).
- Popover-with-list precedent: `frontend/src/lib/BudgetStatusInfo.svelte` and
  `frontend/src/lib/Notifications.svelte` — click-toggle, outside-click via `<svelte:window
  onclick>`, `Escape`-close.
- i18n: `svelte-i18n`, 6 locale files under `frontend/src/lib/i18n/locales/` (`en`, `de`, `es`, `fr`,
  `it`, `pt`), flat-ish nested JSON, referenced via `$_("key", {values:{...}})` in templates / `t()`
  in script.

## Assumptions

1. **Queue cap = 10**, the issue's own proposed value, taken as final (issue: "Value open for
   tuning" — no other signal to tune against, so ship the proposed default).
2. **Unify direct-send and queued-send into one mechanism**: every `sendChatMessage` call — busy or
   not — pushes `{id, text}` onto `pendingQueue` and then calls a re-entrancy-guarded `drainQueue()`.
   When not busy, `drainQueue()` synchronously dequeues that single item and sends it before any
   paint, so the "N queued" indicator never flashes for a direct send. This satisfies the issue's
   literal integration note ("a drain routine invoked from `sendChatMessage`'s `finally`") in
   spirit — sequential, single-drain, FIFO — while avoiding recursive finally-triggered drains (the
   literal reading) in favor of one explicit `while` loop, which is simpler to reason about and to
   unit-test the cap/order logic of in isolation. Documented here as the one point where I depart
   from the issue's literal code sketch.
3. **New pure module `frontend/src/lib/chatQueue.js`** (`QUEUE_CAP`, `canEnqueue`, `enqueue`,
   `dequeue`, `removeFromQueue`) — mirrors the `chatWindow.js`/`budgetDisplay.js` precedent: FIFO
   ordering, cap enforcement, and removal are meaningfully testable logic, and extracting them keeps
   `App.svelte`'s script thin and gives real unit coverage in a repo where the component itself
   can't be tested.
4. **New `QueuedMessages.svelte` component** for the "N queued" indicator + preview/cancel popover,
   mirroring `BudgetStatusInfo.svelte`'s click-toggle/outside-click/Escape-close pattern (same repo
   idiom, not a new one). Not unit-testable per the repo's no-component-harness convention (same
   carve-out `BudgetStatusInfo.svelte`'s plan took) — verified manually.
5. **Re-pin-to-bottom (`pinnedToBottom = true; showAllMessages = false`) moves from the top of
   `sendChatMessage` into the shared per-item send routine (`performSend`)**, so it fires at
   *dequeue* time for every drained item, not just the first. This is a small behavioral
   improvement consistent with the existing #108 comment's intent (a user's own message should
   scroll into view) and required because the transcript now grows across multiple sequential
   sends instead of one.
6. **The `tick()`-then-refocus workaround is removed entirely** (both call sites: the command branch
   `:1024-1025` and the `finally` block `:1142-1143`). It existed solely to restore focus a disabled
   textarea drops; the textarea is never disabled now, so focus is never lost and the workaround has
   no remaining purpose. (`pendingDeletion`-modal focus handling is unrelated and untouched — it
   never depended on `isChatLoading`.)
7. **Queue item ids** use the same ad-hoc `Math.random().toString()` pattern already used for every
   chat-message id in this file, for consistency — no new id scheme introduced.
8. **Cap enforcement blocks the *send* action outright** (`sendChatMessage` no-ops, leaving the
   composer text untouched) rather than silently dropping the newest item — matches "send is
   disabled with a short hint," i.e. the guard rail is at the point of submission, not silent data
   loss.
9. A known, **pre-existing, out-of-scope race**: a handful of `runCommand` branches that don't touch
   the network (`/clear`, `/categories-list`, `/budgets-insights`) never set `isChatLoading`, so
   there is a sub-millisecond window where `busy` is false while such a command's promise is still
   settling. This already exists today (those commands never disabled the textarea either) and is
   unrelated to the FIFO/queue mechanism being added — not fixed here, called out so a reviewer
   doesn't mistake it for a regression.

## Goal & Success Criteria

Let the user keep composing (typing, dictating, recalling prompt history, running commands) while a
chat response is in flight, queuing anything submitted mid-flight and auto-sending it, strictly
sequentially, once the current turn resolves — with zero behavior change to the single-request path
when nothing is queued.

- [ ] Composer (textarea, mic, history-recall, arrow-key recall) is fully interactive while
      `isChatLoading`/draining is true.
- [ ] Submitting while busy queues (doesn't send), clears input, shows a live queued-count
      indicator with per-item preview + cancel and a clear-all action.
- [ ] Queued items auto-drain strictly in FIFO order, one at a time, after the in-flight response
      settles (success or error).
- [ ] Slash commands queue and dequeue through the exact same interception code path as a direct
      send (no branching by "was this queued").
- [ ] A single conversation id is reused across an entire drain; no two `POST /chat` calls are ever
      in flight concurrently.

## Scope

**In scope:** `frontend/src/App.svelte` refactor (send/queue/drain wiring, disables relaxed, button
gating), new `frontend/src/lib/chatQueue.js` (+ unit tests), new
`frontend/src/lib/QueuedMessages.svelte`, 6 locale files (new `chat.queued*` keys — a single
interpolated `"{count} queued"` string, no plural-form branching: this matches the repo's existing
non-ICU interpolation style, e.g. `chat.showEarlier`'s `"Show {count} earlier messages"`, which
also doesn't distinguish singular/plural. `"1 queued"` reading slightly odd grammatically is
accepted, matching that precedent and the issue's own literal example text).

**Out of scope:** streaming/`AbortController`/stop-generation (explicitly deferred by the issue);
concurrent in-flight requests; changing `runCommand`'s per-branch `isChatLoading` toggling; any
backend change (`POST /chat` contract is unchanged).

## Architecture

```
sendChatMessage(e)                     -- form submit / Enter key entry point
  guard: empty input -> no-op
  guard: !canEnqueue(pendingQueue) -> no-op (cap hit, input left untouched)
  chatInput = ""                       -- always clears on a successful queue/send attempt
  pendingQueue = enqueue(pendingQueue, id, raw)
  drainQueue()                         -- fire-and-forget; internally re-entrancy guarded

drainQueue()                           -- the ONLY place performSend is invoked
  if isDraining: return
  isDraining = true
  try:
    await runDrain(() => pendingQueue, performSend, (rest) => { pendingQueue = rest })
  finally:
    isDraining = false
```

`runDrain(getQueue, sendFn, onQueueChange)` (in `chatQueue.js`, pure aside from calling the three
injected functions) is the FIFO loop itself, extracted so it is unit-testable with a mock `sendFn`
— including a `sendFn` that **throws**, which is exactly the round-1 spec-review defect (a throw
escaping the drain loop and permanently wedging `isDraining`). Critically, `runDrain` takes a
**live accessor `getQueue()`, not a one-time array** — every loop iteration (both the `while`
condition and the dequeue itself) calls `getQueue()` fresh rather than tracking a private local
snapshot. This is deliberate, not incidental: a plan-review pass caught that an earlier draft of
this design (a `runDrain(queue, ...)` taking a plain array) silently broke on the exact scenario
the whole feature exists for — enqueuing (or cancelling) an item *while `sendFn`'s `await` for the
current item is still pending* would be invisible to a loop holding a stale local copy, so that
item would sit in `pendingQueue` (the indicator would show it) with nothing left running to ever
drain it, and a mid-drain cancel/clear would be silently overwritten back by the loop's next
`onQueueChange` call. Always reading through `getQueue()` closes both holes: `App.svelte` passes
`() => pendingQueue`, so every iteration sees whatever `pendingQueue` actually is at that instant,
including any enqueue/remove/clear that happened during the just-finished `await`. `onQueueChange(rest)`
is still called immediately after each dequeue (before `sendFn` resolves) purely so the "N queued"
indicator drops the instant an item starts sending; `sendFn` is `await`ed wrapped in `runDrain`'s
own internal `try/catch` (swallow-and-continue — belt-and-suspenders alongside `performSend`'s own
`try/catch` on its command branch), and the function only returns once `getQueue()` reports empty.
This means `drainQueue`'s `try/finally` is defense-in-depth on *two* independent layers (an
internal catch inside `runDrain` and the outer `finally`), and both critical invariants — "a
failing item never stops the drain" and "a queue mutated mid-drain is still drained/respected
correctly" — have direct unit tests instead of only a manual one.

```

performSend(rawInput)                  -- exactly today's sendChatMessage body, parameterized
  pinnedToBottom = true; showAllMessages = false
  if isCommand(rawInput):
    recordPrompt
    try: await runCommand(rawInput)
    catch: append error bubble           -- defensive: every existing runCommand branch already
                                          -- self-catches its own network call (see Current
                                          -- state), so this should never actually trigger, but
                                          -- performSend must not be a throw point either way —
                                          -- see the Error Handling note on drainQueue re-entrancy
    return
  recordPrompt; append optimistic user bubble
  isChatLoading = true
  try: POST /chat -> append AI bubble, sync activeConversationId, budget/notif refresh,
       navigation signals, fetchConversations
  catch: append error bubble
  finally: isChatLoading = false
```

`frontend/src/lib/chatQueue.js` (pure aside from `runDrain`'s injected callbacks, unit-tested):
- `QUEUE_CAP = 10`
- `canEnqueue(queue, cap = QUEUE_CAP)` → `boolean`
- `enqueue(queue, id, text, cap = QUEUE_CAP)` → new array, unchanged input if at cap
- `dequeue(queue)` → `{ item: {id,text} | null, rest: newArray }`
- `removeFromQueue(queue, id)` → new array with that id filtered out
- `async runDrain(getQueue, sendFn, onQueueChange)` → drains whatever `getQueue()` returns, FIFO,
  via `sendFn(text)`, re-reading `getQueue()` fresh every iteration (never a cached snapshot — see
  the Architecture note above on why); calls `onQueueChange(rest)` after each dequeue; swallows a
  `sendFn` rejection internally and continues to the next item; resolves once `getQueue()` is
  empty. No cap/id concerns — purely sequencing.

A queued item's optimistic user bubble is appended to `chatMessages` only inside `performSend`
(i.e. at *dequeue/send* time), never at *enqueue* time — `enqueue` only ever touches `pendingQueue`,
never `chatMessages`. This is what makes AC4's "correct chronological order" hold: the transcript
gains entries strictly in send order, never in queue order. Not-yet-sent text is represented purely
by `QueuedMessages.svelte`'s "N queued" indicator, never by a placeholder transcript bubble.

`frontend/src/lib/QueuedMessages.svelte` — mounted next to the textarea; props `{ queue,
onRemove(id), onClearAll() }`. Renders nothing when `queue.length === 0`. Otherwise a small pill
("N queued") that toggles a popover listing each queued item's text (truncated) with a per-item
remove (×) button and a "Clear all" action — click-toggle / outside-click / Escape-close, mirroring
`BudgetStatusInfo.svelte`.

`App.svelte` disables to relax: textarea (drop `disabled={isChatLoading}` entirely), mic button
(drop `disabled={isChatLoading}`), history-recall button (drop the `isChatLoading ||` half, keep
`promptHistory.length === 0`), the two arrow-key recall guards in `handleChatInputKeydown` (drop the
`!isChatLoading &&` half of each condition). Send button: `disabled={!chatInput.trim() ||
!canEnqueue(pendingQueue)}`, i.e. gated on trimmed input and the cap, never on `isChatLoading`. A
cap-hit hint (`chat.queueFull`) renders under the input when `!canEnqueue(pendingQueue)`.

## Error Handling & Edge Cases

- **Per-item failure isolation, both branches of `performSend`**: the `POST /chat` branch already
  self-catches today (`catch: append error bubble`). The slash-command branch does NOT self-catch
  today — every individual `runCommand` branch that calls the network already wraps its own
  `fetchApi` in `try/catch` (see Current state), so `runCommand` itself should never throw in
  practice — but `performSend`'s command branch gets its own `try/catch` anyway (Architecture,
  above) so `performSend` is *never* a throw point regardless of `runCommand`'s internals. This
  closes a gap the initial spec draft missed: without it, an unexpected `runCommand` throw would
  propagate out of `drainQueue`'s `while` loop, skip the `isDraining = false` reset, and
  permanently wedge auto-drain for the rest of the session (every later `sendChatMessage` would
  enqueue but nothing would ever send again) — silently violating AC bullet 6 far more severely
  than a single missed error bubble. `drainQueue` additionally wraps its own loop in `try/finally`
  as a second, independent layer of defense against exactly that failure mode, so `isDraining` is
  guaranteed to reset even if some future `performSend` change reintroduces a throw path.
- **Cap hit**: `sendChatMessage` no-ops (input text stays so nothing is lost); the indicator shows
  the cap hint.
- **Cancel one item**: `removeFromQueue` — safe mid-drain because `runDrain` re-reads
  `getQueue()` fresh every iteration (never a cached snapshot, per the Architecture fix above), so
  removing an item that hasn't been dequeued yet simply means the next iteration's `getQueue()`
  call never sees it. Covered by a dedicated `runDrain` unit test (Testing Approach) as well as
  manual verification, since this was the exact class of bug the plan-review pass caught.
- **Clear all**: `pendingQueue = []` — safe mid-drain for the same reason; the item currently
  *in flight* (already dequeued, `performSend` running) is not cancelable — matches the issue
  ("cancel queued items before they send"; out-of-scope: no `AbortController`).
- **Reentrancy**: `isDraining` boolean guards `drainQueue` against being started twice (e.g. a
  fire-and-forget call racing with the one still running from the prior `sendChatMessage`).
- **Reload/unmount mid-queue**: not persisted — matches the issue (no persistence requirement) and
  the existing `promptHistory` precedent (session-scoped, in-memory only).

## Testing Approach

- **Unit (Vitest, `frontend/src/lib/chatQueue.test.js`)**: TDD, covers `canEnqueue`/`enqueue` at/under/
  over cap, FIFO `dequeue` ordering including empty-queue, `removeFromQueue` (present/absent id,
  preserves order of the rest), and `runDrain`: sends items in FIFO order via a mock `sendFn`
  backed by a live `getQueue()` accessor, calls `onQueueChange` with the correct remaining array at
  each step, resolves on an empty queue, — the regression test for the round-1 spec-review defect —
  **completes the full drain even when `sendFn` rejects for one or more items**, proving a failing
  item can never wedge the queue, AND — the regression test for the plan-review defect — **picks up
  an item enqueued (via a mock `sendFn` that mutates the backing store mid-call) while a previous
  item's `sendFn` call is still pending**, proving a stale local snapshot can never cause a queued
  item to be silently dropped or a mid-drain cancel/clear to be silently undone.
- **Existing suite**: `cd frontend && pnpm test` must stay green (no regressions to
  `budgetDisplay.test.js`, `chatWindow.test.js`, `router.test.js`).
- **Build**: `cd frontend && pnpm run build` must succeed.
- **Manual** (no component-test harness in this repo — same carve-out as `BudgetStatusInfo.svelte`):
  drive `pnpm run dev` against a real/staging backend session and walk every AC bullet — type during
  generation, queue 2-3 items (mix of plain messages and a slash command), confirm FIFO delivery,
  correct transcript order, a forced-failure item (e.g. temporarily kill the backend) not blocking
  the rest — exercise this for BOTH a queued plain message (kills the `POST /chat` branch) AND a
  queued network-bound slash command like `/tokens` or `/budgets-list` (kills the `runCommand`
  branch) to specifically confirm draining survives a command failure, not just a chat-POST
  failure — cap-hit hint at 10 queued, individual cancel + clear-all, mic/recall usable mid-flight,
  and — the sequential-processing invariant — no duplicate `conversation_id` (inspect the Network
  tab across a multi-item drain).

## Risks & Open Questions

- Cap value (10) is a proposed default per the issue, not empirically tuned — acceptable per
  Assumption 1.
- The pre-existing non-network-command micro-race (Assumption 9) is flagged, not fixed — filing a
  follow-up rather than silently widening this ticket's scope if it's judged worth fixing later.
- No component-test harness means `QueuedMessages.svelte` and the `App.svelte` wiring are verified
  manually, not by an automated test — consistent with this repo's established practice for Svelte
  component code (see the header-simplification plan, which took the same position for
  `BudgetStatusInfo.svelte`).
