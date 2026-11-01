use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// API failure. Messages name fields and limits only; they never carry setting values, URLs or
/// secrets, so any message is safe to return and log.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_import_list", message)
    }
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "import_list_not_found",
            "Import list does not exist",
        )
    }
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
    pub fn key_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_list_key_unavailable",
            "Import list secrets require the independently stored provider key",
        )
    }
    pub fn locked() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_list_credentials_locked",
            "Import list credentials cannot be unlocked; check the independently stored key",
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}
impl From<libsql::Error> for ApiError {
    fn from(error: libsql::Error) -> Self {
        // Only the variant is logged: driver text can embed SQL values.
        eprintln!(
            "event=import_list_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_list_storage_error",
            "Import list storage is unavailable; nothing was changed",
        )
    }
}
