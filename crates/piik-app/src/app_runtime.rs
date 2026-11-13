//! The embedded Piik runtime behind the App window:
//!
//! 1. `client.json` load (and `--site` approval), native media discovery,
//!    remote input and the sharing-quality preference.
//! 2. Loopback v10 native control on 39721..=39730, then the launcher on
//!    `127.0.0.1:0`; Site approval updates both the control origins and the
//!    window navigation allow-list.
//! 3. One serial room owner: a launcher selection (or `--local`/`--link`)
//!    starts a Local or public-link room, reports its target or failure
//!    reason to the launcher, and waits for it to end.
//!
//! Every former system-browser open navigates the main window in place.
//! Shutdown stops the room (ending rooms, bounded 5 s, then the tunnel),
//! then the launcher, then loopback control, then remote input.

mod native;
mod policy;
mod room;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use anyhow::{Context, Result};
use piik_app_config::{AppConfig, QualityPreference};
use piik_failure::{Failure, Reason};
use piik_launcher::{BoxError, ChangeSite, Mode, Selection};
use piik_loopback::CancellationToken;
use piik_remote_input::Manager;
use tokio::task::JoinHandle;
use url::Url;

use crate::diagnostics::Diagnostics;
use crate::options::{DEFAULT_LOCAL_PORT, Options};
use crate::shell::{AllowedOrigins, AppRuntime, RuntimeContext, ShellHandle, WebAssets};

use policy::Entry;
use room::{Ended, RoomOptions};

pub struct PiikRuntime {
    /// Stops the room owner; loopback and launcher close explicitly after it.
    rooms_stop: CancellationToken,
    rooms: JoinHandle<()>,
    launcher: piik_launcher::Server,
    control: piik_loopback::Server,
    input: Option<Arc<Manager>>,
}

impl AppRuntime for PiikRuntime {
    async fn start(context: RuntimeContext) -> Result<(Self, Url)> {
        // Zero keeps the published v10 range 39721..=39730; only gate mode
        // reads a test override.
        let port = context.options.gate_control_port;
        Self::start_on(context, port).await
    }

    async fn shutdown(self) {
        self.rooms_stop.cancel();
        let _ = self.rooms.await;
        if let Err(error) = self.launcher.close().await {
            tracing::debug!(event = "launcher-close-failed", error = %error, "piik-client");
        }
        self.control.set_connect_url("");
        if let Err(error) = self.control.close().await {
            tracing::debug!(event = "control-close-failed", error = %error, "piik-client");
        }
        if let Some(input) = &self.input {
            input.close();
        }
    }
}

