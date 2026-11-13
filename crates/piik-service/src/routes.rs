//! Request dispatch. The first matching route wins; the `/api/` catch-all
//! follows every concrete route so unknown API requests never reach the
//! frontend.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use futures_util::FutureExt as _;
use piik_protocol::{
    RuntimeCapabilities, ToWire, decode_create_room_request, decode_replace_room_request,
    decode_room_access_update_request,
};
use piik_room::ErrorCode;
use serde_json::{Value, json};
use url::Url;

use crate::access::{AccessPolicy, SiteAccessGate};
use crate::http::{
    bearer_token, cookie_header, error_response, has_request_body, header_text, is_upgrade_request,
    json_bytes, json_response, not_found_body, read_json_body, request_host,
};
use crate::server::{Inner, REQUEST_TIMEOUT};

/// Marks an upgrade received over TLS for the signaling site-access check,
/// which sees only headers. Every inbound copy is removed first.
pub(crate) const TLS_TRANSPORT_MARKER: &str = "x-piik-internal-tls-transport";

impl Inner {
    /// Dispatches one request and contains handler panics at the HTTP
    /// boundary. Every response forbids framing.
    pub(crate) async fn dispatch(
        self: &Arc<Self>,
        request: Request<Body>,
        tls: bool,
    ) -> Response<Body> {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let upgrade = is_upgrade_request(request.headers());
        let handled = std::panic::AssertUnwindSafe(self.handle(request, tls))
            .catch_unwind()
            .await;
        let mut response = handled.unwrap_or_else(|_| request_failed(&method, &path, upgrade));
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        );
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        response
    }

    async fn handle(self: &Arc<Self>, mut request: Request<Body>, tls: bool) -> Response<Body> {
        request.headers_mut().remove(TLS_TRANSPORT_MARKER);
        let signaling = self.accepting_signaling();
        let Some(signaling) = signaling else {
            // Nothing is served until startup completes; /healthz is 503 too.
            let mut response = error_response(StatusCode::SERVICE_UNAVAILABLE, "Service starting");
            let headers = response.headers_mut();
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return response;
        };
        if is_upgrade_request(request.headers()) {
            // Signaling owns every upgrade, including rejection of invalid paths.
            if tls {
                request
                    .headers_mut()
                    .insert(TLS_TRANSPORT_MARKER, HeaderValue::from_static("1"));
            }
            return signaling.serve_upgrade(request).await;
        }
        // Escaped: "/api%2frooms" and "/x/../api/rooms" cannot alias /api/rooms.
        let path = request.uri().path().to_owned();
        match path.as_str() {
            "/healthz" => {
                let mut response = allow_method(&request, Method::GET, "GET")
                    .unwrap_or_else(|| json_response(StatusCode::OK, json!({ "status": "ok" })));
                no_store(response.headers_mut());
                response
            }
            "/api/capabilities" => {
                api(allow_method(&request, Method::GET, "GET")
                    .unwrap_or_else(|| self.capabilities()))
            }
            "/api/site-access" => api(self.site_access(request, tls).await),
            "/api/rooms" => api(self.create_room(request, tls, &signaling).await),
            _ => {
                if let Some(room_id) = room_route(&path, "/replacement") {
                    return api(self.replace_room(request, tls, &signaling, room_id).await);
                }
                if let Some(room_id) = room_route(&path, "/access") {
                    return api(self
                        .update_room_access(request, tls, &signaling, room_id)
                        .await);
                }
                if path.starts_with("/api/") {
                    // The catch-all sets no nosniff header; only concrete routes do.
                    let mut response = json_response(StatusCode::NOT_FOUND, not_found_body());
                    no_store(response.headers_mut());
                    return response;
                }
                match &self.frontend {
                    Some(frontend) => frontend.serve(request).await,
                    None => json_response(StatusCode::NOT_FOUND, not_found_body()),
                }
            }
        }
    }

    fn capabilities(&self) -> Response<Body> {
        let capabilities = RuntimeCapabilities {
            sfu: self.sfu_enabled,
            sfu_only: self.sfu_only,
            nat_prediction: self.config.nat_prediction_enabled && !self.sfu_only,
        };
        json_bytes(StatusCode::OK, capabilities.to_json())
    }

    /// The site-access gate for this request's configured destination.
    pub(crate) fn site_access_for<B>(&self, request: &Request<B>, tls: bool) -> SiteAccessGate {
        self.access.for_request(&request_host(request), tls)
    }

    /// Reports or grants site access without claiming room authority.
    async fn site_access(&self, request: Request<Body>, tls: bool) -> Response<Body> {
        let access = self.site_access_for(&request, tls);
        if request.method() == Method::GET {
            let required = access.required();
            let authenticated = access.is_authenticated(&cookie_header(request.headers()));
            let mut response =
                json_response(StatusCode::OK, site_access_body(required, authenticated));
            if required && authenticated {
                // Authenticated visits renew the site's idle access lifetime.
                set_cookie(&mut response, &access);
            }
            return response;
        }
        if request.method() != Method::POST {
            return method_not_allowed("GET, POST");
        }
        if !self.allowed_request_origin(request.headers()) {
            return error_response(StatusCode::FORBIDDEN, "Origin not allowed");
        }
        // Released clients send a bodyless Bearer request. JSON preserves
        // passwords a header cannot carry and takes precedence when sent.
        let mut provided = bearer_token(request.headers()).to_owned();
        if has_request_body(request.headers()) {
            let password = read_json_body(request, REQUEST_TIMEOUT)
                .await
                .and_then(|body| login_password(&body));
            match password {
                Some(password) => provided = password,
                None => {
                    return error_response(StatusCode::BAD_REQUEST, "Invalid site access request");
                }
            }
        }
        if !access.required() {
            return json_response(StatusCode::OK, site_access_body(false, true));
        }
        if !access.password_matches(&provided) {
            let mut response = error_response(StatusCode::UNAUTHORIZED, "Unauthorized");
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return response;
        }
        let mut response = json_response(StatusCode::OK, site_access_body(true, true));
        set_cookie(&mut response, &access);
        response
    }

    async fn create_room(
        &self,
        request: Request<Body>,
        tls: bool,
        signaling: &piik_signal::Server,
    ) -> Response<Body> {
        if let Some(response) = allow_method(&request, Method::POST, "POST") {
            return response;
        }
        if !self.allowed_request_origin(request.headers()) {
            return error_response(StatusCode::FORBIDDEN, "Origin not allowed");
        }
        // With a site password, room creation uses its cookie; a
        // `Bearer <site password>` header does not authorize it.
        if !self.room_creation_authorized(&request, tls) {
            return error_response(StatusCode::UNAUTHORIZED, "Unauthorized");
        }
        let parsed = read_json_body(request, REQUEST_TIMEOUT)
            .await
            .and_then(|body| decode_create_room_request(&body).ok());
        let Some(parsed) = parsed else {
            return error_response(StatusCode::BAD_REQUEST, "Invalid room request");
        };
        let created = signaling
            .create_room(
                parsed.code_entry_policy,
                parsed.room_password.value().cloned(),
                parsed.preferred_room_id,
            )
            .await;
        match created {
            Ok(created) => self.created_room(&created),
            Err(error) => match creation_failure(error.code()) {
                Some((status, message)) => error_response(status, message),
                None => request_failed(&Method::POST, "/api/rooms", false),
            },
        }
    }

    async fn replace_room(
        &self,
        request: Request<Body>,
        tls: bool,
        signaling: &piik_signal::Server,
        room_id: String,
    ) -> Response<Body> {
        let host_token = match self.authorized_room_host_token(&request, tls) {
            Ok(token) => token,
            Err(response) => return *response,
        };
        let parsed = read_json_body(request, REQUEST_TIMEOUT)
            .await
            .and_then(|body| decode_replace_room_request(&body).ok());
        let Some(parsed) = parsed else {
            return error_response(StatusCode::BAD_REQUEST, "Invalid room replacement request");
        };
        let replaced = signaling
            .replace_room(
                &room_id,
                &host_token,
                parsed.code_entry_policy,
                parsed.room_password.value().cloned(),
            )
            .await;
        match replaced {
            Ok(created) => self.created_room(&created),
            Err(error) => match replacement_failure(error.code()) {
                Some((status, message)) => error_response(status, message),
                None => request_failed(&Method::POST, "/api/", false),
            },
        }
    }

    async fn update_room_access(
        &self,
        request: Request<Body>,
        tls: bool,
        signaling: &piik_signal::Server,
        room_id: String,
    ) -> Response<Body> {
        let host_token = match self.authorized_room_host_token(&request, tls) {
            Ok(token) => token,
            Err(response) => return *response,
        };
        let parsed = read_json_body(request, REQUEST_TIMEOUT)
            .await
            .and_then(|body| decode_room_access_update_request(&body).ok());
        let Some(parsed) = parsed else {
            return error_response(StatusCode::BAD_REQUEST, "Invalid room access request");
        };
        match signaling
            .update_room_access(&room_id, &host_token, parsed)
            .await
        {
            Ok(updated) => json_bytes(StatusCode::OK, updated.to_json()),
            Err(error) => match access_failure(&error) {
                Some((status, message)) => error_response(status, message),
                None => request_failed(&Method::POST, "/api/", false),
            },
        }
    }

    /// Method, origin, site access and the bearer Host token, in that order,
    /// each with its own terminal response.
    fn authorized_room_host_token(
        &self,
        request: &Request<Body>,
        tls: bool,
    ) -> Result<String, Box<Response<Body>>> {
        if let Some(response) = allow_method(request, Method::POST, "POST") {
            return Err(Box::new(response));
        }
        if !self.allowed_request_origin(request.headers()) {
            return Err(Box::new(error_response(
                StatusCode::FORBIDDEN,
                "Origin not allowed",
            )));
        }
        if !self.room_creation_authorized(request, tls) {
            return Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
            )));
        }
        let host_token = bearer_token(request.headers());
        if host_token.is_empty() {
            // An absent Host token is indistinguishable from a wrong one.
            return Err(Box::new(error_response(
                StatusCode::NOT_FOUND,
                "Room not found",
            )));
        }
        Ok(host_token.to_owned())
    }

    /// A present, parseable `Origin` equal to its own serialized origin (so
    /// `http://allowed.test/path` is refused) and configured.
    fn allowed_request_origin(&self, headers: &HeaderMap) -> bool {
        let origin = header_text(headers, header::ORIGIN);
        if origin.is_empty() {
            return false;
        }
        Url::parse(origin).is_ok_and(|parsed| piik_config::origin(&parsed) == origin)
            && self.access.origin_allowed(origin)
    }

    fn room_creation_authorized(&self, request: &Request<Body>, tls: bool) -> bool {
        self.site_access_for(request, tls)
            .is_authenticated(&cookie_header(request.headers()))
    }

    /// Signaling owns the invite URL shape; the rotated-grant notification
    /// uses the same function.
    fn created_room(&self, created: &piik_room::CreatedRoom) -> Response<Body> {
        let response = piik_protocol::CreateRoomResponse {
            room_id: created.room_id.clone(),
            host_token: created.host_token.clone(),
            invite_url: piik_signal::invite_url(
                &self.config.public_base_url,
                &created.room_id,
                &created.viewer_grant,
            ),
            code_entry_policy: created.code_entry_policy,
        };
        json_bytes(StatusCode::CREATED, response.to_json())
    }
}

