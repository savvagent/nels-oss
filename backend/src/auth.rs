use axum::{
    extract::{State, Extension},
    http::{StatusCode, header, HeaderMap},
    middleware::Next,
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use std::sync::Arc;
use webauthn_rs::prelude::*;
use webauthn_rs_proto::{PublicKeyCredentialCreationOptions, PublicKeyCredentialRequestOptions};

use crate::db::User;
use crate::error::internal_error;
use crate::passkeys::{self, WebauthnRegistry};

// How long a login session stays valid, and how long a half-finished
// register/login/recovery flow waits for the second step. Expressed as SQL
// intervals so expiry is computed by the database (single source of time
// truth).
const SESSION_TTL: &str = "30 days";
pub(crate) const FLOW_TTL: &str = "10 minutes";

// AppState definition. All auth state lives in the database (tables
// `sessions` and `auth_flows`), so a backend restart no longer logs users out
// or kills in-progress register/login/recovery flows.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cipher: Arc<crate::crypto::SecretCipher>,
    pub webauthn: Arc<WebauthnRegistry>,
}

#[derive(Deserialize)]
pub struct RegisterStartRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct RegisterStartResponse {
    pub flow_id: String,
    pub options: PublicKeyCredentialCreationOptions,
}

#[derive(Deserialize)]
pub struct RegisterFinishRequest {
    pub flow_id: String,
    pub credential: RegisterPublicKeyCredential,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: UserResponse,
    // Present only on register/recovery-finish, and only that one time —
    // these are shown to the user once and never retrievable again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_codes: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
}

// Dedicated response for `GET /api/auth/me`. Kept separate from `UserResponse`
// so the `is_admin` flag (Admin PWA, #140) and the retirement-planner feature
// flag are exposed only here and do not leak into the login/register auth
// responses, which reuse `UserResponse`.
#[derive(Serialize)]
pub struct MeResponse {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub is_admin: bool,
    // Runtime feature flag for the #469 retirement planner dashboard. Read from
    // `RETIREMENT_PLANNER_ENABLED` at request time (see
    // `retirement_planner_enabled`), so it can be flipped on/off in production
    // without a redeploy. Defaults OFF (planner hidden) — the frontend consults
    // this to decide whether to show the planner UI at all.
    pub retirement_planner_enabled: bool,
}

impl From<crate::db::User> for MeResponse {
    // Single mapping site so the `me` handler cannot drift as fields are added.
    // The feature flag is config-derived (not a `users` column), but it is part
    // of the response contract every client reads, so it is populated here
    // rather than letting the handler assemble the struct by hand.
    fn from(user: crate::db::User) -> Self {
        MeResponse {
            id: user.id,
            email: user.email,
            name: user.name,
            is_admin: user.is_admin,
            retirement_planner_enabled: retirement_planner_enabled(),
        }
    }
}