impl PiikRuntime {
    /// `control_port` pins native control to one port; tests use it where
    /// Windows reserves the published range.
    async fn start_on(context: RuntimeContext, control_port: u16) -> Result<(Self, Url)> {
        let RuntimeContext {
            mut options,
            assets,
            origins,
            shell,
            diagnostics,
        } = context;
        if options.port == 0 {
            options.port = DEFAULT_LOCAL_PORT;
        }
        let configuration = |error: &dyn std::fmt::Display| {
            Failure::new(
                Reason::Configuration,
                format!("Piik App configuration is unavailable: {error}"),
            )
        };
        let config_path = match &options.config {
            Some(path) => path.clone(),
            None => piik_app_config::default_path().map_err(|error| configuration(&error))?,
        };
        let mut config =
            piik_app_config::load_or_create(&config_path).map_err(|error| configuration(&error))?;
        if let Some(site) = &options.site {
            policy::approve_site(&mut config, site)
                .map_err(|error| Failure::new(Reason::Configuration, error))?;
            piik_app_config::save(&config_path, &config).map_err(|error| configuration(&error))?;
        }
        tracing::debug!(
            event = "configuration",
            site_configured = !config.sites.is_empty(),
            local_access_protected = !config.local_access_password.is_empty(),
            "piik-client"
        );

        let native = native::discover(options.capture_process.as_deref()).await;
        let input = native::remote_input();
        let capabilities = native.capabilities(input.as_deref());
        tracing::debug!(
            event = "native-capabilities",
            video = capabilities.video,
            process_audio = capabilities.process_audio,
            system_audio = capabilities.system_audio,
            hardware_h264 = capabilities.hardware_h264,
            software_vp8 = capabilities.software_vp8,
            error = native.discovery.as_deref().unwrap_or_default(),
            "piik-client"
        );
        diagnostics.binary("captureExecutable", &native.executable);
        let (quality, loaded) = QualityPreference::load(&config_path);
        if let Err(error) = loaded {
            tracing::debug!(event = "quality-preference-load-failed", error = %error, "piik-client");
        }

        let sites = config.sites.clone();
        let control = piik_loopback::Server::start(
            &CancellationToken::new(),
            piik_loopback::Options {
                allowed_origins: policy::control_origins(&sites, options.port, None),
                native_media: capabilities,
                new_control: Some(native.control_factory(input.clone())),
                quality_preference: Some(Arc::new(quality)),
                port_start: control_port,
                port_end: control_port,
                ..piik_loopback::Options::default()
            },
        )
        .map_err(|error| {
            Failure::new(
                Reason::NativeControl,
                format!("Piik App native control could not start: {error}"),
            )
        })?;
        tracing::debug!(event = "control-ready", "piik-client");
        if options.gate {
            // The endpoint is the first gate stdout line, before the launcher.
            // Four plain fields always serialize.
            let endpoint = serde_json::to_string(&control.endpoint()).unwrap_or_default();
            gate_print(&[endpoint]);
        }

        let shared = Arc::new(Shared {
            config_path: config_path.clone(),
            config: Mutex::new(config.clone()),
            port: options.port,
            control: control.clone(),
            launcher: OnceLock::new(),
            origins,
        });
        let started = start_launcher(&shared, &options, &assets, &diagnostics, &sites).await;
        let (launcher, launcher_url, startup_site) = match started {
            Ok(started) => started,
            Err(error) => {
                let _ = control.close().await;
                if let Some(input) = &input {
                    input.close();
                }
                return Err(error);
            }
        };

        let entry = policy::entry(&options, &config, &startup_site);
        let (initial, direct) = match &entry {
            Entry::Launcher => (launcher_url.clone(), None),
            Entry::Site(site) => {
                tracing::debug!(event = "mode", mode = "site", "piik-client");
                if options.gate {
                    gate_print(&[format!("Piik Site: {site}")]);
                }
                let target = piik_launcher::entry_url(&format!("{site}/"), control.endpoint().port);
                // Approved Sites are normalized origins, so this parses.
                (
                    Url::parse(&target).unwrap_or_else(|_| launcher_url.clone()),
                    None,
                )
            }
            // The launcher shows the starting room until its target is ready.
            Entry::Room(mode) => (launcher_url.clone(), Some(*mode)),
        };
        let rooms_stop = CancellationToken::new();
        let owner = Rooms {
            shared,
            launcher: launcher.clone(),
            launcher_url,
            options,
            assets: Arc::new(Embedded(assets)),
            diagnostics,
            shell,
            stop: rooms_stop.clone(),
        };
        let rooms = tokio::spawn(owner.run(direct));
        Ok((
            Self {
                rooms_stop,
                rooms,
                launcher,
                control,
                input,
            },
            initial,
        ))
    }
}

/// Gate-mode stdout lines, flushed at once for line readers.
/// A missing or closed stdout is not an App failure.
fn gate_print(lines: &[String]) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    for line in lines {
        let _ = writeln!(stdout, "{line}");
    }
    let _ = stdout.flush();
}

/// Starts the launcher, publishes discovery and the launcher origin, and
/// returns it with its URL and the restored startup Site.
async fn start_launcher(
    shared: &Arc<Shared>,
    options: &Options,
    assets: &WebAssets,
    diagnostics: &Diagnostics,
    sites: &[String],
) -> Result<(piik_launcher::Server, Url, String)> {
    let path = &shared.config_path;
    let mode = piik_launcher::load_mode(path).unwrap_or_else(|error| {
        tracing::debug!(error = %error, "Could not restore the last launcher mode");
        None
    });
    let startup_site = piik_launcher::load_startup_site(path, sites).unwrap_or_else(|error| {
        tracing::debug!(error = %error, "Could not restore the startup Site");
        String::new()
    });
    let changes = shared.clone();
    let change_site: ChangeSite = Arc::new(move |site: String, allowed: bool| {
        let shared = changes.clone();
        Box::pin(async move { shared.change_site(&site, allowed).await })
    });
    let startup_path = path.clone();
    let enable = diagnostics.clone();
    let launcher = piik_launcher::Server::start(
        &CancellationToken::new(),
        Embedded(assets.clone()),
        piik_launcher::Options {
            default_mode: mode,
            sites: sites.to_vec(),
            startup_site: startup_site.clone(),
            version: piik_loopback::buildinfo::version().to_owned(),
            revision: piik_loopback::buildinfo::REVISION.to_owned(),
            control_port: shared.control.endpoint().port,
            local_access_password: shared.config().local_access_password,
            debug: diagnostics.enabled(),
            lan_address: options.lan_address.clone().unwrap_or_default(),
            change_site: Some(change_site),
            save_startup_site: Some(Arc::new(move |site: &str| {
                piik_launcher::save_startup_site(&startup_path, site).map_err(BoxError::from)
            })),
            enable_debug: Some(Arc::new(move || {
                enable.enable().map_err(|error| {
                    Box::new(Failure::new(Reason::Diagnostics, format!("{error:#}"))) as BoxError
                })
            })),
            ..piik_launcher::Options::default()
        },
    )
    .map_err(|error| Failure::new(Reason::Launcher, error))?;
    let url = Url::parse(&launcher.url()).context("Piik App launcher URL is invalid")?;
    let _ = shared.launcher.set(url.clone());
    shared.control.set_connect_url(launcher.url().as_str());
    if let Err(error) = shared.publish(sites).await {
        let _ = launcher.close().await;
        return Err(Failure::new(Reason::NativeControl, error).into());
    }
    tracing::debug!(event = "launcher-ready", "piik-client");
    Ok((launcher, url, startup_site))
}