/// The signaling upgrade check: the same cookie policy over headers only.
pub(crate) fn site_access_at_upgrade(access: &AccessPolicy, headers: &HeaderMap) -> bool {
    let tls = headers.contains_key(TLS_TRANSPORT_MARKER);
    access
        .for_request(header_text(headers, header::HOST), tls)
        .is_authenticated(&cookie_header(headers))
}

/// `/api/rooms/{code}{suffix}` with a four-digit room code.
fn room_route(path: &str, suffix: &str) -> Option<String> {
    let code = path.strip_prefix("/api/rooms/")?.strip_suffix(suffix)?;
    let bytes = code.as_bytes();
    (bytes.len() == 4
        && (b'1'..=b'9').contains(&bytes[0])
        && bytes[1..].iter().all(u8::is_ascii_digit))
    .then(|| code.to_owned())
}

/// A JSON login body: exactly one string member named `password`.
fn login_password(body: &[u8]) -> Option<String> {
    let fields: serde_json::Map<String, Value> = serde_json::from_slice(body).ok()?;
    if fields.len() != 1 {
        return None;
    }
    fields.get("password")?.as_str().map(str::to_owned)
}

fn site_access_body(required: bool, authenticated: bool) -> Value {
    json!({ "required": required, "authenticated": authenticated })
}

