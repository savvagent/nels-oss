// Tiny fetch helper for the admin PWA.
//
// Production builds inject VITE_API_BASE (e.g. https://nels-api.fly.dev/api);
// local dev falls back to the backend on localhost.
const API_BASE = import.meta.env.VITE_API_BASE ?? "http://localhost:3000/api";

// Distinct from the frontend's `nels_token` so the two apps don't clobber each
// other's session when they happen to share an origin in dev.
const TOKEN_KEY = "nels_admin_token";

export function getToken() {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(token) {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken() {
  localStorage.removeItem(TOKEN_KEY);
}

// Thrown on any non-2xx response. Carries `status` so callers can branch on
// 401 (stale token) vs 403 (authenticated but not an admin) vs everything else.
export class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

// Fetch `endpoint` (path under API_BASE, e.g. "/admin/users"). Attaches the
// bearer token when present, JSON-encodes a body when given, and throws an
// ApiError carrying the HTTP status on any non-2xx response.
export async function apiFetch(endpoint, options = {}) {
  const headers = {
    "Content-Type": "application/json",
    ...options.headers,
  };

  const token = getToken();
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }

  const res = await fetch(`${API_BASE}${endpoint}`, { ...options, headers });

  if (!res.ok) {
    let detail = "";
    try {
      detail = await res.text();
    } catch {
      // ignore body read failures; status alone is enough to branch on.
    }
    throw new ApiError(detail || `Request failed (${res.status})`, res.status);
  }

  if (res.status === 204) return null;
  return await res.json();
}
