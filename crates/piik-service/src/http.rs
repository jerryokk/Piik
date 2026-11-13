//! Request reading and JSON responses shared by every route.

use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use serde_json::{Value, json};

/// Bounds the body every JSON route accepts.
pub(crate) const MAX_JSON_REQUEST_BYTES: usize = 1024;

/// A JSON response with its exact length and the JSON charset. Responses are
/// never embedded into HTML, so nothing is HTML-escaped.
pub(crate) fn json_response(status: StatusCode, body: Value) -> Response<Body> {
    json_bytes(status, body.to_string().into_bytes())
}

pub(crate) fn json_bytes(status: StatusCode, encoded: Vec<u8>) -> Response<Body> {
    let length = encoded.len();
    let mut response = Response::new(Body::from(encoded));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// The `{ error }` shape every failing route answers with.
pub(crate) fn error_body(message: &str) -> Value {
    json!({ "error": message })
}

pub(crate) fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    json_response(status, error_body(message))
}

pub(crate) fn not_found_body() -> Value {
    error_body("Not found")
}

/// Requires `application/json` once parameters are removed and a nonempty body
/// of at most [`MAX_JSON_REQUEST_BYTES`]. `timeout` bounds a slow sender.
/// Every failure is the route's 400.
pub(crate) async fn read_json_body(request: Request<Body>, timeout: Duration) -> Option<Vec<u8>> {
    let content_type = header_text(request.headers(), header::CONTENT_TYPE);
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if media_type != "application/json" {
        return None;
    }
    let body = request.into_body();
    let bytes = tokio::time::timeout(timeout, axum::body::to_bytes(body, MAX_JSON_REQUEST_BYTES))
        .await
        .ok()?
        .ok()?;
    (!bytes.is_empty()).then(|| bytes.to_vec())
}

/// A chunked body or a nonzero `Content-Length`.
pub(crate) fn has_request_body(headers: &HeaderMap) -> bool {
    headers.contains_key(header::TRANSFER_ENCODING)
        || headers
            .get(header::CONTENT_LENGTH)
            .is_some_and(|value| value.as_bytes() != b"0")
}

/// The first value of a header as text; absent or non-text is empty.
pub(crate) fn header_text(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// Repeated `Cookie` headers joined before the shared cookie reader.
pub(crate) fn cookie_header(headers: &HeaderMap) -> String {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ")
}

/// A case-sensitive `Bearer ` prefix; an empty token counts as absent.
pub(crate) fn bearer_token(headers: &HeaderMap) -> &str {
    header_text(headers, header::AUTHORIZATION)
        .strip_prefix("Bearer ")
        .unwrap_or_default()
}

/// Both a `Connection: upgrade` token and an `Upgrade` header. Signaling owns
/// validation of the exact `/signal` path, query and admission.
pub(crate) fn is_upgrade_request(headers: &HeaderMap) -> bool {
    !header_text(headers, header::UPGRADE).is_empty()
        && headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}

/// The request's `Host`, falling back to an absolute-form authority.
pub(crate) fn request_host<B>(request: &Request<B>) -> String {
    let host = header_text(request.headers(), header::HOST);
    if !host.is_empty() {
        return host.to_owned();
    }
    request
        .uri()
        .authority()
        .map(|authority| authority.as_str().to_owned())
        .unwrap_or_default()
}
