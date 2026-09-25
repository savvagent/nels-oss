<script>
  // Admin PWA root. On mount, if a token exists, verify it against GET /auth/me:
  //   - is_admin === true  -> render the admin console (users table)
  //   - authenticated, not admin -> access-denied state with a sign-out button
  //   - 401 (stale token)  -> clear it and show the login form
  // No token -> show the login form.
  import { apiFetch, ApiError, getToken, clearToken } from "./lib/api.js";
  import Login from "./lib/Login.svelte";
  import UsersTable from "./lib/UsersTable.svelte";

  let status = $state("loading"); // loading | login | admin | denied | error
  let me = $state(null);
  let error = $state("");

  async function checkSession() {
    error = "";
    if (!getToken()) {
      status = "login";
      return;
    }

    status = "loading";
    try {
      const user = await apiFetch("/auth/me");
      me = user;
      status = user?.is_admin === true ? "admin" : "denied";
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        // Stale / invalid token — drop it and fall back to the login form.
        clearToken();
        status = "login";
        return;
      }
      error = err.message || "Could not verify your session.";
      status = "error";
    }
  }

  async function signOut() {
    try {
      // Best-effort server-side session revocation; mirror the frontend's logout.
      await apiFetch("/auth/logout", { method: "POST" });
    } catch (err) {
      // Still clear locally even if the server call fails, but surface a
      // persistent server-side revocation failure so it is observable.
      console.warn("admin logout call failed; clearing local token anyway", err);
    }
    clearToken();
    me = null;
    status = "login";
  }

  $effect(() => {
    checkSession();
  });
</script>

{#if status === "loading"}
  <div class="min-h-dvh flex items-center justify-center gap-2">
    <span class="loading loading-spinner loading-lg"></span>
  </div>
{:else if status === "login"}
  <Login onAuthenticated={checkSession} />
{:else if status === "admin"}
  <div class="min-h-dvh flex flex-col">
    <header class="navbar bg-base-200 border-b border-base-300 px-4">
      <div class="flex-1">
        <span class="text-lg font-semibold">Nels Admin</span>
      </div>
      <div class="flex-none flex items-center gap-3">
        {#if me?.email}<span class="text-sm opacity-70 hidden sm:inline">{me.email}</span>{/if}
        <button class="btn btn-sm btn-ghost" onclick={signOut}>Sign out</button>
      </div>
    </header>
    <main class="flex-1">
      <UsersTable />
    </main>
  </div>
{:else if status === "denied"}
  <div class="min-h-dvh flex items-center justify-center p-4">
    <div class="card w-full max-w-sm bg-base-200 shadow-xl">
      <div class="card-body items-center text-center">
        <h1 class="card-title">Access denied</h1>
        <p class="opacity-70">This account is not an administrator.</p>
        <button class="btn btn-primary mt-2" onclick={signOut}>Sign out</button>
      </div>
    </div>
  </div>
{:else}
  <div class="min-h-dvh flex items-center justify-center p-4">
    <div class="card w-full max-w-sm bg-base-200 shadow-xl">
      <div class="card-body items-center text-center">
        <h1 class="card-title">Something went wrong</h1>
        <p class="opacity-70">{error}</p>
        <div class="flex gap-2 mt-2">
          <button class="btn btn-primary" onclick={checkSession}>Retry</button>
          <button class="btn btn-ghost" onclick={signOut}>Sign out</button>
        </div>
      </div>
    </div>
  </div>
{/if}
