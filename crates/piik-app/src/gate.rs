//! Headless gate mode (`PIIK_CLIENT_GATE_NO_BROWSER=true`) for gate scripts:
//! no window or event loop, the runtime on its own Tokio runtime, the
//! endpoint and readiness lines on stdout (printed by the runtime), and a
//! bounded stop on any stdin byte, stdin EOF, Ctrl+C or a self-ended run.
//!
//! The executable keeps `windows_subsystem = "windows"`: a parent that passes
//! pipe handles (Node `stdio: "pipe"`) still gives the process working
//! stdin/stdout/stderr through `STARTF_USESTDHANDLES`.

use std::io::Read as _;

use tauri::Wry;
use tokio::sync::{mpsc, oneshot};

use crate::diagnostics::Diagnostics;
use crate::options::Options;
use crate::shell::{AllowedOrigins, AppRuntime, RuntimeContext, ShellHandle, WebAssets};
use crate::{Runtime, SHUTDOWN_TIMEOUT};

/// Runs until stopped; returns whether the run failed.
pub fn run(
    options: Options,
    diagnostics: Diagnostics,
    assets: Box<dyn tauri::Assets<Wry>>,
) -> bool {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Piik App runtime could not start: {error}");
            return true;
        }
    };
    let assets = WebAssets::embedded(assets);
    runtime.block_on(serve(options, diagnostics, assets))
}

async fn serve(options: Options, diagnostics: Diagnostics, assets: WebAssets) -> bool {
    let (exit, mut exited) = mpsc::unbounded_channel();
    let context = RuntimeContext {
        options,
        assets,
        origins: AllowedOrigins::default(),
        // There is no window: the runtime prints readiness itself.
        shell: ShellHandle::new(
            |_url| {},
            move |failed| {
                let _ = exit.send(failed);
            },
        ),
        diagnostics,
    };
    let runtime = match Runtime::start(context).await {
        Ok((runtime, _entry)) => runtime,
        Err(error) => {
            tracing::error!(
                error = format!("{error:#}"),
                "Piik App runtime could not start"
            );
            // Display alone: a Failure's cause is also its source.
            eprintln!("{error}");
            return true;
        }
    };
    let failed = tokio::select! {
        () = stdin_stop() => false,
        () = ctrl_c() => false,
        failed = exited.recv() => failed.unwrap_or(true),
    };
    let timed_out = tokio::time::timeout(SHUTDOWN_TIMEOUT, runtime.shutdown())
        .await
        .is_err();
    if timed_out {
        tracing::warn!("Piik App runtime did not stop within its bound");
        eprintln!("Piik App runtime did not stop within its bound");
    }
    failed || timed_out
}

/// Resolves on the first stdin byte or EOF. The reader is a detached thread:
/// a blocked console read must never hold up process exit.
async fn stdin_stop() {
    let (done, stopped) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("piik-gate-stdin".to_owned())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            let _ = std::io::stdin().lock().read(&mut byte);
            let _ = done.send(());
        });
    if spawned.is_err() {
        return std::future::pending().await;
    }
    let _ = stopped.await;
}

async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}
