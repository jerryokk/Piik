//! The Piik Windows App: one WebView2 main window around the embedded Piik
//! runtime (`docs/architecture/system.md`). The window opens on the launcher URL and navigates in
//! place to the room or an approved Site; every other web destination opens in
//! the system browser. A second launch focuses this window.

// No console in any build; diagnostics go to the opt-in rotating log.
#![windows_subsystem = "windows"]

mod app_runtime;
mod build_info;
mod diagnostics;
mod gate;
mod options;
mod shell;

use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use url::Url;

use crate::diagnostics::Diagnostics;
use crate::options::Options;
use crate::shell::{AllowedOrigins, AppRuntime, RuntimeContext, ShellHandle, WebAssets};

/// The one runtime implementation; nothing else in the shell names it.
type Runtime = app_runtime::PiikRuntime;

const MAIN_WINDOW: &str = "main";
/// Bounds the ordered runtime stop: the room's own 5 s end, then the
/// launcher (3 s) and loopback control (2 s) bounds.
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(12);

/// Shell state shared by the event loop, window handlers and second launches.
struct Shell {
    runtime: Mutex<Option<Runtime>>,
    origins: AllowedOrigins,
    stopping: AtomicBool,
    /// The exit code chosen by `stop`.
    code: AtomicI32,
    /// A navigation requested before the window exists replaces its entry.
    window: Mutex<Window>,
}

enum Window {
    Pending(Option<Url>),
    Open,
}

fn main() -> ExitCode {
    let options = match Options::parse_from_env(std::env::args_os()) {
        Ok(options) => options,
        Err(error) => {
            let _ = error.print();
            return if error.use_stderr() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    let diagnostics = Diagnostics::new(options.log_dir.clone());
    if options.debug
        && let Err(error) = diagnostics.enable()
    {
        eprintln!("{error:#}");
        return ExitCode::FAILURE;
    }
    tracing::debug!(
        event = "start",
        version = build_info::RELEASE,
        revision = build_info::REVISION,
        "piik-client"
    );

    // One embedding of `frontendDist`, shared by the window and gate paths.
    let context = tauri::generate_context!();
    if options.gate {
        let failed = gate::run(options, diagnostics.clone(), context.assets);
        diagnostics.finish(failed);
        return if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }

    let shell = Shell {
        runtime: Mutex::new(None),
        origins: AllowedOrigins::default(),
        stopping: AtomicBool::new(false),
        code: AtomicI32::new(0),
        window: Mutex::new(Window::Pending(None)),
    };
    let runtime_diagnostics = diagnostics.clone();
    let app = tauri::Builder::default()
        // The single-instance plugin must be registered first.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            focus_main_window(app);
            if let Some(runtime) = lock(&app.state::<Shell>().runtime).as_ref() {
                runtime.activate(argv);
            }
        }))
        .manage(shell)
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(start(handle, options, runtime_diagnostics));
            Ok(())
        })
        .build(context);
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            tracing::error!(error = %error, "Piik App could not start");
            diagnostics.finish(true);
            return ExitCode::FAILURE;
        }
    };

    let exit_code = app.run_return(|app, event| match event {
        // Closing the window (or any exit request) stops the runtime first.
        RunEvent::ExitRequested { api, .. } => {
            let shell = app.state::<Shell>();
            if !shell.stopping.swap(true, Ordering::SeqCst) {
                api.prevent_exit();
                tauri::async_runtime::spawn(stop(app.clone()));
            }
        }
        RunEvent::Exit => tracing::debug!(event = "exit", "piik-client"),
        _ => {}
    });
    // `stop` and `fail` choose the exit code; run_return reports it.
    let failed = exit_code != 0;
    diagnostics.finish(failed);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Runtime requests: in-place navigation (any thread) and a self-ended run.
fn shell_handle(app: &AppHandle) -> ShellHandle {
    let navigate = app.clone();
    let exit = app.clone();
    ShellHandle::new(
        move |url| {
            let shell = navigate.state::<Shell>();
            let mut window = lock(&shell.window);
            match &mut *window {
                Window::Pending(pending) => *pending = Some(url),
                Window::Open => {
                    if let Some(window) = navigate.get_webview_window(MAIN_WINDOW)
                        && let Err(error) = window.navigate(url)
                    {
                        tracing::debug!(error = %error, "Could not navigate the App window");
                    }
                }
            }
        },
        move |failed| {
            let shell = exit.state::<Shell>();
            if failed {
                shell.code.store(1, Ordering::SeqCst);
            }
            if !shell.stopping.swap(true, Ordering::SeqCst) {
                tauri::async_runtime::spawn(stop(exit.clone()));
            }
        },
    )
}

