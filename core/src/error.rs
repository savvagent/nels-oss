//! The error type for domain guards (spec savvagent/nels-oss#5, A6).
//!
//! Core cannot depend on axum/http, so a guard reports a KIND and a message.
//! The backend maps the kind to a status in exactly one place
//! (`budget::rule_status`). The message is shown to the user verbatim.

/// Which class of rejection this is. The backend maps `BadRequest` to 400 and
/// `Conflict` to 409; these are the only two statuses the moved guards produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleErrorKind {
    BadRequest,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError {
    pub kind: RuleErrorKind,
    pub message: String,
}

impl RuleError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self { kind: RuleErrorKind::BadRequest, message: message.into() }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self { kind: RuleErrorKind::Conflict, message: message.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_kind_and_keep_message_verbatim() {
        let e = RuleError::bad_request("Invalid budget_type");
        assert_eq!(e.kind, RuleErrorKind::BadRequest);
        assert_eq!(e.message, "Invalid budget_type");
        let e = RuleError::conflict(String::from("already linked"));
        assert_eq!(e.kind, RuleErrorKind::Conflict);
        assert_eq!(e.message, "already linked");
    }
}
