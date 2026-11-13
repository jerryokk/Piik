//! `piik-server.exe`, the Hosted Piik entry point: it loads the environment,
//! serves the embedded Browser UI with the signaling and room API, and stops
//! on Ctrl+C, Ctrl+Break, console close or system shutdown.
//!
//! `--check-config` validates the environment and exits without serving.
//! Exit codes: 0 after an orderly stop, 1 for a startup or runtime failure,
//! 2 for invalid command-line flags.

#![forbid(unsafe_code)]

mod environment;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use piik_diagnostics::{Recorder, RedactingLayer};
use piik_service::{Options, Server};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::environment::{
    environment_file, process_environment, server_debug_enabled, server_log_directory,
};

/// Bounds the ordered stop (signaling, listener, room store), well inside the
/// time a service manager allows before it terminates the process.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Read from the working directory; a service keeps its real values in the
/// service manager's environment.
const ENVIRONMENT_FILE: &str = ".env";

/// The packager injects the product version and full source revision.
const BUILD_VERSION: &str = match option_env!("PIIK_RELEASE") {
    Some(value) => value,
    None => "development",
};
const BUILD_REVISION: &str = match option_env!("PIIK_REVISION") {
    Some(value) => value,
    None => "development",
};

const USAGE: &str = "Usage of piik-server:\n  \
    -check-config\n    \tvalidate the environment and exit\n  \
    -debug\n    \tsave opt-in server diagnostics to rotated files\n";

#[derive(Debug, Default, PartialEq, Eq)]
struct Flags {
    check_config: bool,
    debug: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum FlagError {
    Help,
    Invalid(String),
}

/// Flag syntax: `-name` or `--name`, booleans optionally `=value`;
/// parsing stops at `--` or the first non-flag argument.
fn parse_flags(arguments: impl IntoIterator<Item = String>) -> Result<Flags, FlagError> {
    let mut flags = Flags::default();
    for argument in arguments {
        if argument == "--" || !argument.starts_with('-') || argument == "-" {
            break;
        }
        let name = argument.trim_start_matches('-');
        let (name, value) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (name, None),
        };
        let enabled = match value {
            None => true,
            Some("1" | "t" | "T" | "true" | "TRUE" | "True") => true,
            Some("0" | "f" | "F" | "false" | "FALSE" | "False") => false,
            Some(other) => {
                return Err(FlagError::Invalid(format!(
                    "invalid boolean value {other:?} for -{name}: parse error"
                )));
            }
        };
        match name {
            "check-config" => flags.check_config = enabled,
            "debug" => flags.debug = enabled,
            "h" | "help" => return Err(FlagError::Help),
            _ => {
                return Err(FlagError::Invalid(format!(
                    "flag provided but not defined: -{name}"
                )));
            }
        }
    }
    Ok(flags)
}

