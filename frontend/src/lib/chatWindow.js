// Pure helpers for windowing the rendered chat transcript.
//
// A long session can accumulate hundreds of bubbles, which makes the message
// list sluggish on mobile (DOM node count + Svelte reactivity over a large
// list). We render only the most recent messages by default and let the user
// reveal the rest on demand. Keeping the slice math here — framework-free,
// mirroring commands.js — lets it be unit-tested without mounting the Svelte
// component. The component owns the reactive state and the on-demand re-fetch;
// this module only decides what to show. Recall is unaffected: every message
// stays in the DB and pgvector regardless of what the client renders.

export const MAX_VISIBLE_MESSAGES = 30;

// Given the full message list and whether the user has chosen to reveal all,
// return { visible, hiddenCount }.
//   - showAll true, or list length at/under `max`: the whole list is visible,
//     hiddenCount 0.
//   - otherwise: `visible` is the most recent `max` messages (original order
//     preserved) and `hiddenCount` is how many older messages are withheld from
//     the top.
// Non-array input is treated as empty.
export function windowMessages(messages, showAll, max = MAX_VISIBLE_MESSAGES) {
  const list = Array.isArray(messages) ? messages : [];
  if (showAll || list.length <= max) {
    return { visible: list, hiddenCount: 0 };
  }
  return { visible: list.slice(-max), hiddenCount: list.length - max };
}
