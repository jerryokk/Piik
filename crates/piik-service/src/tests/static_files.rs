//! Compression negotiation and the 26-row parity table measured against
//! sirv@3.0.2.

use std::io::Read as _;
use std::sync::Arc;

use super::{Reply, Req};
use crate::assets::{Frontend, has_dot_segment, looks_like_file};
use crate::{Assets, MemoryAssets};

const NOT_FOUND: &str = r#"{"error":"Not found"}"#;

async fn serve(frontend: &Frontend, request: Req) -> Reply {
    Reply::read(frontend.serve(request.build()).await).await
}

fn frontend(assets: MemoryAssets) -> Frontend {
    Frontend::new(Arc::new(assets) as Arc<dyn Assets>)
}

#[tokio::test]
async fn compression_keeps_negotiation_and_serve_content_semantics() {
    let body = "const application = 'owned fixture';\n".repeat(100);
    let mut assets = MemoryAssets::new();
    for name in [
        "index.html",
        "app.js",
        "app.mjs",
        "app.css",
        "data.json",
        "audio.opus",
    ] {
        assets = assets.with(name, body.clone().into_bytes());
    }
    let handler = frontend(assets);
    let full = body.len() as i64;
    for (path, method, encoding, range, status, compressed, length) in [
        ("/", "GET", "gzip", "", 200, true, full),
        ("/app.js", "GET", "br, gzip", "", 200, true, full),
        ("/app.mjs", "GET", "zstd, gzip", "", 200, true, full),
        ("/app.css", "GET", "gzip", "", 200, true, full),
        ("/app.js", "GET", "gzip;q=0", "", 200, false, full),
        ("/app.js", "GET", "", "", 200, false, full),
        ("/app.js", "HEAD", "gzip", "", 200, false, 0),
        ("/app.js", "GET", "gzip", "bytes=0-2047", 206, false, 2048),
        ("/app.js", "GET", "gzip", "bytes=99999-", 416, false, -1),
        ("/data.json", "GET", "gzip", "", 200, false, full),
        ("/audio.opus", "GET", "gzip", "", 200, false, full),
    ] {
        let label = format!("{method} {path} {encoding} {range}");
        let mut request = Req::new(method, path).header("Accept-Encoding", encoding);
        if !range.is_empty() {
            request = request.header("Range", range);
        }
        let handler = handler.clone();
        let response = handler.serve(request.build()).await;
        let status_code = response.status().as_u16();
        let headers = response.headers().clone();
        let raw = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(status_code, status, "{label}");
        assert_eq!(header("content-encoding") == "gzip", compressed, "{label}");
        assert_eq!(
            header("vary"),
            "Accept-Encoding",
            "{label}: missing encoding cache variance"
        );
        let mut data = raw.to_vec();
        if compressed {
            let mut decoded = Vec::new();
            flate2::read::GzDecoder::new(raw.as_ref())
                .read_to_end(&mut decoded)
                .expect("gzip body");
            assert_eq!(
                decoded,
                body.as_bytes(),
                "{label}: compressed asset changed"
            );
            assert!(raw.len() < body.len(), "{label}: compressed asset grew");
            assert_eq!(header("content-length"), "", "{label}: raw length retained");
            data = decoded;
        }
        if length >= 0 {
            assert_eq!(data.len() as i64, length, "{label}: body length");
        }
        if method == "HEAD" {
            assert_eq!(
                header("content-length"),
                body.len().to_string(),
                "{label}: HEAD lost the length"
            );
        }
        if status == 206 {
            assert_eq!(
                header("content-range"),
                format!("bytes 0-2047/{}", body.len()),
                "{label}"
            );
        }
        for name in ["cache-control", "last-modified", "etag"] {
            assert_eq!(header(name), "", "{label}: fabricated {name}");
        }
    }
}

