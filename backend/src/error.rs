//! Helpers for returning generic error responses while logging the real cause.
//!
//! Returning raw sqlx/Postgres error text to clients leaks schema, column, and
//! constraint names (CWE-209). These helpers log the underlying error
//! server-side via `tracing` and hand the client a generic message instead.

use axum::http::StatusCode;
use std::fmt::Display;

/// Generic body returned to clients for any internal, server-side failure.
pub const INTERNAL_ERROR_MESSAGE: &str = "Internal server error";

/// Log `err` server-side and return a generic `500` response. The underlying
/// error text is never sent to the client.
///
/// Use at handler boundaries that return `(StatusCode, String)`:
///
/// ```ignore
/// sqlx::query(..).execute(db).await.map_err(internal_error)?;
/// ```
///
/// Pass a `format!` string to record extra context in the log while still
/// returning the generic body to the client:
///
/// ```ignore
/// .map_err(|e| internal_error(format!("Failed to create session: {e}")))?;
/// ```
pub fn internal_error<E: Display>(err: E) -> (StatusCode, String) {
    tracing::error!(error = %err, "internal server error");
    (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE.to_string())
}

/// Log `err` server-side and return the generic message as a bare `String`.
///
/// For helper functions that propagate `Result<_, String>` up to a handler:
/// genericizing at the source guarantees the detailed error can never reach a
/// response body, regardless of how the caller wraps it.
pub fn internal_error_message<E: Display>(err: E) -> String {
    tracing::error!(error = %err, "internal server error");
    INTERNAL_ERROR_MESSAGE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A stand-in for a raw database error that would leak schema details.
    const LEAKY_DETAIL: &str =
        "error returned from database: column \"budget_limit\" of relation \"budgets\"";

    #[test]
    fn internal_error_returns_500_with_generic_body() {
        let (status, body) = internal_error(LEAKY_DETAIL);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, INTERNAL_ERROR_MESSAGE);
    }

    #[test]
    fn internal_error_does_not_leak_underlying_detail() {
        let (_, body) = internal_error(LEAKY_DETAIL);
        assert!(!body.contains("budget_limit"));
        assert!(!body.contains("relation"));
        assert!(!body.contains("column"));
    }

    #[test]
    fn internal_error_preserves_context_in_returned_body_only_as_generic() {
        // Even when caller adds context, the client-facing body stays generic.
        let (_, body) = internal_error(format!("Failed to create session: {LEAKY_DETAIL}"));
        assert_eq!(body, INTERNAL_ERROR_MESSAGE);
    }

    #[test]
    fn internal_error_message_returns_generic_string() {
        let body = internal_error_message(LEAKY_DETAIL);
        assert_eq!(body, INTERNAL_ERROR_MESSAGE);
        assert!(!body.contains("budget_limit"));
    }
}
