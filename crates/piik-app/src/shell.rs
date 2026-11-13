//! The boundary between the Tauri shell and the embedded Piik runtime.
//!
//! The shell owns the process, the single main window and its navigation
//! policy. A runtime owns everything behind the launcher URL: the loopback v10
//! native control service, the launcher HTTP server, and Local/public-link
//! rooms. `main.rs` selects the implementation in one place.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, PoisonError, RwLock};

use anyhow::Result;
use tauri::{AppHandle, Wry};
use url::{Origin, Url};

use crate::diagnostics::Diagnostics;
use crate::options::Options;

/// What the shell hands a runtime at startup.
pub struct RuntimeContext {
    pub options: Options,
    /// The Vite build embedded in the App executable.
    pub assets: WebAssets,
    /// Origins the main window may load in place; the runtime keeps it current.
    pub origins: AllowedOrigins,
    /// Window navigation and exit requests back to the shell.
    pub shell: ShellHandle,
    /// The opt-in diagnostics the launcher can enable later.
    pub diagnostics: Diagnostics,
}

/// Requests a runtime makes of the shell. Both are safe from any thread.
#[derive(Clone)]
pub struct ShellHandle {
    navigate: Arc<dyn Fn(Url) + Send + Sync>,
    exit: Arc<dyn Fn(bool) + Send + Sync>,
}

impl ShellHandle {
    pub fn new(
        navigate: impl Fn(Url) + Send + Sync + 'static,
        exit: impl Fn(bool) + Send + Sync + 'static,
    ) -> Self {
        Self {
            navigate: Arc::new(navigate),
            exit: Arc::new(exit),
        }
    }

    /// Loads `url` in the main window in place (every former browser open).
    pub fn navigate(&self, url: Url) {
        (self.navigate)(url);
    }

    /// The runtime ended on its own; the shell stops it and exits.
    pub fn exit(&self, failed: bool) {
        (self.exit)(failed);
    }
}

/// The embedded Piik runtime. `start` returns once the launcher (or the
/// approved Site entry) is ready to load; `shutdown` stops every owned
/// resource and is bounded by the shell.
pub trait AppRuntime: Send + Sync + Sized + 'static {
    fn start(context: RuntimeContext) -> impl Future<Output = Result<(Self, Url)>> + Send;

    /// A second App launch focused the existing window; `argv` is its command line.
    fn activate(&self, _argv: Vec<String>) {}

    fn shutdown(self) -> impl Future<Output = ()> + Send;
}

/// Read access to the frontend assets Tauri embeds from `frontendDist`, so a
/// launcher server needs no second copy of the Vite build.
#[derive(Clone)]
pub struct WebAssets(Arc<ReadAsset>);

type ReadAsset = dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync;

impl WebAssets {
    pub fn new(app: &AppHandle) -> Self {
        let resolver: tauri::AssetResolver<Wry> = app.asset_resolver();
        // Tauri's lookup falls back to `index.html`; the launcher and room
        // servers own their fallback rules, so only exact paths resolve.
        let paths: HashSet<String> = resolver
            .iter()
            .map(|(path, _)| path.trim_start_matches('/').to_owned())
            .collect();
        Self(Arc::new(move |path| {
            paths
                .contains(path)
                .then(|| resolver.get(format!("/{path}")))
                .flatten()
                .map(|asset| asset.bytes)
        }))
    }

    /// The same embedded `frontendDist` without a Tauri app (gate mode).
    /// Embedded keys are exact, so there is no `index.html` fallback here either.
    pub fn embedded(assets: Box<dyn tauri::Assets<Wry>>) -> Self {
        Self(Arc::new(move |path| {
            assets
                .get(&tauri::utils::assets::AssetKey::from(format!("/{path}")))
                .map(std::borrow::Cow::into_owned)
        }))
    }

    /// Assets from any lookup, such as a test table.
    #[cfg(test)]
    pub fn from_fn(read: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self(Arc::new(read))
    }

    /// The exact file at a relative path such as `assets/index.js`.
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        (self.0)(path)
    }
}

/// The runtime-owned origin list for in-place navigation: the launcher, the
/// Local room and approved Sites. Anything else opens in the system browser.
#[derive(Clone, Default)]
pub struct AllowedOrigins(Arc<RwLock<Vec<Origin>>>);

impl AllowedOrigins {
    pub fn set(&self, urls: &[Url]) {
        let origins = urls.iter().map(Url::origin).collect();
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = origins;
    }

    pub fn allows(&self, url: &Url) -> bool {
        let origin = url.origin();
        origin.is_tuple()
            && self
                .0
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_listed_origins() {
        let origins = AllowedOrigins::default();
        origins.set(&[
            Url::parse("http://127.0.0.1:4100/client").unwrap(),
            Url::parse("https://demo.piik.tv/").unwrap(),
        ]);
        assert!(origins.allows(&Url::parse("http://127.0.0.1:4100/join").unwrap()));
        assert!(origins.allows(&Url::parse("https://demo.piik.tv/#piik-client=1").unwrap()));
        assert!(!origins.allows(&Url::parse("http://127.0.0.1:4101/client").unwrap()));
        assert!(!origins.allows(&Url::parse("http://demo.piik.tv/").unwrap()));
        assert!(!origins.allows(&Url::parse("https://github.com/").unwrap()));
        assert!(!origins.allows(&Url::parse("about:blank").unwrap()));
    }
}