/// The tree evidence/runtime.md served through real sirv@3.0.2.
fn fixture() -> MemoryAssets {
    let filled = |size: usize| vec![b'x'; size];
    MemoryAssets::new()
        .with("index.html", filled(100))
        .with("assets/app.js", filled(20))
        .with("assets/app.css", filled(15))
        .with("third-party-licenses.txt", filled(21))
        .with("favicon.svg", filled(41))
        .with("fonts/inter.woff2", filled(300))
        .with("manifest.webmanifest", filled(19))
        .with("data.json", filled(7))
        .with("nomime.bin", filled(64))
        .with("noext", filled(23))
        .with(".gitkeep", filled(0))
        .with(".vite/manifest.json", filled(7))
        .with(".well-known/x.txt", filled(10))
}

#[tokio::test]
async fn static_handler_matches_sirv() {
    let handler = frontend(fixture());
    const HTML: &str = "text/html;charset=utf-8";
    const JSON: &str = "application/json; charset=utf-8";
    let not_found = NOT_FOUND.len();
    /// method, path, extra header, status, type, length, range, body length.
    type Case<'a> = (
        &'a str,
        &'a str,
        (&'a str, &'a str),
        u16,
        &'a str,
        &'a str,
        &'a str,
        usize,
    );
    #[rustfmt::skip]
    let cases: [Case<'_>; 26] = [
        ("GET", "/", ("", ""), 200, HTML, "100", "", 100),
        ("GET", "/index.html", ("", ""), 200, HTML, "100", "", 100),
        ("GET", "/host", ("", ""), 200, HTML, "100", "", 100),
        ("GET", "/room/1234", ("", ""), 200, HTML, "100", "", 100),
        ("GET", "/assets/app.js", ("", ""), 200, "text/javascript", "20", "", 20),
        ("GET", "/assets/app.css", ("", ""), 200, "text/css", "15", "", 15),
        ("GET", "/third-party-licenses.txt", ("", ""), 200, "text/plain", "21", "", 21),
        ("GET", "/favicon.svg", ("", ""), 200, "image/svg+xml", "41", "", 41),
        ("GET", "/fonts/inter.woff2", ("", ""), 200, "font/woff2", "300", "", 300),
        ("GET", "/manifest.webmanifest", ("", ""), 200, "application/manifest+json", "19", "", 19),
        ("GET", "/data.json", ("", ""), 200, "application/json", "7", "", 7),
        ("GET", "/nomime.bin", ("", ""), 200, "application/octet-stream", "64", "", 64),
        // No known extension: an empty Content-Type, as sirv sends.
        ("GET", "/noext", ("", ""), 200, "", "23", "", 23),
        ("GET", "/assets/missing.js", ("", ""), 404, JSON, "", "", not_found),
        ("GET", "/missing.css", ("", ""), 404, JSON, "", "", not_found),
        ("GET", "/.well-known/x.txt", ("", ""), 200, "text/plain", "10", "", 10),
        // A dotfile without the "any extn" ignore falls back to index.html.
        ("GET", "/.gitkeep", ("", ""), 200, HTML, "100", "", 100),
        ("GET", "/.vite/manifest.json", ("", ""), 404, JSON, "", "", not_found),
        ("GET", "/assets", ("", ""), 200, HTML, "100", "", 100),
        ("GET", "/assets/", ("", ""), 200, HTML, "100", "", 100),
        ("HEAD", "/index.html", ("", ""), 200, HTML, "100", "", 0),
        ("GET", "/index.html", ("Range", "bytes=0-9"), 206, HTML, "10", "bytes 0-9/100", 10),
        ("GET", "/index.html", ("Accept", "application/json"), 200, HTML, "100", "", 100),
        ("GET", "/../go.mod", ("", ""), 404, JSON, "", "", not_found),
        // sirv ignores the method; /api/** is routed first.
        ("POST", "/", ("", ""), 200, HTML, "100", "", 100),
        // The "any extn" class is ASCII-only, so /文件.js is not ignored.
        ("GET", "/%E6%96%87%E4%BB%B6.js", ("", ""), 200, HTML, "100", "", 100),
    ];
    for (method, path, (name, value), status, content_type, length, range, body_length) in cases {
        let mut request = Req::new(method, path);
        if !name.is_empty() {
            request = request.header(name, value);
        }
        let reply = serve(&handler, request).await;
        let label = format!("{method} {path}");
        assert_eq!(reply.status, status, "{label}");
        let types: Vec<_> = reply.headers.get_all("content-type").iter().collect();
        assert_eq!(types.len(), 1, "{label}: Content-Type {types:?}");
        assert_eq!(
            types[0].to_str().unwrap_or_default(),
            content_type,
            "{label}"
        );
        if !length.is_empty() {
            assert_eq!(reply.header("content-length"), length, "{label}");
        }
        assert_eq!(reply.header("content-range"), range, "{label}");
        assert_eq!(reply.body.len(), body_length, "{label}");
        for header in ["last-modified", "cache-control", "etag"] {
            assert_eq!(reply.header(header), "", "{label}: unexpected {header}");
        }
        // Security headers belong to served files only, not the JSON 404.
        let served = status != 404;
        for (header, want) in [
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
        ] {
            assert_eq!(
                reply.header(header),
                if served { want } else { "" },
                "{label}: {header}"
            );
        }
    }
}