/// Starts the runtime, then opens the hidden main window on its entry URL.
async fn start(app: AppHandle, options: Options, diagnostics: Diagnostics) {
    let shell = app.state::<Shell>();
    let context = RuntimeContext {
        options,
        assets: WebAssets::new(&app),
        origins: shell.origins.clone(),
        shell: shell_handle(&app),
        diagnostics,
    };
    let entry = match Runtime::start(context).await {
        Ok((runtime, entry)) => {
            *lock(&shell.runtime) = Some(runtime);
            entry
        }
        Err(error) => return fail(&app, "Piik App runtime could not start", &error),
    };
    if shell.stopping.load(Ordering::SeqCst) {
        // An exit raced startup; `stop` may already have run without a runtime.
        if let Some(runtime) = take_runtime(&app) {
            runtime.shutdown().await;
        }
        return;
    }
    // Holding the slot orders this open against runtime navigations.
    let mut window = lock(&shell.window);
    let entry = match &mut *window {
        Window::Pending(pending) => pending.take().unwrap_or(entry),
        Window::Open => entry,
    };
    match open_main_window(&app, entry) {
        Ok(()) => *window = Window::Open,
        Err(error) => {
            drop(window);
            fail(&app, "Piik App window could not open", &error.into());
        }
    }
}

/// Must stay byte-identical to `appShellInitScript` in app-shell.ts.
fn app_shell_marker() -> String {
    let version = serde_json::Value::from(build_info::RELEASE).to_string();
    format!(
        r#"Object.defineProperty(window,"__PIIK_APP__",{{value:Object.freeze({{version:{version}}}),enumerable:false,configurable:false,writable:false}});"#
    )
}

fn open_main_window(app: &AppHandle, entry: Url) -> tauri::Result<()> {
    let navigation = app.state::<Shell>().origins.clone();
    let new_window = navigation.clone();
    let handle = app.clone();
    WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::External(entry))
        .title("Piik")
        .inner_size(1280.0, 820.0)
        .min_inner_size(880.0, 600.0)
        .center()
        // Shown on the first finished page load, so no blank frame appears.
        .visible(false)
        // Presentation marker for src/client/lib/app-shell.ts. Only allowed
        // origins ever load in this window; authority stays with loopback.
        .initialization_script(app_shell_marker())
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Finished && !window.is_visible().unwrap_or(true) {
                let _ = window.show();
                let _ = window.set_focus();
            }
        })
        .on_navigation(move |url| {
            if navigation.allows(url) {
                return true;
            }
            open_externally(url);
            false
        })
        // `target=_blank` and `window.open`: never a second App window.
        .on_new_window(move |url, _features| {
            if new_window.allows(&url) {
                if let Some(window) = handle.get_webview_window(MAIN_WINDOW) {
                    let _ = window.navigate(url);
                }
            } else {
                open_externally(&url);
            }
            NewWindowResponse::Deny
        })
        .build()?;
    Ok(())
}

/// Web destinations outside the App-owned and approved origins.
fn open_externally(url: &Url) {
    if matches!(url.scheme(), "http" | "https") {
        if let Err(error) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
            tracing::debug!(error = %error, "Could not open the system browser");
        }
    } else {
        tracing::debug!(scheme = url.scheme(), "Blocked a non-web navigation");
    }
}

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Stops the runtime within [`SHUTDOWN_TIMEOUT`], then exits the event loop.
async fn stop(app: AppHandle) {
    let shell = app.state::<Shell>();
    if let Some(runtime) = take_runtime(&app)
        && tokio::time::timeout(SHUTDOWN_TIMEOUT, runtime.shutdown())
            .await
            .is_err()
    {
        tracing::warn!("Piik App runtime did not stop within its bound");
        shell.code.store(1, Ordering::SeqCst);
    }
    app.exit(shell.code.load(Ordering::SeqCst));
}

fn fail(app: &AppHandle, message: &str, error: &anyhow::Error) {
    tracing::error!(error = format!("{error:#}"), "{message}");
    app.state::<Shell>().stopping.store(true, Ordering::SeqCst);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(runtime) = take_runtime(&app) {
            runtime.shutdown().await;
        }
        app.exit(1);
    });
}

fn take_runtime(app: &AppHandle) -> Option<Runtime> {
    lock(&app.state::<Shell>().runtime).take()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod shell_marker_tests {
    /// The frontend owns the marker contract; this keeps both producers equal.
    #[test]
    fn marker_matches_the_frontend_contract() {
        let source = include_str!("../../../src/client/lib/app-shell.ts");
        let documented = source
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .find(|line| line.starts_with("Object.defineProperty(window,\"__PIIK_APP__\""))
            .expect("app-shell.ts documents the 3.0.0 script");
        let expected = documented.replace(
            "\"3.0.0\"",
            &serde_json::Value::from(super::build_info::RELEASE).to_string(),
        );
        assert_eq!(super::app_shell_marker(), expected);
    }
}
