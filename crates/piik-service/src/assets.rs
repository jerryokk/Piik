//! The Browser UI build and its HTTP delivery: exact content types, SPA
//! fallback, byte ranges and gzip for text assets.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use tower::ServiceExt as _;
use tower_http::compression::Compression;
use tower_http::compression::predicate::Predicate;

use crate::http::{json_response, not_found_body};

/// Read-only Browser assets addressed by `/`-separated relative paths such as
/// `index.html` or `assets/app.js`.
pub trait Assets: Send + Sync + 'static {
    /// The file at `path`, or `None` when it is absent or not a file.
    fn read(&self, path: &str) -> Option<Cow<'static, [u8]>>;
}

/// Assets held in memory, such as an `include_bytes!` table.
#[derive(Debug, Clone, Default)]
pub struct MemoryAssets(pub BTreeMap<String, Cow<'static, [u8]>>);

impl MemoryAssets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, path: impl Into<String>, data: impl Into<Cow<'static, [u8]>>) -> Self {
        self.0.insert(path.into(), data.into());
        self
    }
}

impl Assets for MemoryAssets {
    fn read(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        self.0.get(path).cloned()
    }
}

/// Assets read from one build directory, such as a development Vite output.
#[derive(Debug, Clone)]
pub struct DirAssets(PathBuf);

impl DirAssets {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self(root.into())
    }

    pub fn root(&self) -> &Path {
        &self.0
    }
}

impl Assets for DirAssets {
    fn read(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        let mut file = self.0.clone();
        for segment in path.split('/') {
            // Separators and drive prefixes the URL grammar cannot express
            // must not reach the Windows file system.
            if segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.contains(['\\', ':', '\0'])
            {
                return None;
            }
            file.push(segment);
        }
        if !file.is_file() {
            return None;
        }
        std::fs::read(file).ok().map(Cow::Owned)
    }
}

/// Fixed MIME types for this repository's Web assets, independent of the
/// machine. With `nosniff`, a wrong type on a font or manifest is visible.
pub(crate) fn content_type(name: &str) -> &'static str {
    let extension = name
        .rfind('.')
        .filter(|dot| !name[*dot..].contains('/'))
        .map(|dot| name[dot..].to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        ".html" | ".htm" => "text/html;charset=utf-8",
        ".js" | ".mjs" => "text/javascript",
        ".css" => "text/css",
        ".json" | ".map" => "application/json",
        ".txt" => "text/plain",
        ".svg" => "image/svg+xml",
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".webp" => "image/webp",
        ".gif" => "image/gif",
        ".avif" => "image/avif",
        ".woff" => "font/woff",
        ".woff2" => "font/woff2",
        ".ttf" => "font/ttf",
        ".otf" => "font/otf",
        ".wasm" => "application/wasm",
        ".webmanifest" => "application/manifest+json",
        ".bin" => "application/octet-stream",
        ".xml" => "text/xml",
        // `.ico` keeps the published empty type, as does any other name.
        _ => "",
    }
}

/// gzhttp's default minimum: smaller bodies are not worth compressing.
const MIN_COMPRESS_BYTES: u64 = 1024;
const COMPRESSIBLE: [&str; 4] = [
    "text/html",
    "text/javascript",
    "application/javascript",
    "text/css",
];

/// Compresses text assets of at least [`MIN_COMPRESS_BYTES`]; ranges, HEAD
/// and precompressed media stay identity.
#[derive(Clone, Copy)]
struct TextAssets;

impl Predicate for TextAssets {
    fn should_compress<B: axum::body::HttpBody>(&self, response: &Response<B>) -> bool {
        let kind = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or_default()
            .trim();
        let size = response.body().size_hint().exact().unwrap_or(0);
        COMPRESSIBLE.contains(&kind) && size >= MIN_COMPRESS_BYTES
    }
}

/// Serves `assets`, falling back to `index.html` for SPA routes; missing
/// files answer the application's JSON 404.
#[derive(Clone)]
pub(crate) struct Frontend {
    assets: Arc<dyn Assets>,
}

impl Frontend {
    pub(crate) fn new(assets: Arc<dyn Assets>) -> Self {
        Self { assets }
    }

    pub(crate) async fn serve(&self, request: Request<Body>) -> Response<Body> {
        let assets = self.assets.clone();
        let service = tower::service_fn(move |request: Request<Body>| {
            let assets = assets.clone();
            async move { Ok::<_, Infallible>(static_response(assets.as_ref(), &request)) }
        });
        let service = Compression::new(service)
            .no_br()
            .no_deflate()
            .no_zstd()
            .compress_when(TextAssets);
        match service.oneshot(request).await {
            Ok(response) => response.map(Body::new),
            Err(never) => match never {},
        }
    }
}

fn static_response(assets: &dyn Assets, request: &Request<Body>) -> Response<Body> {
    let raw = decode_path(request.uri().path());
    let cleaned = clean_path(&format!("/{raw}"));
    let name = cleaned.trim_start_matches('/');

    let serve = |candidate: &str| -> Option<Response<Body>> {
        // Cleaned names cannot traverse; hidden assets are excluded.
        if candidate.is_empty() || has_dot_segment(candidate) {
            return None;
        }
        let data = assets.read(candidate)?;
        Some(file_response(
            request.method(),
            request.headers(),
            candidate,
            data,
        ))
    };

    // A trailing slash cannot name an asset file.
    let mut response = (!raw.ends_with('/'))
        .then(|| serve(name))
        .flatten()
        // Route-like paths use the SPA; files and .well-known requests do not.
        .or_else(|| {
            (!looks_like_file(&cleaned) && !cleaned.contains("/.well-known"))
                .then(|| serve("index.html"))
                .flatten()
        })
        .unwrap_or_else(|| json_response(StatusCode::NOT_FOUND, not_found_body()));
    // gzhttp varies every response it wraps, compressed or not.
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    response
}