/// Pure parse of the retirement-planner feature flag value (no env access) so
/// it is unit-testable without mutating process-global state (which would race
/// concurrent tests). Mirrors `access::enforcement_flag_enabled`.
fn retirement_flag_enabled(val: Option<&str>) -> bool {
    matches!(val, Some(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// True iff the retirement planner dashboard is enabled
/// (`RETIREMENT_PLANNER_ENABLED` = "1"/"true"). Default OFF so the planner stays
/// hidden until a deployment deliberately opts in — see §25's blocking AC.
pub(crate) fn retirement_planner_enabled() -> bool {
    retirement_flag_enabled(std::env::var("RETIREMENT_PLANNER_ENABLED").ok().as_deref())
}

#[derive(Deserialize)]
pub struct LoginStartRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct LoginStartResponse {
    pub flow_id: String,
    pub options: PublicKeyCredentialRequestOptions,
}

#[derive(Deserialize)]
pub struct LoginFinishRequest {
    pub flow_id: String,
    pub credential: PublicKeyCredential,
}

#[derive(Deserialize)]
pub struct RecoveryStartRequest {
    pub email: String,
    pub recovery_code: String,
}

#[derive(Serialize)]
pub struct RecoveryStartResponse {
    pub flow_id: String,
    pub options: PublicKeyCredentialCreationOptions,
}

#[derive(Deserialize)]
pub struct RecoveryFinishRequest {
    pub flow_id: String,
    pub credential: RegisterPublicKeyCredential,
}

// Create a persistent session row and return its bearer token.
async fn create_session(db: &PgPool, user_id: Uuid) -> Result<String, (StatusCode, String)> {
    let token = Uuid::new_v4().to_string();
    sqlx::query(&format!(
        "INSERT INTO sessions (token, user_id, expires_at) VALUES ($1, $2, NOW() + INTERVAL '{SESSION_TTL}')"
    ))
    .bind(&token)
    .bind(user_id)
    .execute(db)
    .await
    .map_err(|e| internal_error(format!("Failed to create session: {}", e)))?;
    Ok(token)
}

// Handlers
pub async fn register_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, (StatusCode, String)> {
    let email = payload.email.trim().to_lowercase();
    if email.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Email cannot be empty".to_string()));
    }
    let rp = state.webauthn.resolve_rp(&headers)?;

    let existing_user = sqlx::query("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;

    if existing_user.is_some() {
        return Err((StatusCode::CONFLICT, "User already registered with this email. Please log in.".to_string()));
    }

    let user_id = Uuid::new_v4();
    let (ccr, reg_state) = state
        .webauthn
        .get(rp)
        .start_passkey_registration(user_id, &email, &email, None)
        .map_err(|e| internal_error(format!("Failed to start passkey registration: {e}")))?;

    let ceremony_state = serde_json::to_value(&reg_state)
        .map_err(|e| internal_error(format!("Failed to serialize registration state: {e}")))?;

    // Persist the in-progress registration challenge so it survives a restart.
    let flow_id = Uuid::new_v4().to_string();
    sqlx::query(&format!(
        "INSERT INTO auth_flows (flow_id, flow_type, user_id, email, rp, ceremony_state, expires_at) \
         VALUES ($1, 'register', $2, $3, $4, $5, NOW() + INTERVAL '{FLOW_TTL}')"
    ))
    .bind(&flow_id)
    .bind(user_id)
    .bind(&email)
    .bind(rp)
    .bind(&ceremony_state)
    .execute(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to store registration flow: {}", e)))?;

    Ok(Json(RegisterStartResponse { flow_id, options: ccr.public_key }))
}

pub async fn register_finish(
    State(state): State<AppState>,
    Json(payload): Json<RegisterFinishRequest>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    // Retrieve the registration challenge (left in place until it verifies, so
    // a failed ceremony can simply be retried).
    let challenge: Option<(Uuid, String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT user_id, email, rp, ceremony_state FROM auth_flows \
         WHERE flow_id = $1 AND flow_type = 'register' AND expires_at > NOW()"
    )
    .bind(&payload.flow_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;

    let (user_id, email, rp, ceremony_state) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };
    let rp = passkeys::to_rp(&rp);

    let reg_state: PasskeyRegistration = serde_json::from_value(ceremony_state)
        .map_err(|e| internal_error(format!("Corrupt registration state: {e}")))?;

    let passkey = state
        .webauthn
        .get(rp)
        .finish_passkey_registration(&payload.credential, &reg_state)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Passkey registration failed: {e}")))?;

    // Create user
    let mut tx = state.db.begin().await
        .map_err(internal_error)?;

    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email) VALUES ($1, $2) RETURNING *"
    )
    .bind(user_id)
    .bind(&email)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| internal_error(format!("Failed to create user: {}", e)))?;

    tx.commit().await
        .map_err(internal_error)?;

    passkeys::store_new_credential(&state.db, user_id, rp, &passkey).await?;
    let recovery_codes = passkeys::issue_recovery_codes(&state.db, user_id).await?;

    // Consume the registration challenge now that it has succeeded.
    let _ = sqlx::query("DELETE FROM auth_flows WHERE flow_id = $1")
        .bind(&payload.flow_id)
        .execute(&state.db)
        .await;

    // Create a persistent session
    let session_token = create_session(&state.db, user_id).await?;

    Ok(Json(AuthResponse {
        token: session_token,
        user: UserResponse { id: user.id, email: user.email, name: user.name },
        recovery_codes: Some(recovery_codes),
    }))
}

pub async fn login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, (StatusCode, String)> {
    let email = payload.email.trim().to_lowercase();
    if email.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Email cannot be empty".to_string()));
    }
    let rp = state.webauthn.resolve_rp(&headers)?;

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;

    let user = match user {
        Some(u) => u,
        None => return Err((StatusCode::NOT_FOUND, "User not found. Please register first.".to_string())),
    };

    let creds = passkeys::load_passkeys(&state.db, user.id, rp).await?;
    if creds.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            "No passkey registered for this app on this device. Use a recovery code to enroll a new one.".to_string(),
        ));
    }

    let (rcr, auth_state) = state
        .webauthn
        .get(rp)
        .start_passkey_authentication(&creds)
        .map_err(|e| internal_error(format!("Failed to start passkey authentication: {e}")))?;

    let ceremony_state = serde_json::to_value(&auth_state)
        .map_err(|e| internal_error(format!("Failed to serialize authentication state: {e}")))?;

    // Persist the in-progress login challenge so it survives a restart.
    let flow_id = Uuid::new_v4().to_string();
    sqlx::query(&format!(
        "INSERT INTO auth_flows (flow_id, flow_type, user_id, email, rp, ceremony_state, expires_at) \
         VALUES ($1, 'login', $2, $3, $4, $5, NOW() + INTERVAL '{FLOW_TTL}')"
    ))
    .bind(&flow_id)
    .bind(user.id)
    .bind(&user.email)
    .bind(rp)
    .bind(&ceremony_state)
    .execute(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to store login flow: {}", e)))?;

    Ok(Json(LoginStartResponse { flow_id, options: rcr.public_key }))
}

