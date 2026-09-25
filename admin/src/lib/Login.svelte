<script>
  // Passkey login for the admin console (nels#551):
  //   1. POST /auth/login/start { email } -> { flow_id, options }
  //   2. navigator.credentials.get() via startAuthentication (browser prompt)
  //   3. POST /auth/login/finish { flow_id, credential } -> { token, user }
  // On success the token is stored under "nels_admin_token" and the parent is
  // signalled via the `onAuthenticated` callback to re-check the admin gate.
  //
  // The admin console (nels-admin.pages.dev) is a different WebAuthn relying
  // party than the main app (app.nels.money) — see backend/src/passkeys.rs —
  // so a passkey enrolled from the main app does NOT work here. An admin's
  // first passkey on THIS origin is enrolled the same way any lost-device
  // recovery works: redeem one of the recovery codes already issued when
  // their account was created.
  import { startAuthentication, startRegistration } from "@simplewebauthn/browser";
  import { apiFetch, setToken } from "./api.js";

  let { onAuthenticated } = $props();

  let mode = $state("login"); // "login" | "recover"
  let email = $state("");
  let recoveryCode = $state("");
  let loading = $state(false);
  let error = $state("");

  async function login(e) {
    e.preventDefault();
    error = "";
    const trimmed = email.trim();
    if (!trimmed) {
      error = "Enter your email address.";
      return;
    }

    loading = true;
    try {
      const start = await apiFetch("/auth/login/start", {
        method: "POST",
        body: JSON.stringify({ email: trimmed }),
      });
      const credential = await startAuthentication({ optionsJSON: start.options });
      const finish = await apiFetch("/auth/login/finish", {
        method: "POST",
        body: JSON.stringify({ flow_id: start.flow_id, credential }),
      });
      setToken(finish.token);
      onAuthenticated?.();
    } catch (err) {
      if (err.status === 404) {
        error = "No passkey registered for the admin console on this device.";
        mode = "recover";
      } else {
        error = err.message || "Could not sign in. Try again.";
      }
    } finally {
      loading = false;
    }
  }

  async function enrollWithRecoveryCode(e) {
    e.preventDefault();
    error = "";
    const trimmedEmail = email.trim();
    const trimmedCode = recoveryCode.trim();
    if (!trimmedEmail || !trimmedCode) {
      error = "Enter your email and a recovery code.";
      return;
    }

    loading = true;
    try {
      const start = await apiFetch("/auth/recovery/start", {
        method: "POST",
        body: JSON.stringify({ email: trimmedEmail, recovery_code: trimmedCode }),
      });
      const credential = await startRegistration({ optionsJSON: start.options });
      const finish = await apiFetch("/auth/recovery/finish", {
        method: "POST",
        body: JSON.stringify({ flow_id: start.flow_id, credential }),
      });
      setToken(finish.token);
      onAuthenticated?.();
    } catch (err) {
      error = err.message || "Could not verify that recovery code. Try again.";
    } finally {
      loading = false;
    }
  }

  function backToLogin() {
    mode = "login";
    recoveryCode = "";
    error = "";
  }
</script>

<div class="min-h-dvh flex items-center justify-center p-4">
  <div class="card w-full max-w-sm bg-base-200 shadow-xl">
    <div class="card-body">
      <h1 class="card-title text-2xl">Nels Admin</h1>
      <p class="text-sm opacity-70">
        {mode === "login" ? "Sign in with your passkey." : "Enroll a passkey for this device using a recovery code."}
      </p>

      {#if error}
        <div class="alert alert-error text-sm" role="alert">{error}</div>
      {/if}

      {#if mode === "login"}
        <form onsubmit={login} class="flex flex-col gap-3 mt-2">
          <label class="form-control w-full">
            <span class="label-text mb-1">Email</span>
            <input
              type="email"
              class="input input-bordered w-full"
              placeholder="you@example.com"
              bind:value={email}
              autocomplete="email"
              disabled={loading}
            />
          </label>
          <button type="submit" class="btn btn-primary" disabled={loading}>
            {#if loading}<span class="loading loading-spinner loading-sm"></span>{/if}
            Sign in with passkey
          </button>
          <button type="button" class="btn btn-ghost btn-sm" onclick={() => (mode = "recover")} disabled={loading}>
            Enroll a passkey with a recovery code
          </button>
        </form>
      {:else}
        <form onsubmit={enrollWithRecoveryCode} class="flex flex-col gap-3 mt-2">
          <label class="form-control w-full">
            <span class="label-text mb-1">Email</span>
            <input
              type="email"
              class="input input-bordered w-full"
              placeholder="you@example.com"
              bind:value={email}
              autocomplete="email"
              disabled={loading}
            />
          </label>
          <label class="form-control w-full">
            <span class="label-text mb-1">Recovery code</span>
            <input
              type="text"
              class="input input-bordered w-full font-mono"
              placeholder="ABCDE-FGHJK-MNPQR"
              bind:value={recoveryCode}
              autocomplete="off"
              disabled={loading}
            />
          </label>
          <button type="submit" class="btn btn-primary" disabled={loading}>
            {#if loading}<span class="loading loading-spinner loading-sm"></span>{/if}
            Enroll new passkey
          </button>
          <button type="button" class="btn btn-ghost btn-sm" onclick={backToLogin} disabled={loading}>
            Back to sign in
          </button>
        </form>
      {/if}
    </div>
  </div>
</div>