/// `http.ServeContent` without modification times or ETags: full bodies,
/// HEAD and one byte range.
fn file_response(
    method: &Method,
    request: &HeaderMap,
    name: &str,
    data: Cow<'static, [u8]>,
) -> Response<Body> {
    let size = data.len() as u64;
    let mut status = StatusCode::OK;
    let mut range = None;
    // Without validators an If-Range condition never matches.
    if let Some(value) = request
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        && !request.contains_key(header::IF_RANGE)
    {
        match parse_range(value, size) {
            Ok(Some(selected)) => {
                status = StatusCode::PARTIAL_CONTENT;
                range = Some(selected);
            }
            Ok(None) => {}
            Err(no_overlap) => return range_not_satisfiable(size, no_overlap),
        }
    }
    let (start, length) = range.unwrap_or((0, size));
    let mut response = Response::new(if method == Method::HEAD {
        Body::empty()
    } else {
        let end = usize::try_from(start + length).unwrap_or(usize::MAX);
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        match data {
            Cow::Borrowed(bytes) => Body::from(&bytes[start..end]),
            Cow::Owned(bytes) => Body::from(bytes[start..end].to_vec()),
        }
    });
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // Possibly empty, so the browser never sniffs and nothing guesses.
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(name)),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if range.is_some() {
        headers.insert(
            header::CONTENT_RANGE,
            header_value(&format!("bytes {start}-{}/{size}", start + length - 1)),
        );
    }
    headers.insert(header::CONTENT_LENGTH, header_value(&length.to_string()));
    response
}

fn range_not_satisfiable(size: u64, no_overlap: bool) -> Response<Body> {
    let message = if no_overlap {
        "invalid range: failed to overlap\n"
    } else {
        "invalid range\n"
    };
    let mut response = Response::new(Body::from(message));
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if no_overlap {
        headers.insert(
            header::CONTENT_RANGE,
            header_value(&format!("bytes */{size}")),
        );
    }
    response
}

fn header_value(text: &str) -> HeaderValue {
    HeaderValue::from_str(text).expect("generated header values are visible ASCII")
}

/// Parses a `Range` header into one `(start, length)`. `Ok(None)` serves the whole
/// file: several ranges, or ranges that together exceed it. `Err(true)` is
/// the no-overlap failure, `Err(false)` a malformed header.
fn parse_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, bool> {
    let Some(specs) = value.strip_prefix("bytes=") else {
        return Err(false);
    };
    let mut ranges = Vec::new();
    let mut no_overlap = false;
    for spec in specs.split(',') {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let (start, end) = spec.split_once('-').ok_or(false)?;
        let (start, end) = (start.trim(), end.trim());
        let parse = |text: &str| {
            text.parse::<u64>()
                .ok()
                .filter(|_| text.bytes().all(|b| b.is_ascii_digit()))
        };
        if start.is_empty() {
            // A suffix range names the final bytes.
            let suffix = parse(end).ok_or(false)?.min(size);
            if suffix == 0 {
                no_overlap = true;
                continue;
            }
            ranges.push((size - suffix, suffix));
            continue;
        }
        let first = parse(start).ok_or(false)?;
        if first >= size {
            no_overlap = true;
            continue;
        }
        let length = if end.is_empty() {
            size - first
        } else {
            let last = parse(end).ok_or(false)?;
            if first > last {
                return Err(false);
            }
            last.min(size - 1) - first + 1
        };
        ranges.push((first, length));
    }
    if ranges.is_empty() {
        return if no_overlap { Err(true) } else { Ok(None) };
    }
    let total: u64 = ranges.iter().map(|(_, length)| length).sum();
    // Multipart byte ranges are not served; the whole file satisfies them.
    if ranges.len() > 1 || total > size {
        return Ok(None);
    }
    Ok(Some(ranges[0]))
}

/// The percent-decoded request path. Undecodable escapes stay
/// literal; invalid UTF-8 is replaced.
pub(crate) fn decode_path(path: &str) -> String {
    percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .into_owned()
}

/// Lexically cleans a rooted path, resolving `.` and `..` within the root.
pub(crate) fn clean_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    format!("/{}", segments.join("/"))
}

/// Excludes hidden assets, with the published `.well-known` exemption.
pub(crate) fn has_dot_segment(name: &str) -> bool {
    if name.contains(".well-known/") {
        return false;
    }
    name.split('/').any(|segment| segment.starts_with('.'))
}

/// The published SPA fallback boundary: the last segment needs an
/// ASCII-style basename and a nonempty alphanumeric/underscore extension, so
/// names such as `/文件.js` and `/café.css` still reach the SPA.
pub(crate) fn looks_like_file(pathname: &str) -> bool {
    let Some(slash) = pathname.rfind('/') else {
        return false;
    };
    let segment = &pathname[slash + 1..];
    let Some(dot) = segment.rfind('.') else {
        return false;
    };
    if dot == 0 || dot == segment.len() - 1 {
        return false;
    }
    // [A-Za-z\s\d~$._-]; URL paths are decoded first, so a non-ASCII space
    // can reach this test and the protocol owns the JavaScript whitespace class.
    let basename = segment[..dot].chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, '~' | '$' | '.' | '_' | '-')
            || piik_protocol::is_js_whitespace(character)
    });
    // \w+
    let extension = segment[dot + 1..]
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_');
    basename && extension
}