pub async fn login_finish(
    State(state): State<AppState>,
    Json(payload): Json<LoginFinishRequest>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    // Retrieve the login challenge (left in place until it verifies).
    let challenge: Option<(Uuid, String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT user_id, email, rp, ceremony_state FROM auth_flows \
         WHERE flow_id = $1 AND flow_type = 'login' AND expires_at > NOW()"
    )
    .bind(&payload.flow_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;

    let (user_id, email, rp, ceremony_state) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };
    let rp = passkeys::to_rp(&rp);

    let auth_state: PasskeyAuthentication = serde_json::from_value(ceremony_state)
        .map_err(|e| internal_error(format!("Corrupt authentication state: {e}")))?;

    let auth_result = state
        .webauthn
        .get(rp)
        .finish_passkey_authentication(&payload.credential, &auth_state)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Passkey authentication failed: {e}")))?;

    // Persist an updated sign-count/backup-state, if any, for clone/replay
    // detection on the next login. A miss here is logged, never fatal — the
    // assertion above already verified.
    let mut creds = passkeys::load_passkeys(&state.db, user_id, rp).await?;
    for pk in &mut creds {
        if pk.update_credential(&auth_result).is_some() {
            passkeys::update_credential_after_auth(&state.db, rp, pk).await;
        }
    }

    // Consume the login challenge now that it has succeeded.
    let _ = sqlx::query("DELETE FROM auth_flows WHERE flow_id = $1")
        .bind(&payload.flow_id)
        .execute(&state.db)
        .await;

    // Create a persistent session
    let session_token = create_session(&state.db, user_id).await?;

    // Surface the stored name so returning users are greeted by name immediately.
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    Ok(Json(AuthResponse {
        token: session_token,
        user: UserResponse { id: user_id, email, name },
        recovery_codes: None,
    }))
}