/// Configuration and approval state shared by the launcher and room owner.
struct Shared {
    config_path: PathBuf,
    /// The last successfully saved configuration.
    config: Mutex<AppConfig>,
    port: u16,
    control: piik_loopback::Server,
    launcher: OnceLock<Url>,
    origins: AllowedOrigins,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, AppConfig> {
        self.config.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn config(&self) -> AppConfig {
        self.lock().clone()
    }

    /// Saves one change and publishes it only once the file holds it.
    fn update(
        &self,
        change: impl FnOnce(&mut AppConfig),
    ) -> Result<AppConfig, piik_app_config::Error> {
        let mut config = self.lock();
        let mut next = config.clone();
        change(&mut next);
        piik_app_config::save(&self.config_path, &next)?;
        *config = next;
        Ok(config.clone())
    }

    /// Applies the approved Sites to the window and to native control.
    async fn publish(&self, sites: &[String]) -> Result<(), piik_loopback::Error> {
        let launcher = self.launcher.get();
        self.origins
            .set(&policy::navigation_urls(launcher, self.port, sites));
        self.control
            .set_allowed_origins(&policy::control_origins(sites, self.port, launcher))
            .await
    }

    /// The launcher serializes approval changes; this returns the approval
    /// actually in force even when part of the change failed.
    async fn change_site(&self, site: &str, allowed: bool) -> (Vec<String>, Result<(), BoxError>) {
        match self.update(|config| policy::change_sites(config, site, allowed)) {
            Err(error) => (self.config().sites, Err(error.into())),
            Ok(config) => {
                let sites = config.sites;
                let result = self.publish(&sites).await.map_err(BoxError::from);
                (sites, result)
            }
        }
    }
}

/// The embedded Vite build for the launcher and the room server.
struct Embedded(WebAssets);

impl piik_launcher::Assets for Embedded {
    fn read(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        self.0.read(path).map(Cow::Owned)
    }
}

impl piik_service::Assets for Embedded {
    fn read(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        self.0.read(path).map(Cow::Owned)
    }
}

/// How one room attempt ended.
enum Outcome {
    /// The owner asked it to stop.
    Stopped,
    /// The resident App outlives a failed room; `shown` means the window
    /// already left the launcher and must return to it.
    Failed { failure: Failure, shown: bool },
    /// Native control stopped: the App cannot continue.
    Exit { failed: bool },
}

/// The serial room owner: one room at a time, retried only after the
/// previous one has fully stopped.
struct Rooms {
    shared: Arc<Shared>,
    launcher: piik_launcher::Server,
    launcher_url: Url,
    options: Options,
    assets: Arc<Embedded>,
    diagnostics: Diagnostics,
    shell: ShellHandle,
    stop: CancellationToken,
}

impl Rooms {
    async fn run(self, mut direct: Option<Mode>) {
        loop {
            let (selection, from_shell) = match direct.take() {
                Some(mode) => {
                    self.launcher.begin();
                    let selection = Selection {
                        mode,
                        language: String::new(),
                        local_access_password: self.shared.config().local_access_password,
                        debug: false,
                        lan_address: None,
                    };
                    (selection, true)
                }
                None => tokio::select! {
                    () = self.stop.cancelled() => return,
                    selection = self.launcher.next_selection() => match selection {
                        Some(selection) => (selection, false),
                        None => {
                            if !self.stop.is_cancelled() {
                                tracing::error!("Piik App launcher stopped unexpectedly");
                                self.shell.exit(true);
                            }
                            return;
                        }
                    },
                    result = self.shared.control.done() => {
                        if let Err(error) = &result {
                            tracing::error!(error = %error, "Piik App native control stopped unexpectedly");
                        }
                        self.shell.exit(result.is_err());
                        return;
                    }
                },
            };
            match self.run_room(selection, from_shell).await {
                Outcome::Stopped => return,
                Outcome::Exit { failed } => {
                    self.shell.exit(failed);
                    return;
                }
                // Gate mode ends with a failed direct room (exit 1,
                // error on stderr); the windowed App stays resident.
                Outcome::Failed { failure, .. } if from_shell && self.options.gate => {
                    eprintln!("{failure}");
                    self.shell.exit(true);
                    return;
                }
                Outcome::Failed { failure, shown } => {
                    tracing::debug!(
                        event = "local-room-stopped",
                        reason = failure.reason().as_str(),
                        error = %failure.cause(),
                        "piik-client"
                    );
                    self.launcher.set_result(Err(Box::new(failure)));
                    if shown {
                        self.shell.navigate(self.launcher_url.clone());
                    }
                }
            }
        }
    }

