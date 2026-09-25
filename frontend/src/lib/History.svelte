<script>
  import { ArrowLeft, AlertTriangle, MessageSquare, MoreHorizontal, Pencil, Trash2 } from "lucide-svelte";
  import { _, locale } from "svelte-i18n";
  import { get } from "svelte/store";
  import { groupConversations, relativeTime, titleFor, shouldCommitRename, findFirstUserMessage } from "./history.js";

  // `conversations` comes from App.svelte's own state (the same array
  // Sidebar.svelte used to consume) — this component does not self-fetch, so
  // rename/delete stay in sync with the single source of truth instead of
  // risking a second, potentially-stale copy of the list.
  //
  // `hasDraft` tells this component whether App.svelte's chat input already
  // has unsaved text — it decides whether row clicks need the overwrite
  // confirm dialog. `onPromptSelected(text)` is the parent's hook to set
  // chatInput and navigate back to chat; this component never touches
  // chatInput or route state directly (App.svelte owns both).
  //
  // `onRename(id, title)` / `onDelete(id, name)` are the same callback
  // contract Sidebar.svelte already exposed — App.svelte wires them to its
  // existing handleRenameConversation/requestDeleteConversation unchanged.
  let { conversations = [], fetchApi, onBack, onPromptSelected, hasDraft = false, onRename, onDelete } = $props();

  let pendingSelection = $state(null); // { id, title } while the overwrite-confirm dialog is open
  let menuOpenId = $state(null);
  let renamingId = $state(null);
  let renameValue = $state("");
  let backButtonEl = $state(null);
  let selectingId = $state(null); // conversation id whose messages are currently being fetched
  let selectError = $state("");

  // Matches Settings.svelte/DeleteAccount.svelte's own mount-focus effect —
  // keyboard/screen-reader users landing on this route get focus placed
  // deliberately instead of left at document body.
  $effect(() => {
    backButtonEl?.focus();
  });

  const groups = $derived(groupConversations(conversations));

  const groupLabelKey = { Today: "sidebar.groupToday", Yesterday: "sidebar.groupYesterday", Earlier: "sidebar.groupEarlier" };

  function displayTitle(c) {
    return titleFor(c) ?? get(_)("sidebar.newConversation");
  }

  function startRename(c) {
    menuOpenId = null;
    renamingId = c.id;
    renameValue = displayTitle(c);
  }

  function commitRename(c) {
    const v = renameValue.trim();
    if (shouldCommitRename(v, displayTitle(c))) onRename?.(c.id, v);
    renamingId = null;
    renameValue = "";
  }

  // Finds the first user-sent message via the existing
  // GET /conversations/:id/messages endpoint (no new backend work — see
  // spec). The backend's ChatHistoryItem uses a `sender` field (values
  // "user" / "ai"), confirmed against backend/src/rag.rs.
  async function selectConversation(c) {
    if (hasDraft) {
      pendingSelection = { id: c.id, title: displayTitle(c) };
      return;
    }
    await applySelection(c.id);
  }

  // Wrapped in try/catch (unlike an earlier draft) so a failed fetch — a
  // deleted-concurrently conversation (404), a network drop, a timeout —
  // surfaces to the user instead of becoming a silently-swallowed unhandled
  // rejection with the row just doing nothing on click. Matches every other
  // fetchApi-driven action in this codebase (see CategoriesView.svelte's own
  // load(), or App.svelte's handleRenameConversation/downloadExport).
  async function applySelection(conversationId) {
    selectError = "";
    selectingId = conversationId;
    try {
      const messages = await fetchApi(`/conversations/${conversationId}/messages`);
      const firstUserMessage = findFirstUserMessage(messages);
      if (!firstUserMessage) return; // edge case: no user message — no-op, row stays clickable
      onPromptSelected?.(firstUserMessage.message_text);
    } catch (e) {
      selectError = e?.message || get(_)("history.error");
    } finally {
      selectingId = null;
    }
  }

  async function confirmOverwrite() {
    const target = pendingSelection;
    pendingSelection = null;
    if (target) await applySelection(target.id);
  }

  function cancelOverwrite() {
    pendingSelection = null;
  }
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("history.title")}</h3>
    <button type="button" bind:this={backButtonEl} class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" /> {$_("history.back")}
    </button>
  </div>

  {#if selectError}
    <div class="alert alert-error mb-3">
      <AlertTriangle class="w-5 h-5" />
      <span>{selectError}</span>
    </div>
  {/if}

  {#if conversations.length === 0}
    <div class="flex flex-col items-center justify-center flex-grow text-base-content/60 gap-3 py-12">
      <MessageSquare class="w-10 h-10 opacity-40" />
      <span>{$_("history.empty")}</span>
    </div>
  {:else}
    <div class="flex-grow overflow-y-auto space-y-4">
      {#each groups as group (group.label)}
        <div>
          <div class="text-xs font-semibold text-base-content/50 uppercase px-2 mb-1">
            {$_(groupLabelKey[group.label])}
          </div>
          <ul class="space-y-1">
            {#each group.items as c (c.id)}
              <li>
                {#if renamingId === c.id}
                  <input
                    type="text"
                    bind:value={renameValue}
                    onblur={() => commitRename(c)}
                    onkeydown={(e) => {
                      if (e.key === "Enter") commitRename(c);
                      if (e.key === "Escape") {
                        renamingId = null;
                        renameValue = "";
                      }
                    }}
                    class="w-full rounded-lg px-3 py-2.5 text-sm bg-base-100 border border-secondary text-base-content focus:outline-none"
                  />
                {:else}
                  <div class="relative group">
                    <button
                      type="button"
                      class="flex items-center justify-between gap-2 w-full text-left px-3 py-2.5 rounded-lg hover:bg-base-200 min-h-[44px] disabled:opacity-60"
                      disabled={selectingId === c.id}
                      aria-label={selectingId === c.id ? $_("history.loading") : undefined}
                      onclick={() => selectConversation(c)}
                    >
                      <span class="truncate">{displayTitle(c)}</span>
                      {#if selectingId === c.id}
                        <span class="loading loading-spinner loading-xs shrink-0"></span>
                      {:else}
                        <span class="text-xs text-base-content/50 shrink-0 {menuOpenId === c.id ? 'invisible' : 'group-hover:invisible'}">{relativeTime(c.updated_at, new Date(), get(locale))}</span>
                      {/if}
                    </button>
                    <button
                      type="button"
                      aria-label={$_("sidebar.conversationActionsAria")}
                      onclick={() => (menuOpenId = menuOpenId === c.id ? null : c.id)}
                      class="absolute right-1.5 top-1/2 -translate-y-1/2 p-1 rounded text-base-content/60 hover:text-base-content hover:bg-base-300 {menuOpenId === c.id ? 'visible' : 'invisible group-hover:visible'}"
                    >
                      <MoreHorizontal class="w-3.5 h-3.5" />
                    </button>
                    {#if menuOpenId === c.id}
                      <div class="absolute right-1.5 top-full z-50 mt-1 w-32 rounded-lg bg-base-200 border border-base-300 shadow-2xl py-1">
                        <button
                          type="button"
                          onclick={() => startRename(c)}
                          class="flex items-center gap-2 w-full px-3 py-1.5 text-xs text-base-content/80 hover:bg-base-100"
                        >
                          <Pencil class="w-3.5 h-3.5" /> {$_("sidebar.rename")}
                        </button>
                        <button
                          type="button"
                          onclick={() => {
                            menuOpenId = null;
                            onDelete?.(c.id, displayTitle(c));
                          }}
                          class="flex items-center gap-2 w-full px-3 py-1.5 text-xs text-error hover:bg-base-100"
                        >
                          <Trash2 class="w-3.5 h-3.5" /> {$_("sidebar.delete")}
                        </button>
                      </div>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/each}
    </div>
  {/if}
</div>

{#if pendingSelection}
  <div class="modal modal-open" role="alertdialog" aria-modal="true">
    <div class="modal-box bg-base-200 border border-base-300 max-w-md">
      <h3 class="font-bold text-lg">{$_("history.confirmOverwriteTitle")}</h3>
      <p class="py-4 text-base-content/80">{$_("history.confirmOverwriteBody")}</p>
      <div class="modal-action">
        <button class="btn btn-ghost" onclick={cancelOverwrite}>{$_("common.cancel")}</button>
        <button class="btn btn-primary" onclick={confirmOverwrite}>{$_("history.confirmOverwriteConfirm")}</button>
      </div>
    </div>
    <button class="modal-backdrop" aria-label={$_("common.cancel")} onclick={cancelOverwrite}></button>
  </div>
{/if}