// Redeem a one-time recovery code to enroll a fresh passkey when a user has
// no surviving credential for this RP — device loss, admin-assisted reset
// re-entry, or bootstrapping a passkey on the admin console's RP for the
// first time using a code already issued on the app's RP.
pub async fn recovery_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RecoveryStartRequest>,
) -> Result<Json<RecoveryStartResponse>, (StatusCode, String)> {
    let email = payload.email.trim().to_lowercase();
    let rp = state.webauthn.resolve_rp(&headers)?;

    let found = passkeys::find_unused_code(&state.db, &email, &payload.recovery_code).await?;
    let (code_id, user_id) = match found {
        Some(v) => v,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or already-used recovery code.".to_string())),
    };

    // Recovery codes are precious — don't let a working login be bypassed
    // with one just to add a spare device.
    if passkeys::has_any_credential(&state.db, user_id, rp).await? {
        return Err((
            StatusCode::CONFLICT,
            "This account already has a passkey for this app — sign in normally instead.".to_string(),
        ));
    }

    let (ccr, reg_state) = state
        .webauthn
        .get(rp)
        .start_passkey_registration(user_id, &email, &email, None)
        .map_err(|e| internal_error(format!("Failed to start passkey registration: {e}")))?;

    let ceremony_state = serde_json::to_value(&reg_state)
        .map_err(|e| internal_error(format!("Failed to serialize registration state: {e}")))?;

    let flow_id = Uuid::new_v4().to_string();
    sqlx::query(&format!(
        "INSERT INTO auth_flows (flow_id, flow_type, user_id, email, rp, ceremony_state, recovery_code_id, expires_at) \
         VALUES ($1, 'recovery', $2, $3, $4, $5, $6, NOW() + INTERVAL '{FLOW_TTL}')"
    ))
    .bind(&flow_id)
    .bind(user_id)
    .bind(&email)
    .bind(rp)
    .bind(&ceremony_state)
    .bind(code_id)
    .execute(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to store recovery flow: {}", e)))?;

    Ok(Json(RecoveryStartResponse { flow_id, options: ccr.public_key }))
}

pub async fn recovery_finish(
    State(state): State<AppState>,
    Json(payload): Json<RecoveryFinishRequest>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    let challenge: Option<(Uuid, String, String, serde_json::Value, Option<Uuid>)> = sqlx::query_as(
        "SELECT user_id, email, rp, ceremony_state, recovery_code_id FROM auth_flows \
         WHERE flow_id = $1 AND flow_type = 'recovery' AND expires_at > NOW()"
    )
    .bind(&payload.flow_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;

    let (user_id, email, rp, ceremony_state, recovery_code_id) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };
    let rp = passkeys::to_rp(&rp);
    let recovery_code_id = recovery_code_id
        .ok_or_else(|| internal_error("recovery flow missing recovery_code_id".to_string()))?;

    let reg_state: PasskeyRegistration = serde_json::from_value(ceremony_state)
        .map_err(|e| internal_error(format!("Corrupt registration state: {e}")))?;

    let passkey = state
        .webauthn
        .get(rp)
        .finish_passkey_registration(&payload.credential, &reg_state)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Passkey enrollment failed: {e}")))?;

    passkeys::store_new_credential(&state.db, user_id, rp, &passkey).await?;
    passkeys::mark_code_used(&state.db, recovery_code_id).await?;

    let _ = sqlx::query("DELETE FROM auth_flows WHERE flow_id = $1")
        .bind(&payload.flow_id)
        .execute(&state.db)
        .await;

    let session_token = create_session(&state.db, user_id).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    Ok(Json(AuthResponse {
        token: session_token,
        user: UserResponse { id: user_id, email, name },
        recovery_codes: None,
    }))
}

// Auth middleware check
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Extract the bearer token as an owned String so the request is no longer
    // borrowed when we later insert the resolved user id into its extensions.
    let token = {
        let auth_header = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());

        match auth_header {
            Some(header_val) if header_val.starts_with("Bearer ") => header_val[7..].to_string(),
            _ => return Err(StatusCode::UNAUTHORIZED),
        }
    };

    // Look up a non-expired session in the database.
    let user_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM sessions WHERE token = $1 AND expires_at > NOW()"
    )
    .bind(&token)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user_id = match user_id {
        Some(id) => id,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    // Insert the bare Uuid so handlers can read it via the `Extension<Uuid>`
    // extractor (the extractor looks up the inner type, not `Extension<Uuid>`).
    request.extensions_mut().insert(user_id);

    let response = next.run(request).await;
    Ok(response)
}

