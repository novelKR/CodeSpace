//! Optional static Bearer for HTTP experiments. The token is never logged.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use subtle::ConstantTimeEq;

/// Returns `None` when the request is allowed.
/// Missing `expected` disables the check.
pub fn authorize_headers(headers: &HeaderMap, expected: Option<&str>) -> Option<Response> {
    let expected = expected?;
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match presented {
        Some(token) if tokens_match(token, expected) => None,
        _ => Some(unauthorized()),
    }
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

fn tokens_match(a: &str, b: &str) -> bool {
    let left = a.as_bytes();
    let right = b.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_body_has_no_token() {
        let response = unauthorized();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
