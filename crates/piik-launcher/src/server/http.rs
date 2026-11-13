//! The launcher page and its `/api/client-launcher*` request handlers.

use std::error::Error as StdError;
use std::net::IpAddr;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Attempt, AttemptResult, Inner, LauncherState, MAX_DETAIL_CHARS, MAX_REQUEST_BYTES, Mode,
    Selection, SharedError,
};
use crate::assets::{LAUNCHER_INDEX, clean_path, file_response};
use piik_loopback::buildinfo;
use piik_loopback::http::{json, method_not_allowed, not_found, text_error};
pub(super) async fn dispatch(State(inner): State<Arc<Inner>>, request: Request) -> Response {
    if !inner.request_allowed(&request) {
        return not_found();
    }
    let mut response = match request.uri().path() {
        "/api/client-launcher" => handle_state(&inner, request.method()),
        "/api/client-launcher/launch" => handle_launch(&inner, request).await,
        "/api/client-launcher/site" => handle_site(&inner, request, false).await,
        "/api/client-launcher/startup-site" => handle_site(&inner, request, true).await,
        "/api/client-launcher/debug" => handle_debug(&inner, request.method()),
        "/client" | "/client/" => serve_client(&inner, &request),
        _ => serve_static(&inner, &request),
    };
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn serve_client(inner: &Inner, request: &Request) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return method_not_allowed("GET, HEAD");
    }
    let Some(index) = inner.assets.read(LAUNCHER_INDEX) else {
        return not_found();
    };
    let mut response = file_response(request.headers(), LAUNCHER_INDEX, index);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn serve_static(inner: &Inner, request: &Request) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return method_not_allowed("GET, HEAD");
    }
    let Some(path) = clean_path(request.uri().path()) else {
        return not_found();
    };
    match inner.assets.read(&path) {
        Some(data) => file_response(request.headers(), &path, data),
        None => not_found(),
    }
}

