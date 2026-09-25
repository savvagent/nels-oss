<script>
  // Admin users table: fetches GET /admin/users and renders a sortable table.
  // Sorting is client-side; clicking a column header toggles asc/desc and shows
  // a ▲/▼ indicator. Numeric and timestamp columns sort by their underlying
  // value, not their formatted display string.
  import { apiFetch, ApiError } from "./api.js";

  let users = $state([]);
  let loading = $state(true);
  let error = $state("");

  // Column definitions. `key` is the field read from each user row; `type`
  // selects the comparator (string / number / date). `numeric` columns map
  // null/undefined to 0; date columns to epoch.
  const columns = [
    { key: "email", label: "Email", type: "string" },
    { key: "name", label: "Name", type: "string" },
    { key: "created_at", label: "Created", type: "date" },
    { key: "input_tokens", label: "Input Tokens", type: "number" },
    { key: "output_tokens", label: "Output Tokens", type: "number" },
    { key: "total_tokens", label: "Total Tokens", type: "number" },
    { key: "first_activity", label: "First Activity", type: "date" },
    { key: "last_activity", label: "Last Activity", type: "date" },
    { key: "subscription_status", label: "Subscription", type: "string" },
  ];

  let sortKey = $state("total_tokens");
  let sortDir = $state("desc"); // "asc" | "desc"

  function colType(key) {
    return columns.find((c) => c.key === key)?.type ?? "string";
  }

  function toComparable(value, type) {
    if (type === "number") return Number(value) || 0;
    if (type === "date") {
      const t = value ? new Date(value).getTime() : NaN;
      return Number.isNaN(t) ? -Infinity : t;
    }
    return (value ?? "").toString().toLowerCase();
  }

  const sortedUsers = $derived.by(() => {
    const type = colType(sortKey);
    const dir = sortDir === "asc" ? 1 : -1;
    return [...users].sort((a, b) => {
      const av = toComparable(a[sortKey], type);
      const bv = toComparable(b[sortKey], type);
      if (av < bv) return -1 * dir;
      if (av > bv) return 1 * dir;
      return 0;
    });
  });

  function setSort(key) {
    if (sortKey === key) {
      sortDir = sortDir === "asc" ? "desc" : "asc";
    } else {
      sortKey = key;
      // Text defaults to ascending; numbers/dates default to descending
      // (largest / most recent first), which is the useful view here.
      sortDir = colType(key) === "string" ? "asc" : "desc";
    }
  }

  function fmtNumber(value) {
    const n = Number(value);
    return Number.isFinite(n) ? n.toLocaleString() : "—";
  }

  function fmtDate(value) {
    if (!value) return "—";
    const d = new Date(value);
    if (Number.isNaN(d.getTime())) return "—";
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  function fmtCell(col, value) {
    if (col.type === "number") return fmtNumber(value);
    if (col.type === "date") return fmtDate(value);
    return value || "—";
  }

  // Maps a user row to a { text, cls } badge for the Subscription column.
  // Pro (trialing/active, per billing::user_is_pro) renders green; a present
  // but non-entitled status (past_due, canceled, unpaid, etc.) renders amber
  // with the raw status text so support can distinguish "lapsed" from "never
  // subscribed". No status recorded yet renders a neutral "Free" — this covers
  // both "never started checkout" and "checkout started but no webhook has
  // synced a status yet" (a `subscriptions` row can exist with `status` still
  // NULL; see billing::ensure_customer), which are indistinguishable here.
  function subscriptionBadge(user) {
    if (user.is_pro) {
      return {
        text: user.subscription_status === "trialing" ? "Trialing" : "Pro",
        cls: "badge-success",
      };
    }
    if (user.subscription_status) {
      return { text: user.subscription_status, cls: "badge-warning" };
    }
    return { text: "Free", cls: "badge-neutral" };
  }

  async function load() {
    loading = true;
    error = "";
    try {
      const data = await apiFetch("/admin/users");
      users = Array.isArray(data) ? data : (data?.users ?? []);
    } catch (err) {
      if (err instanceof ApiError && err.status === 403) {
        error = "You do not have permission to view users.";
      } else {
        error = err.message || "Failed to load users.";
      }
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    load();
  });
</script>

<div class="p-4">
  <div class="flex items-center justify-between mb-4">
    <h2 class="text-xl font-semibold">Users</h2>
    <button class="btn btn-sm btn-ghost" onclick={load} disabled={loading}>Refresh</button>
  </div>

  {#if loading}
    <div class="flex items-center gap-2 py-8 justify-center">
      <span class="loading loading-spinner loading-md"></span>
      <span class="opacity-70">Loading users…</span>
    </div>
  {:else if error}
    <div class="alert alert-error" role="alert">{error}</div>
  {:else if sortedUsers.length === 0}
    <div class="opacity-70 py-8 text-center">No users found.</div>
  {:else}
    <div class="overflow-x-auto">
      <table class="table table-zebra table-sm">
        <thead>
          <tr>
            {#each columns as col}
              <th>
                <button
                  type="button"
                  class="inline-flex items-center gap-1 font-semibold cursor-pointer hover:opacity-80"
                  onclick={() => setSort(col.key)}
                >
                  {col.label}
                  {#if sortKey === col.key}
                    <span aria-hidden="true">{sortDir === "asc" ? "▲" : "▼"}</span>
                  {/if}
                </button>
              </th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each sortedUsers as user (user.id ?? user.email)}
            <tr>
              {#each columns as col}
                {#if col.key === "subscription_status"}
                  {@const badge = subscriptionBadge(user)}
                  <td>
                    <span class="badge {badge.cls}">
                      {badge.text}
                    </span>
                  </td>
                {:else}
                  <td>{fmtCell(col, user[col.key])}</td>
                {/if}
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