fn main() -> ExitCode {
    let flags = match parse_flags(std::env::args().skip(1)) {
        Ok(flags) => flags,
        Err(FlagError::Help) => {
            eprint!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(FlagError::Invalid(message)) => {
            eprintln!("{message}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&flags) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(flags: &Flags) -> Result<(), String> {
    let assignments = environment_file(Path::new(ENVIRONMENT_FILE), |name| {
        std::env::var_os(name).is_some()
    })
    .map_err(|error| format!("open {ENVIRONMENT_FILE}: {error}"))?;
    let mut environment = process_environment();
    environment.extend(assignments);
    if flags.check_config {
        piik_config::load(&environment).map_err(|error| error.to_string())?;
        println!("config=ok");
        return Ok(());
    }
    let debug = flags.debug
        || server_debug_enabled(environment.get("PIIK_DEBUG").map_or("", String::as_str));
    let recorder = if debug {
        let directory = server_log_directory(
            environment.get("PIIK_LOG_DIR").map(String::as_str),
            environment.get("LOGS_DIRECTORY").map(String::as_str),
        );
        Some(
            Recorder::open(&directory, "server", BUILD_REVISION)
                .map_err(|error| format!("Piik server diagnostics are unavailable: {error}"))?,
        )
    } else {
        None
    };
    // Normal service notices stay on the console; the recorder keeps DEBUG
    // and higher levels, including dependency records.
    let subscriber = Registry::default()
        .with(RedactingLayer::console(std::io::stderr))
        .with(recorder.as_ref().map(Recorder::layer));
    let _ = tracing::subscriber::set_global_default(subscriber);
    if let Some(recorder) = &recorder {
        eprintln!("Piik diagnostic log: {}", recorder.log_path().display());
        tracing::info!(
            event = "start",
            version = BUILD_VERSION,
            revision = BUILD_REVISION,
            "piik-server"
        );
    }

    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| format!("Piik server runtime: {error}"))?;
    let served = runtime.block_on(serve(environment, recorder.as_ref()));
    match &served {
        Ok(()) => tracing::info!(event = "stopped", failed = false, "piik-server"),
        Err(error) => {
            tracing::info!(event = "stopped", failed = true, error = %error, "piik-server")
        }
    }
    drop(runtime);
    let exported = recorder.map(|recorder| export_diagnostics(&recorder));
    served?;
    exported.unwrap_or(Ok(()))
}

/// Runs until the first stop request, then stops with [`Server::close`]:
/// durable room authority survives a restart.
async fn serve(
    environment: std::collections::HashMap<String, String>,
    recorder: Option<&Recorder>,
) -> Result<(), String> {
    let config = piik_config::load(&environment).map_err(|error| error.to_string())?;
    if let Some(recorder) = recorder {
        recorder.context(
            "configuration",
            serde_json::json!({
                "environment": format!("{:?}", config.env).to_lowercase(),
                "port": config.port,
                "sqlite": config.room_database_path.is_some(),
                "siteAccessProtected": !config.site_access_password.is_empty(),
                "maxViewers": config.max_viewers_per_room,
                "endpointCapacity": config.endpoint_media_copy_capacity,
                "sfu": config.sfu.as_ref().map(|sfu| serde_json::json!({
                    "listenHost": sfu.listen_host.to_string(),
                    "port": sfu.port,
                    "publicIP": sfu.public_ip,
                    "only": sfu.only,
                })),
                "stunURLs": config.stun_urls,
                "natPrediction": config.nat_prediction_enabled,
            }),
        );
    }
    let tls = config
        .tls
        .as_ref()
        .map(piik_tls::direct)
        .transpose()
        .map_err(|error| error.to_string())?;
    let server = Server::new(Options {
        assets: piik_service::webassets::embedded().map(|assets| Arc::new(assets) as _),
        tls: tls.clone(),
        config: config.clone(),
        ..Options::default()
    })
    .map_err(|error| error.to_string())?;
    // Ordered shutdown is installed after startup; a stop during startup
    // keeps the operating system's default handling.
    let port = server.listen().await.map_err(|error| error.to_string())?;
    tracing::info!(
        event = "ready",
        version = BUILD_VERSION,
        revision = BUILD_REVISION,
        host = %config.listen_host,
        port,
        publicUrl = %piik_config::origin(&config.public_base_url),
        sqlite = config.room_database_path.is_some(),
        siteAccessProtected = !config.site_access_password.is_empty(),
        sfu = config.sfu.is_some(),
        natPrediction = config.nat_prediction_enabled,
        "Piik server is listening"
    );
    if let Some(tls) = &tls {
        tracing::info!(sha256 = tls.fingerprint(), "HTTPS certificate");
    }
    let failure = tokio::select! {
        () = stop_requested() => None,
        error = server.failed() => Some(error),
    };
    let closed = server.close(SHUTDOWN_TIMEOUT).await;
    let mut errors: Vec<String> = failure.into_iter().map(|error| error.to_string()).collect();
    if let Err(error) = closed {
        errors.push(error.to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn export_diagnostics(recorder: &Recorder) -> Result<(), String> {
    let exported = recorder.export();
    match &exported {
        Ok(path) => eprintln!("Piik diagnostic bundle: {}", path.display()),
        Err(error) => eprintln!("Piik diagnostic export failed: {error}"),
    }
    let closed = recorder.close();
    exported
        .map(drop)
        .and(closed)
        .map_err(|error| error.to_string())
}

/// The first Ctrl+C, Ctrl+Break, console close or system shutdown event.
async fn stop_requested() {
    use tokio::signal::windows;
    let (Ok(mut c), Ok(mut brk), Ok(mut close), Ok(mut shutdown)) = (
        windows::ctrl_c(),
        windows::ctrl_break(),
        windows::ctrl_close(),
        windows::ctrl_shutdown(),
    ) else {
        // Without handlers the default termination still applies.
        return std::future::pending().await;
    };
    tokio::select! {
        _ = c.recv() => {},
        _ = brk.recv() => {},
        _ = close.recv() => {},
        _ = shutdown.recv() => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Flags, FlagError> {
        parse_flags(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn flags_follow_go_flag_syntax() {
        assert_eq!(parse(&[]), Ok(Flags::default()));
        for arguments in [
            &["-check-config"][..],
            &["--check-config"],
            &["-check-config=true"],
        ] {
            assert_eq!(
                parse(arguments),
                Ok(Flags {
                    check_config: true,
                    debug: false
                }),
                "{arguments:?}"
            );
        }
        assert_eq!(parse(&["--debug", "-debug=false"]), Ok(Flags::default()));
        assert_eq!(
            parse(&["-debug", "extra", "-unknown"]),
            Ok(Flags {
                check_config: false,
                debug: true
            })
        );
        assert_eq!(parse(&["--", "-unknown"]), Ok(Flags::default()));
        assert_eq!(parse(&["-h"]), Err(FlagError::Help));
        assert!(
            matches!(parse(&["-check-release"]), Err(FlagError::Invalid(_))),
            "there is no update check"
        );
        assert!(matches!(
            parse(&["-debug=maybe"]),
            Err(FlagError::Invalid(_))
        ));
    }

    #[derive(Clone, Default)]
    struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer").extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
        type Writer = Buffer;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn console_keeps_service_notices_while_diagnostics_record() {
        let directory = tempfile::tempdir().expect("tempdir");
        let recorder = Recorder::open(directory.path(), "server", "test").expect("recorder");
        let buffer = Buffer::default();
        let subscriber = Registry::default()
            .with(RedactingLayer::console(buffer.clone()))
            .with(Some(recorder.layer()));
        // A global dispatcher caches callsite interest, as the binary's does;
        // no other test in this binary installs one.
        tracing::subscriber::set_global_default(subscriber).expect("global subscriber");
        tracing::info!(event = "start", "piik-server");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // Signaling probes `enabled!(DEBUG)` at startup; neither that
            // probe nor recorder-only records may suppress the next notice.
            tracing::debug!(target: "piik_signal", "detail");
            tracing::info!(target: "hyper", "dependency");
            let _ = tracing::enabled!(tracing::Level::DEBUG);
            tracing::info!(event = "ready", port = 1_u16, "Piik server is listening");
        });
        let text = String::from_utf8_lossy(&buffer.0.lock().expect("buffer")).into_owned();
        assert!(
            text.contains("\"start\"") && text.contains("Piik server is listening"),
            "{text}"
        );
        assert!(
            !text.contains("detail") && !text.contains("dependency"),
            "the console admitted debug or dependency records: {text}"
        );
        recorder.close().expect("close recorder");
    }
}