fn write_json(status: StatusCode, value: &Value) -> Response {
    let mut response = json(status, value.to_string());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) fn reason(error: Option<&SharedError>) -> &'static str {
    error
        .and_then(|error| piik_failure::of(error.as_ref() as &(dyn StdError + 'static)))
        .map_or("", piik_failure::Reason::as_str)
}

fn handle_state(inner: &Inner, method: &Method) -> Response {
    if method != Method::GET {
        return method_not_allowed("GET");
    }
    let addresses = (inner.lan_addresses)().unwrap_or_else(|error| {
        tracing::debug!(
            error = %piik_diagnostics::safe_text(&error.to_string()),
            "launcher-lan-unavailable"
        );
        Vec::new()
    });
    let values: Vec<String> = addresses
        .iter()
        .map(|address| address.address.clone())
        .collect();
    let selected = piik_lan::select(&values, &inner.lan_address).unwrap_or_default();
    let modes = if inner.modes.is_empty() {
        vec![Mode::Local, Mode::Link]
    } else {
        inner.modes.clone()
    };
    let state = inner.lock();
    let (result, starting) = state.attempt.as_ref().map_or_else(
        || (AttemptResult::default(), false),
        |attempt| (attempt.result(), !attempt.ready.is_cancelled()),
    );
    // Keys stay in the published (alphabetical) order.
    let mut body = serde_json::Map::new();
    body.insert("controlPort".into(), inner.control_port.into());
    if inner.enable_debug.is_some() || state.debug {
        body.insert("debug".into(), state.debug.into());
    }
    body.insert(
        "defaultMode".into(),
        inner.default_mode.unwrap_or(Mode::Link).as_str().into(),
    );
    body.insert(
        "detail".into(),
        error_detail(result.error.as_deref()).into(),
    );
    body.insert(
        "lan".into(),
        json!({ "addresses": addresses, "selected": selected }),
    );
    body.insert(
        "localAccessPassword".into(),
        inner.local_access_password.clone().into(),
    );
    body.insert("modes".into(), json!(modes));
    body.insert("packageTarget".into(), buildinfo::package_target().into());
    body.insert("reason".into(), reason(result.error.as_ref()).into());
    body.insert("revision".into(), inner.revision.clone().into());
    body.insert("sites".into(), json!(state.sites));
    body.insert("startupSite".into(), state.startup_site.clone().into());
    body.insert("starting".into(), starting.into());
    body.insert("target".into(), result.target.into());
    body.insert("version".into(), inner.version.clone().into());
    drop(state);
    write_json(StatusCode::OK, &Value::Object(body))
}

async fn read_body(body: Body) -> Option<Bytes> {
    axum::body::to_bytes(body, MAX_REQUEST_BYTES).await.ok()
}

fn site_result(state: &LauncherState) -> Value {
    json!({ "sites": state.sites, "startupSite": state.startup_site })
}

async fn handle_site(inner: &Inner, request: Request, startup_only: bool) -> Response {
    if request.method() != Method::POST {
        return method_not_allowed("POST");
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Change {
        #[serde(default)]
        site: String,
        #[serde(default)]
        allowed: Option<bool>,
    }
    let change = read_body(request.into_body())
        .await
        .and_then(|body| serde_json::from_slice::<Change>(&body).ok())
        .filter(|change| change.allowed.is_some() != startup_only);
    let Some(change) = change else {
        return text_error(StatusCode::BAD_REQUEST, "invalid site request");
    };
    let site = match piik_app_config::normalize_site(&change.site) {
        Ok(site) if startup_only || !site.is_empty() => site,
        _ => return text_error(StatusCode::BAD_REQUEST, "invalid site origin"),
    };
    let _serial = inner.site_change.lock().await;
    if startup_only {
        let mut state = inner.lock();
        if !site.is_empty() && !state.sites.contains(&site) {
            return write_json(StatusCode::CONFLICT, &site_result(&state));
        }
        let Some(save) = &inner.save_startup_site else {
            return text_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "startup preference is unavailable",
            );
        };
        if save(&site).is_err() {
            return write_json(StatusCode::SERVICE_UNAVAILABLE, &site_result(&state));
        }
        state.startup_site = site;
        return write_json(StatusCode::OK, &site_result(&state));
    }
    let Some(change_site) = &inner.change_site else {
        return text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "site approval is unavailable",
        );
    };
    let (current, result) = change_site(site, change.allowed.unwrap_or_default()).await;
    let mut errors: Vec<String> = result
        .err()
        .map(|error| error.to_string())
        .into_iter()
        .collect();
    let mut state = inner.lock();
    // Permission can be revoked even if its media cleanup times out.
    state.sites = current;
    if !state.startup_site.is_empty() && !state.sites.contains(&state.startup_site) {
        state.startup_site.clear();
        if let Some(save) = &inner.save_startup_site
            && let Err(error) = save("")
        {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        return write_json(StatusCode::OK, &site_result(&state));
    }
    let mut failure = site_result(&state);
    failure["detail"] = error_text_detail(&errors.join("\n")).into();
    write_json(StatusCode::SERVICE_UNAVAILABLE, &failure)
}

fn handle_debug(inner: &Inner, method: &Method) -> Response {
    if method != Method::POST {
        return method_not_allowed("POST");
    }
    let mut state = inner.lock();
    if !state.debug {
        let running = state
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.result().error.is_none());
        let Some(enable) = inner.enable_debug.as_ref().filter(|_| !running) else {
            return text_error(
                StatusCode::CONFLICT,
                "enable diagnostics before starting a local room",
            );
        };
        if let Err(error) = enable() {
            return write_json(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({ "detail": error_text_detail(&error.to_string()) }),
            );
        }
        state.debug = true;
    }
    write_json(StatusCode::OK, &json!({ "debug": true }))
}

pub(super) async fn handle_launch(inner: &Inner, request: Request) -> Response {
    if request.method() != Method::POST {
        return method_not_allowed("POST");
    }
    let Some(body) = read_body(request.into_body()).await else {
        return invalid_launch("invalid launch request");
    };
    match admit_launch(inner, &body) {
        Ok(attempt) => await_launch(inner, &attempt).await,
        Err(response) => *response,
    }
}

/// Accepts a selection and joins or starts its attempt. No await separates
/// starting an attempt from queuing its selection, so a departed page cannot
/// leave the App starting without one.
pub(super) fn admit_launch(inner: &Inner, body: &[u8]) -> Result<Arc<Attempt>, Box<Response>> {
    let selection = decode_selection(body).map_err(|detail| Box::new(invalid_launch(detail)))?;
    if !inner.modes.is_empty() && !inner.modes.contains(&selection.mode) {
        return Err(Box::new(text_error(
            StatusCode::BAD_REQUEST,
            "unsupported launch mode",
        )));
    }
    let (attempt, fresh) = inner.begin();
    if fresh {
        // Each accepted attempt outlives its page request and owns its result.
        let _ = inner.selection_sender.send(selection);
    }
    Ok(attempt)
}

/// Reentry joins this exact attempt, including an accepted startup whose
/// original page has gone away. It cannot start another room or join a retry.
pub(super) async fn await_launch(inner: &Inner, attempt: &Attempt) -> Response {
    tokio::select! {
        () = attempt.ready.cancelled() => {}
        () = inner.cancel.cancelled() => {
            return text_error(StatusCode::SERVICE_UNAVAILABLE, "App launcher stopped");
        }
    }
    let result = attempt.result();
    match result.error {
        Some(error) => write_json(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({
                "detail": error_detail(Some(error.as_ref())),
                "error": "Piik App could not start",
                "reason": reason(Some(&error)),
            }),
        ),
        None => write_json(StatusCode::OK, &json!({ "target": result.target })),
    }
}

pub(super) fn invalid_launch(detail: &str) -> Response {
    write_json(
        StatusCode::BAD_REQUEST,
        &json!({ "detail": detail, "error": "invalid launch request" }),
    )
}

pub(super) fn decode_selection(body: &[u8]) -> Result<Selection, &'static str> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct Raw {
        #[serde(default)]
        mode: String,
        #[serde(default)]
        language: String,
        #[serde(default)]
        local_access_password: String,
        #[serde(default)]
        debug: bool,
        #[serde(default)]
        lan_address: Option<String>,
    }
    let raw: Raw = serde_json::from_slice(body).map_err(|_| "invalid launch request")?;
    if !matches!(raw.language.as_str(), "zh" | "en" | "vis") {
        return Err("invalid launch language");
    }
    let mode = Mode::parse(&raw.mode);
    let lan_address = match raw.lan_address {
        None => None,
        Some(_) if mode != Some(Mode::Local) => {
            return Err("only Local launch can select a LAN invitation address");
        }
        Some(value) => {
            let value = value.trim().to_owned();
            let ipv4 = match value.parse::<IpAddr>() {
                Ok(IpAddr::V4(_)) => true,
                Ok(IpAddr::V6(ip)) => ip.to_ipv4_mapped().is_some(),
                Err(_) => false,
            };
            if !ipv4 {
                return Err("Local invitation address must be an active IPv4 address");
            }
            Some(value)
        }
    };
    let mode = mode.ok_or("unknown launch mode")?;
    Ok(Selection {
        mode,
        language: raw.language,
        local_access_password: raw.local_access_password,
        debug: raw.debug,
        lan_address,
    })
}

