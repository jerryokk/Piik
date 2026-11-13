//! One Local or public-link room: sockets, the optional cloudflared tunnel,
//! the embedded Piik server and their ordered stop.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use piik_failure::{Failure, Reason};
use piik_launcher::Mode;
use piik_loopback::CancellationToken;
use piik_public_tunnel::{Process, Startup};
use piik_service::{Assets, Listener, Options as ServerOptions, Server};

/// Bounds ending every room, signaling and the drain.
pub(super) const END_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct RoomOptions {
    pub mode: Mode,
    pub port: u16,
    pub lan_address: String,
    pub password: String,
    pub tunnel_process: Option<PathBuf>,
    pub control_port: u16,
    pub assets: Arc<dyn Assets>,
}

/// A ready room. [`Room::end`] stops it; dropping it only kills the tunnel.
pub(super) struct Room {
    server: Server,
    tunnel: Option<Process>,
    /// The Host page entry on the loopback origin.
    pub target: String,
    /// Gate-mode stdout readiness lines: access, then the invitation origin.
    pub ready_lines: Vec<String>,
}

/// Why a ready room stopped without its owner asking.
pub(super) enum Ended {
    Failed(Failure),
    /// The loopback control server stopped; the App cannot continue.
    ControlStopped(Option<String>),
}

fn failure(reason: Reason, cause: impl std::fmt::Display) -> Failure {
    Failure::new(reason, cause.to_string())
}