    async fn run_room(&self, selection: Selection, direct: bool) -> Outcome {
        // Before readiness the window is still on the launcher, which shows
        // the failure from its own state.
        let failed = |reason, cause: String| Outcome::Failed {
            failure: Failure::new(reason, cause),
            shown: false,
        };
        if let Err(error) = piik_launcher::save_mode(&self.shared.config_path, selection.mode) {
            tracing::debug!(error = %error, "Could not save the launcher mode");
        }
        let password = selection.local_access_password.clone();
        let config = match self
            .shared
            .update(|config| config.local_access_password = password)
        {
            Ok(config) => config,
            Err(error) => {
                return failed(
                    Reason::Configuration,
                    format!("Piik App configuration could not be saved: {error}"),
                );
            }
        };
        if (self.options.debug || selection.debug)
            && let Err(error) = self.diagnostics.enable()
        {
            return failed(Reason::Diagnostics, format!("{error:#}"));
        }
        let lan_address = selection
            .lan_address
            .clone()
            .or_else(|| self.options.lan_address.clone())
            .unwrap_or_default();
        self.diagnostics.context(
            "configuration",
            serde_json::json!({
                "siteConfigured": !config.sites.is_empty(),
                "local": selection.mode == Mode::Local,
                "link": selection.mode == Mode::Link,
                "port": self.shared.port,
                "lanAddress": lan_address,
                "localAccessProtected": !config.local_access_password.is_empty(),
            }),
        );
        let options = RoomOptions {
            mode: selection.mode,
            port: self.shared.port,
            lan_address,
            password: config.local_access_password,
            tunnel_process: self.options.tunnel_process.clone(),
            control_port: self.shared.control.endpoint().port,
            assets: self.assets.clone(),
        };
        let room = match room::start(options, &self.stop).await {
            Ok(Some(room)) => room,
            Ok(None) => return Outcome::Stopped,
            Err(failure) => {
                return Outcome::Failed {
                    failure,
                    shown: false,
                };
            }
        };
        // Readiness lines follow the listener, before anything probes the port.
        if self.options.gate {
            gate_print(&room.ready_lines);
        }
        self.launcher.set_result(Ok(room.target.clone()));
        // A launcher selection navigates itself; a direct room needs the shell.
        if direct && let Ok(target) = Url::parse(&room.target) {
            self.shell.navigate(target);
        }
        let ended = tokio::select! {
            () = self.stop.cancelled() => None,
            ended = room.wait(&self.shared.control) => Some(ended),
        };
        if let Err(error) = room.end().await {
            tracing::warn!(error = %error, "Piik Local room did not stop cleanly");
        }
        match ended {
            None => Outcome::Stopped,
            Some(Ended::Failed(failure)) => Outcome::Failed {
                failure,
                shown: true,
            },
            Some(Ended::ControlStopped(error)) => {
                if let Some(error) = &error {
                    tracing::error!(error = %error, "Piik App native control stopped unexpectedly");
                }
                Outcome::Exit {
                    failed: error.is_some(),
                }
            }
        }
    }
}
