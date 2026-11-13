//! HTTP routing for discovery, health, presentation and sharing quality.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use piik_app_config::{MAX_QUALITY_PREFERENCE_BYTES, QualityPreference, normalize_site};
use piik_protocol::QualitySettings;
use serde::{Deserialize, Serialize};

use super::control::handle_control;
use super::{
    Inner, MAX_PRESENTATION_BYTES, buildinfo, encode_message, header_text, json,
    method_not_allowed, not_found, request_host, text_error,
};
use crate::capabilities::Health;
use crate::protocol::{PROTOCOL_VERSION, SERVICE_NAME};

pub(super) async fn dispatch(State(inner): State<Arc<Inner>>, request: Request) -> Response {
    if request.uri().path() == "/connect-discovery" {
        return handle_connect_discovery(&inner, &request);
    }
    let origin = header_text(request.headers(), header::ORIGIN)
        .trim()
        .to_owned();
    if !origin.is_empty() && !inner.allowed_origin(&origin) && !inner.local_origin(&origin) {
        return text_error(StatusCode::FORBIDDEN, "forbidden");
    }
    if !inner.host_allowed(&request_host(&request)) {
        return not_found();
    }
    let mut response = match request.uri().path() {
        "/health" => handle_health(&inner, &request),
        "/control" => handle_control(inner, request).await,
        "/presentation" => handle_presentation(&inner, request).await,
        "/quality-preference" => handle_quality_preference(&inner, request).await,
        _ => not_found(),
    };
    if !origin.is_empty() {
        allow_origin(&mut response, &origin);
    }
    response
}

fn allow_origin(response: &mut Response, origin: &str) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.append(header::VARY, HeaderValue::from_static("Origin"));
    }
}

fn no_store(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

fn preflight<B>(
    request: &Request<B>,
    methods: &'static str,
    headers: Option<&'static str>,
) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let values = response.headers_mut();
    values.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(methods),
    );
    if let Some(allowed) = headers {
        values.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(allowed),
        );
    }
    if header_text(
        request.headers(),
        header::HeaderName::from_static("access-control-request-private-network"),
    )
    .trim()
    .eq_ignore_ascii_case("true")
    {
        values.insert(
            header::HeaderName::from_static("access-control-allow-private-network"),
            HeaderValue::from_static("true"),
        );
    }
    response
}

fn write_json<T: Serialize + ?Sized>(status: StatusCode, value: &T) -> Response {
    match encode_message(value) {
        Ok(payload) => json(status, payload),
        Err(_) => text_error(StatusCode::INTERNAL_SERVER_ERROR, "response unavailable"),
    }
}

/// Discovery is the only cross-origin endpoint available before local
/// approval. It exposes process identity and its local approval page, never
/// capabilities, source lists, settings, or control access. A GET has no side
/// effects.
fn handle_connect_discovery(inner: &Inner, request: &Request) -> Response {
    if !inner.host_allowed(&request_host(request)) {
        return not_found();
    }
    let origin = header_text(request.headers(), header::ORIGIN);
    if origin.is_empty() || normalize_site(origin).ok().as_deref() != Some(origin) {
        return text_error(StatusCode::FORBIDDEN, "forbidden");
    }
    let mut response = if request.method() == Method::OPTIONS {
        preflight(request, "GET", None)
    } else if request.method() != Method::GET {
        method_not_allowed("GET, OPTIONS")
    } else {
        let connect_url = inner.lock().connect_url.clone();
        if connect_url.is_empty() {
            not_found()
        } else {
            // Field order is alphabetical.
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Discovery<'a> {
                #[serde(rename = "connectURL")]
                connect_url: &'a str,
                instance_token: &'a str,
                port: u16,
                protocol: i64,
                service: &'static str,
            }
            write_json(
                StatusCode::OK,
                &Discovery {
                    connect_url: &connect_url,
                    instance_token: &inner.endpoint.instance_token,
                    port: inner.endpoint.port,
                    protocol: PROTOCOL_VERSION,
                    service: SERVICE_NAME,
                },
            )
        }
    };
    allow_origin(&mut response, origin);
    no_store(&mut response);
    response
}

fn handle_health(inner: &Inner, request: &Request) -> Response {
    if request.method() == Method::OPTIONS {
        return preflight(request, "GET", None);
    }
    if request.method() != Method::GET {
        return method_not_allowed("GET, OPTIONS");
    }
    let mut response = write_json(
        StatusCode::OK,
        &Health {
            version: buildinfo::version().to_owned(),
            revision: buildinfo::REVISION.to_owned(),
            package_target: buildinfo::package_target(),
            protocol: PROTOCOL_VERSION,
            service: SERVICE_NAME.to_owned(),
            port: inner.endpoint.port,
            instance_token: inner.endpoint.instance_token.clone(),
            native_media: inner.native_media,
        },
    );
    no_store(&mut response);
    response
}

async fn read_body(body: Body, limit: usize) -> Option<axum::body::Bytes> {
    axum::body::to_bytes(body, limit).await.ok()
}

async fn handle_presentation(inner: &Inner, request: Request) -> Response {
    if request.method() == Method::OPTIONS {
        return preflight(&request, "POST", Some("Content-Type"));
    }
    if request.method() != Method::POST {
        return method_not_allowed("POST, OPTIONS");
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Presentation {
        #[serde(default)]
        language: String,
    }
    let presentation = read_body(request.into_body(), MAX_PRESENTATION_BYTES)
        .await
        .and_then(|body| serde_json::from_slice::<Presentation>(&body).ok())
        .filter(|value| matches!(value.language.as_str(), "zh" | "en" | "vis"));
    let Some(presentation) = presentation else {
        return text_error(StatusCode::BAD_REQUEST, "invalid presentation");
    };
    if let Some(callback) = &inner.presentation {
        callback(&presentation.language);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn handle_quality_preference(inner: &Inner, request: Request) -> Response {
    let Some(preference) = &inner.quality_preference else {
        return not_found();
    };
    let mut response = quality_preference(inner, preference, request).await;
    no_store(&mut response);
    response
}

async fn quality_preference(
    inner: &Inner,
    preference: &QualityPreference,
    request: Request,
) -> Response {
    if request.method() == Method::OPTIONS {
        return preflight(&request, "GET, PUT", Some("Content-Type, X-Piik-Instance"));
    }
    if request.method() != Method::GET && request.method() != Method::PUT {
        return method_not_allowed("GET, PUT, OPTIONS");
    }
    // Origin/Host are checked by dispatch. Bind reads and writes to this App run.
    let instance = header_text(
        request.headers(),
        header::HeaderName::from_static("x-piik-instance"),
    );
    if instance != inner.endpoint.instance_token {
        return text_error(StatusCode::FORBIDDEN, "invalid App instance");
    }
    if request.method() == Method::GET {
        let payload = preference.read().map_or_else(
            || b"null".to_vec(),
            |quality| piik_protocol::encode(&quality),
        );
        return json(StatusCode::OK, payload);
    }
    let quality = read_body(request.into_body(), MAX_QUALITY_PREFERENCE_BYTES)
        .await
        .and_then(|body| piik_protocol::decode::<QualitySettings>(&body).ok());
    let Some(quality) = quality else {
        return text_error(StatusCode::BAD_REQUEST, "invalid sharing quality");
    };
    if let Err(error) = preference.save(quality) {
        tracing::debug!(
            event = "quality-preference-save-failed",
            error = %error,
            "piik-client"
        );
        return text_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "sharing preference could not be saved",
        );
    }
    StatusCode::NO_CONTENT.into_response()
}