// Me endpoint
pub async fn me(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<MeResponse>, StatusCode> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(user.into()))
}

// Logout endpoint
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    let token = match auth_header {
        Some(header_val) if header_val.starts_with("Bearer ") => &header_val[7..],
        _ => return Ok(StatusCode::OK),
    };

    let _ = sqlx::query("DELETE FROM sessions WHERE token = $1")
        .bind(token)
        .execute(&state.db)
        .await;

    Ok(StatusCode::OK)
}

// Periodically delete expired sessions and abandoned auth flows. Expiry is
// already enforced at query time; this just stops the tables growing forever.
pub async fn purge_expired_auth_state(db: &PgPool) {
    let _ = sqlx::query("DELETE FROM sessions WHERE expires_at <= NOW()")
        .execute(db)
        .await;
    let _ = sqlx::query("DELETE FROM auth_flows WHERE expires_at <= NOW()")
        .execute(db)
        .await;
}

#[cfg(test)]
pub(crate) fn test_cipher() -> crate::crypto::SecretCipher {
    crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    // @simplewebauthn/browser's startRegistration/startAuthentication take the
    // bare options JSON (`challenge`, `rp`, ... at the top level). Wrapping it
    // in webauthn-rs's `{ "publicKey": ... }` envelope crashed every ceremony
    // client-side with "Cannot read properties of undefined (reading 'replace')".
    #[test]
    fn start_response_options_are_unwrapped_for_simplewebauthn() {
        let registry = WebauthnRegistry::for_test();
        let (ccr, _) = registry
            .get("app")
            .start_passkey_registration(Uuid::new_v4(), "a@example.com", "a@example.com", None)
            .unwrap();
        let json = serde_json::to_value(RegisterStartResponse {
            flow_id: "f".to_string(),
            options: ccr.public_key,
        })
        .unwrap();
        let options = &json["options"];
        assert!(options.get("publicKey").is_none(), "options must not be wrapped: {options}");
        assert!(options["challenge"].is_string(), "challenge must be top-level: {options}");
        assert_eq!(options["rp"]["id"], "localhost");
    }

    // The single `From<db::User> for MeResponse` mapping is the response contract
    // every client reads.
    #[test]
    fn me_response_from_user_maps_each_field() {
        let user = crate::db::User {
            id: uuid::Uuid::nil(),
            email: "user@example.com".to_string(),
            name: Some("Ada".to_string()),
            created_at: chrono::Utc::now(),
            is_admin: true,
            active_budget_id: None,
        };
        let me: MeResponse = user.into();
        assert_eq!(me.email, "user@example.com");
        assert_eq!(me.name.as_deref(), Some("Ada"));
        assert!(me.is_admin);
        // The planner flag is env-driven; no test in this suite sets the env var
        // (see `retirement_flag_enabled` tests, which stay pure), so it must
        // default to OFF here.
        assert!(!me.retirement_planner_enabled);
    }

    // The flag parse is the only contract between the env var spelling and the
    // response. "1" and case-insensitive "true" enable; anything else (unset,
    // empty, "0", "yes") leaves the planner hidden.
    #[test]
    fn retirement_flag_enabled_parses_like_the_other_feature_flags() {
        assert!(retirement_flag_enabled(Some("1")));
        assert!(retirement_flag_enabled(Some("true")));
        assert!(retirement_flag_enabled(Some("TRUE")));
        assert!(retirement_flag_enabled(Some("True")));
        assert!(!retirement_flag_enabled(Some("0")));
        assert!(!retirement_flag_enabled(Some("yes")));
        assert!(!retirement_flag_enabled(Some("")));
        assert!(!retirement_flag_enabled(Some("false")));
        assert!(!retirement_flag_enabled(None));
    }

    // `me` must return the new `is_admin` flag (Admin PWA, #140), and it must
    // default to false for a freshly-seeded user. Requires Postgres:
    //   podman-compose up -d && cd backend && cargo test -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn me_returns_is_admin_defaulting_to_false() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = AppState {
            db: pool.clone(),
            cipher: Arc::new(test_cipher()),
            webauthn: Arc::new(WebauthnRegistry::for_test()),
        };

        let user_id = Uuid::new_v4();
        let email = format!("me-isadmin-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed user");

        let resp = me(State(state.clone()), Extension(user_id))
            .await
            .expect("me should succeed");
        assert_eq!(resp.0.id, user_id);
        assert_eq!(resp.0.email, email);
        assert!(!resp.0.is_admin, "freshly-seeded user must default to is_admin=false");
        assert!(
            !resp.0.retirement_planner_enabled,
            "planner flag must default OFF in the test process (env var unset)"
        );

        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
    }

    fn app_origin_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, "http://localhost:5173".parse().unwrap());
        h
    }

    fn test_state(pool: PgPool) -> AppState {
        AppState {
            db: pool,
            cipher: Arc::new(test_cipher()),
            webauthn: Arc::new(WebauthnRegistry::for_test()),
        }
    }

    // register_start must not let a second registration claim an email
    // already bound to an account, regardless of which RP is registering.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn register_start_rejects_existing_email() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let user_id = Uuid::new_v4();
        let email = format!("register-conflict-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed user");

        let resp = register_start(
            State(state),
            app_origin_headers(),
            Json(RegisterStartRequest { email: email.clone() }),
        )
        .await;
        match resp {
            Err((status, _)) => assert_eq!(status, StatusCode::CONFLICT),
            Ok(_) => panic!("expected 409 Conflict, got Ok"),
        }

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }

    // register_start must persist a resumable ceremony (ceremony_state) tagged
    // with the resolved RP, mirroring the old TOTP-era auth_flows pattern —
    // a backend restart between /start and /finish must not lose the flow.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn register_start_persists_a_resumable_ceremony() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let email = format!("register-flow-{}@example.test", Uuid::new_v4());
        let resp = register_start(
            State(state),
            app_origin_headers(),
            Json(RegisterStartRequest { email: email.clone() }),
        )
        .await
        .expect("register_start should succeed");

        let row: (String, Uuid, serde_json::Value) = sqlx::query_as(
            "SELECT flow_type, user_id, ceremony_state FROM auth_flows WHERE flow_id = $1",
        )
        .bind(&resp.0.flow_id)
        .fetch_one(&pool)
        .await
        .expect("flow row must exist");
        assert_eq!(row.0, "register");
        assert!(!row.2.is_null() && row.2 != serde_json::json!({}), "ceremony state must be persisted");

        let _ = sqlx::query("DELETE FROM auth_flows WHERE flow_id = $1")
            .bind(&resp.0.flow_id)
            .execute(&pool)
            .await;
    }

    // login_start must fail fast with 404 (not start an empty ceremony) when
    // the account has no credential for the resolved RP — this is the signal
    // the frontend uses to route the user to the recovery-code flow instead.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn login_start_fails_fast_when_no_credential_for_this_rp() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let user_id = Uuid::new_v4();
        let email = format!("login-no-cred-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed user");

        let resp = login_start(
            State(state),
            app_origin_headers(),
            Json(LoginStartRequest { email: email.clone() }),
        )
        .await;
        match resp {
            Err((status, _)) => assert_eq!(status, StatusCode::NOT_FOUND),
            Ok(_) => panic!("expected 404 Not Found, got Ok"),
        }

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }
}