fn set_cookie(response: &mut Response<Body>, access: &SiteAccessGate) {
    if let Some(cookie) = access.create_cookie()
        && let Ok(value) = HeaderValue::from_str(&cookie)
    {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
}

fn no_store(headers: &mut HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// The two headers every `/api` route sets before its method check, so a
/// 405 carries them too.
fn api(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    no_store(headers);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// The 405 row of the HTTP table, `Allow` included; `None` when allowed.
fn allow_method<B>(
    request: &Request<B>,
    allowed: Method,
    allow: &'static str,
) -> Option<Response<Body>> {
    (request.method() != allowed).then(|| method_not_allowed(allow))
}

fn method_not_allowed(allow: &'static str) -> Response<Body> {
    let mut response = error_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

/// Logs fixed request categories, then answers 500. Request paths and
/// arbitrary methods are private.
pub(crate) fn request_failed(method: &Method, path: &str, upgrade: bool) -> Response<Body> {
    let method = match *method {
        Method::GET
        | Method::POST
        | Method::HEAD
        | Method::OPTIONS
        | Method::PUT
        | Method::PATCH
        | Method::DELETE
        | Method::CONNECT
        | Method::TRACE => method.as_str(),
        _ => "other",
    };
    let route = if upgrade {
        "signaling"
    } else if path == "/healthz" {
        "health"
    } else if path.starts_with("/api/") {
        "api"
    } else {
        "frontend"
    };
    tracing::error!(method, route, "HTTP request failed");
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

type Mapped = Option<(StatusCode, &'static str)>;

/// Room replacement errors; unmapped errors reach the 500 handler.
pub(crate) fn replacement_failure(code: Option<ErrorCode>) -> Mapped {
    match code? {
        ErrorCode::RoomLimit => Some((StatusCode::SERVICE_UNAVAILABLE, "Room capacity reached")),
        ErrorCode::RoomAccessDenied | ErrorCode::RoomBusy => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "Room replacement unavailable",
        )),
        ErrorCode::InvalidToken | ErrorCode::RoomNotFound => {
            Some((StatusCode::NOT_FOUND, "Room not found"))
        }
        _ => None,
    }
}

/// Room access errors: every remaining room error is a 404; other failures
/// reach the 500 handler.
pub(crate) fn access_failure(error: &piik_room::Error) -> Mapped {
    match error.code()? {
        ErrorCode::RoomBusy => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "Room access update unavailable",
        )),
        ErrorCode::RoomAccessDenied => Some((StatusCode::CONFLICT, "Room access update rejected")),
        _ => Some((StatusCode::NOT_FOUND, "Room not found")),
    }
}

/// Room creation errors.
pub(crate) fn creation_failure(code: Option<ErrorCode>) -> Mapped {
    match code? {
        ErrorCode::RoomLimit => Some((StatusCode::SERVICE_UNAVAILABLE, "Room capacity reached")),
        ErrorCode::RoomBusy => Some((StatusCode::SERVICE_UNAVAILABLE, "Room creation unavailable")),
        ErrorCode::InvalidToken => Some((StatusCode::BAD_REQUEST, "Invalid room request")),
        _ => None,
    }
}
