//! The built Browser UI the launcher serves, and its HTTP delivery.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// The launcher's Browser entry document. `/client` serves it directly
/// because that route belongs to the client-side router, not to the assets.
pub const LAUNCHER_INDEX: &str = "index.html";

/// gzhttp's default: smaller bodies are not worth compressing.
const MIN_COMPRESS_BYTES: usize = 1024;
const COMPRESSIBLE: [&str; 4] = [
    "text/html",
    "text/javascript",
    "application/javascript",
    "text/css",
];

/// Read-only Browser assets addressed by `/`-separated relative paths such as
/// `index.html` or `assets/app.js`. The App supplies its embedded build.
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

/// Assets read from one build directory.
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
            file.push(segment);
        }
        std::fs::read(file).ok().map(Cow::Owned)
    }
}

/// A clean relative path, or `None` for anything that could leave the root.
pub(crate) fn clean_path(path: &str) -> Option<String> {
    let path = percent_decode(path)?;
    let relative = path.strip_prefix('/')?;
    let mut segments = Vec::new();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            _ if segment.contains(['\\', ':', '\0']) => return None,
            _ => segments.push(segment),
        }
    }
    let mut clean = segments.join("/");
    if relative.is_empty() || relative.ends_with('/') {
        if !clean.is_empty() {
            clean.push('/');
        }
        clean.push_str(LAUNCHER_INDEX);
    }
    Some(clean)
}

fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            output.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

/// The content type for the extensions a Vite build uses.
pub(crate) fn content_type(path: &str) -> &'static str {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/vnd.microsoft.icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// Serves one asset, negotiating gzip for text types as `httpassets` does.
pub(crate) fn file_response(request: &HeaderMap, path: &str, data: Cow<'static, [u8]>) -> Response {
    let content_type = content_type(path);
    let compressible = COMPRESSIBLE
        .iter()
        .any(|kind| content_type.split(';').next() == Some(kind));
    let mut response = if compressible && data.len() >= MIN_COMPRESS_BYTES && accepts_gzip(request)
    {
        match gzip(&data) {
            Some(compressed) => {
                let mut response = (StatusCode::OK, compressed).into_response();
                response
                    .headers_mut()
                    .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                response
            }
            None => (StatusCode::OK, data.into_owned()).into_response(),
        }
    } else {
        (StatusCode::OK, data.into_owned()).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if compressible {
        headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    response
}

fn accepts_gzip(request: &HeaderMap) -> bool {
    request
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|coding| {
            let mut parts = coding.split(';').map(str::trim);
            let name = parts.next().unwrap_or_default();
            let refused = parts.any(|parameter| {
                parameter
                    .strip_prefix("q=")
                    .and_then(|quality| quality.parse::<f32>().ok())
                    == Some(0.0)
            });
            (name.eq_ignore_ascii_case("gzip") || name == "*") && !refused
        })
}

fn gzip(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_paths_stay_inside_the_build() {
        assert_eq!(clean_path("/").as_deref(), Some("index.html"));
        assert_eq!(
            clean_path("/assets/app.js").as_deref(),
            Some("assets/app.js")
        );
        assert_eq!(clean_path("/assets/").as_deref(), Some("assets/index.html"));
        assert_eq!(clean_path("/a%20b.txt").as_deref(), Some("a b.txt"));
        for escape in ["/../secret", "/a/%2e%2e/b", "/C:/x", "/a\\b", "/%zz"] {
            assert_eq!(clean_path(escape), None, "{escape}");
        }
    }

    #[test]
    fn gzip_negotiation_honors_refusal() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("br, gzip"),
        );
        assert!(accepts_gzip(&headers));
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip;q=0"),
        );
        assert!(!accepts_gzip(&headers));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }
}
