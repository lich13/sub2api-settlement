use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
    pub detail: Option<serde_json::Value>,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            detail: None,
        }
    }
    pub fn validation(detail: serde_json::Value) -> Self {
        Self {
            status: 422,
            message: "请求参数无效".into(),
            detail: Some(detail),
        }
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(422, message)
    }
    pub fn unavailable() -> Self {
        Self::new(503, "操作未完成，请刷新后重试")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Error {}
impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::unavailable()
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::unavailable()
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::unavailable()
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut response = (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            Json(json!({"detail": self.detail.unwrap_or(serde_json::Value::String(self.message))})),
        )
            .into_response();
        if self.status == 429 {
            response
                .headers_mut()
                .insert("Retry-After", axum::http::HeaderValue::from_static("60"));
        }
        response
    }
}