#[tokio::test]
async fn static_handler_refuses_traversal() {
    let handler = frontend(fixture());
    for target in [
        "/../go.mod",
        "/../../go.mod",
        "/assets/../../go.mod",
        "/..%2fgo.mod",
        "/./../package.json",
        "//../go.mod",
    ] {
        assert_eq!(
            serve(&handler, Req::get(target)).await.status,
            404,
            "GET {target}"
        );
    }
}

#[tokio::test]
async fn static_handler_without_index_html() {
    // Nothing can be served: everything is a 404.
    let handler = frontend(MemoryAssets::new().with(".gitkeep", Vec::new()));
    for target in ["/", "/host", "/assets/app.js", "/.gitkeep"] {
        assert_eq!(
            serve(&handler, Req::get(target)).await.status,
            404,
            "GET {target}"
        );
    }
}

#[test]
fn looks_like_file_matches_sirv_ignore() {
    for (pathname, want) in [
        ("/", false),
        ("/host", false),
        ("/room/1234", false),
        ("/assets", false),
        ("/assets/", false),
        ("/index.html", true),
        ("/assets/index-abc123.js", true),
        ("/a.b.c.js", true),
        ("/favicon.ico", true),
        ("/my file.txt", true),
        ("/.gitkeep", false),
        ("/x.", false),
        ("/.env", false),
        ("/a.b-c", false),
        ("/文件.js", false),
        ("/café.css", false),
        ("/v1.2/thing", false),
    ] {
        assert_eq!(looks_like_file(pathname), want, "{pathname}");
    }
}

#[test]
fn dot_segments_are_hidden_except_well_known() {
    for (name, want) in [
        ("index.html", false),
        ("assets/app.js", false),
        (".gitkeep", true),
        (".vite/manifest.json", true),
        ("assets/.hidden", true),
        (".well-known/x.txt", false),
        ("deep/.well-known/x.txt", false),
        ("well-known/x.txt", false),
        (".well-knownish/file.txt", true),
    ] {
        assert_eq!(has_dot_segment(name), want, "{name}");
    }
}

#[test]
fn directory_assets_stay_inside_their_root() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("assets")).expect("assets dir");
    std::fs::write(root.path().join("assets").join("app.js"), b"ok").expect("asset");
    let assets = crate::DirAssets::new(root.path());
    assert_eq!(
        assets.read("assets/app.js").as_deref(),
        Some(b"ok".as_slice())
    );
    for escape in [
        "assets",
        "../secret",
        "assets/../assets/app.js",
        "C:/x",
        "a\\b",
        "",
    ] {
        assert!(assets.read(escape).is_none(), "{escape}");
    }
}