/// Starts a room. `Ok(None)` is a clean `stop` during tunnel startup.
pub(super) async fn start(
    options: RoomOptions,
    stop: &CancellationToken,
) -> Result<Option<Room>, Failure> {
    let link = options.mode == Mode::Link;
    let port = options.port;
    tracing::debug!(event = "mode", mode = options.mode.as_str(), "piik-client");
    let (stun_urls, nat_prediction_stun_urls) = if link {
        let value = std::env::var("PIIK_STUN_URLS").unwrap_or_default();
        piik_app_config::public_stun_urls(&value)
            .map_err(|error| failure(Reason::Configuration, error))?
    } else {
        (Vec::new(), Vec::new())
    };
    // Local keeps the Host on loopback; a public link serves the tunnel too.
    let bind = if link {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    let plain = TcpListener::bind(SocketAddr::from((bind, port))).map_err(|error| {
        failure(
            Reason::LocalPort,
            format!("local server port is unavailable: {error}"),
        )
    })?;
    let mut listeners = vec![Listener::plain(plain)];

    // A public invitation needs only the tunnel origin: LAN selection never
    // blocks it, and resolved addresses still become allowed local origins.
    let lan = piik_lan::addresses().and_then(|addresses| {
        let selected = piik_lan::select(&addresses, &options.lan_address)?;
        Ok((addresses, selected))
    });
    let (addresses, selected) = match lan {
        Ok(lan) => lan,
        Err(error) if !link => return Err(failure(Reason::LocalNetwork, error)),
        Err(_) => (piik_lan::addresses().unwrap_or_default(), String::new()),
    };
    let mut certificate = String::new();
    if !link {
        let identity = piik_tls::self_signed(&selected).map_err(|error| {
            failure(
                Reason::LocalServer,
                format!("local HTTPS certificate could not be created: {error}"),
            )
        })?;
        let address: Ipv4Addr = selected
            .parse()
            .map_err(|_| failure(Reason::LocalNetwork, "selected LAN address is invalid"))?;
        let secure = TcpListener::bind(SocketAddr::from((address, port))).map_err(|error| {
            failure(
                Reason::LocalPort,
                format!("local HTTPS port is unavailable: {error}"),
            )
        })?;
        tracing::debug!(
            event = "local-certificate",
            sha256 = identity.fingerprint(),
            "piik-client"
        );
        identity.fingerprint().clone_into(&mut certificate);
        listeners.push(Listener::tls(secure, identity));
    }

    let tunnel = if link {
        let executable = options
            .tunnel_process
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(piik_public_tunnel::packaged_executable)
            .unwrap_or_default();
        match Startup::new(executable, format!("http://127.0.0.1:{port}"))
            .start(stop)
            .await
        {
            Ok(process) => Some(process),
            // Only the owner's own clean cancellation is a stopped outcome.
            Err(error) if error.is_canceled() && stop.is_cancelled() => return Ok(None),
            Err(error) => return Err(tunnel_failure(&error)),
        }
    } else {
        None
    };
    let public_origin = tunnel
        .as_ref()
        .map(|tunnel| tunnel.origin().to_owned())
        .unwrap_or_default();
    let mut ready_lines = vec![if options.password.is_empty() {
        "Local access: open".to_owned()
    } else {
        format!("Local access password: {}", options.password)
    }];
    if !public_origin.is_empty() {
        ready_lines.push(format!("Public invitation origin: {public_origin}"));
    }
    if tunnel.is_some() {
        tracing::debug!(event = "public-link-ready", "piik-client");
    }

    let config = piik_config::local(piik_config::LocalOptions {
        port,
        public_address: selected,
        public_origin,
        allowed_addresses: addresses,
        site_access_password: options.password.clone(),
        stun_urls,
        nat_prediction_stun_urls,
    })
    .map_err(|error| failure(Reason::Configuration, error))?;
    if !link {
        let origin = piik_config::origin(&config.public_base_url);
        ready_lines.push(format!("LAN invitation origin: {origin}"));
        ready_lines.push(format!("LAN certificate SHA-256: {certificate}"));
    }
    // SFU: the App never enables the embedded SFU for Local or Link rooms.
    let server = Server::new(ServerOptions {
        config,
        listeners,
        assets: Some(options.assets),
        sfu: None,
        ..ServerOptions::default()
    })
    .map_err(|error| failure(Reason::LocalServer, error))?;
    let room = Room {
        server,
        tunnel,
        target: piik_launcher::local_entry_url(
            &format!("http://127.0.0.1:{port}/"),
            &options.password,
            options.control_port,
        ),
        ready_lines,
    };
    if let Err(error) = room.server.listen().await {
        let reason = match error {
            piik_service::Error::Bind(_) => Reason::LocalPort,
            _ => Reason::LocalServer,
        };
        let _ = room.end().await;
        return Err(failure(reason, error));
    }
    tracing::debug!(
        event = "local-server-ready",
        port,
        public_link = link,
        "piik-client"
    );
    Ok(Some(room))
}

/// The tunnel error already leads with its reason line; keep it once.
fn tunnel_failure(error: &piik_public_tunnel::Error) -> Failure {
    let reason = error.reason().unwrap_or(Reason::TunnelStart);
    let text = error.to_string();
    let cause = text
        .strip_prefix(reason.as_str())
        .map_or(text.as_str(), |rest| rest.trim_start_matches('\n'));
    failure(reason, cause)
}

impl Room {
    /// Resolves when the room stops on its own.
    pub async fn wait(&self, control: &piik_loopback::Server) -> Ended {
        let tunnel = async {
            match &self.tunnel {
                Some(tunnel) => tunnel.done().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            error = self.server.failed() => Ended::Failed(failure(Reason::LocalServer, error)),
            result = control.done() => Ended::ControlStopped(result.err().map(|error| error.to_string())),
            () = tunnel => Ended::Failed(failure(Reason::TunnelStopped, "public invitation link stopped")),
        }
    }

    /// Ends every room and drains the server, then retires the tunnel.
    pub async fn end(self) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Err(error) = self.server.end(END_TIMEOUT).await {
            errors.push(error.to_string());
        }
        if let Some(tunnel) = &self.tunnel
            && let Err(error) = tunnel.close().await
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_carry_launcher_reasons() {
        let error = failure(Reason::LocalPort, "local server port is unavailable: busy");
        assert_eq!(piik_failure::of(&error), Some(Reason::LocalPort));
        assert_eq!(
            error.to_string(),
            "local-port\nlocal server port is unavailable: busy"
        );
        // A launcher result keeps the reason through its boxed error.
        let boxed: piik_launcher::BoxError = Box::new(error);
        assert_eq!(piik_failure::of(boxed.as_ref()), Some(Reason::LocalPort));
    }

    #[tokio::test]
    async fn tunnel_failures_keep_their_reason_once() {
        let stop = CancellationToken::new();
        let error = Startup::new("Z:\\missing\\cloudflared.exe", "http://127.0.0.1:1")
            .start(&stop)
            .await
            .unwrap_err();
        let mapped = tunnel_failure(&error);
        assert_eq!(mapped.reason(), Reason::TunnelUnavailable);
        assert!(!mapped.cause().to_string().starts_with("tunnel-unavailable"));
    }

    #[tokio::test]
    async fn busy_port_is_a_local_port_failure() {
        let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        let options = RoomOptions {
            mode: Mode::Local,
            port,
            lan_address: String::new(),
            password: String::new(),
            tunnel_process: None,
            control_port: 0,
            assets: Arc::new(piik_service::MemoryAssets::new()),
        };
        let error = start(options, &CancellationToken::new())
            .await
            .err()
            .expect("the port is taken");
        assert_eq!(error.reason(), Reason::LocalPort);
    }
}