pub(super) fn error_detail(error: Option<&(dyn StdError + Send + Sync + 'static)>) -> String {
    error.map_or_else(String::new, |error| error_text_detail(&error.to_string()))
}

/// Redacts before truncation so a long credential cannot lose its delimiter.
fn error_text_detail(text: &str) -> String {
    let detail = piik_diagnostics::safe_text(text);
    match detail.char_indices().nth(MAX_DETAIL_CHARS) {
        Some((end, _)) => format!("{}...", &detail[..end]),
        None => detail,
    }
}

/// An exact `scheme://host` origin with no path, query or fragment.
pub(super) fn same_origin(actual: &str, expected: &str) -> bool {
    let split = |value: &str| {
        let (scheme, host) = value.split_once("://")?;
        (!host.contains(['/', '?', '#'])).then(|| (scheme.to_ascii_lowercase(), host.to_owned()))
    };
    match (split(actual), split(expected)) {
        (Some((scheme, host)), Some((expected_scheme, expected_host))) => {
            scheme == expected_scheme && host.eq_ignore_ascii_case(&expected_host)
        }
        _ => false,
    }
}

pub(super) fn valid_target(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|target| {
        matches!(target.scheme(), "http" | "https")
            && target.host_str().is_some_and(|host| !host.is_empty())
    })
}
