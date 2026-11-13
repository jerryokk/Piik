//! The App command line.

use std::path::PathBuf;

use clap::Parser;

/// Default Local Piik server port.
pub const DEFAULT_LOCAL_PORT: u16 = 8787;

#[derive(Debug, Clone, Parser)]
#[command(name = "piik-app", about = "Piik App", version = crate::build_info::RELEASE)]
pub struct Options {
    /// Save and open a Piik Site origin.
    #[arg(long, value_name = "ORIGIN")]
    pub site: Option<String>,
    /// Use the self-contained Local room authority.
    #[arg(long, conflicts_with = "site")]
    pub local: bool,
    /// Create one public Viewer invitation link.
    #[arg(long, conflicts_with_all = ["site", "local"])]
    pub link: bool,
    /// Save opt-in App diagnostics to rotated files (also `PIIK_DEBUG=client`).
    #[arg(long)]
    pub debug: bool,
    /// Diagnostic directory (overrides `PIIK_LOG_DIR`).
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,
    /// Path to the platform native capture process.
    #[arg(long, value_name = "PATH")]
    pub capture_process: Option<PathBuf>,
    /// Path to the packaged public tunnel process.
    #[arg(long, value_name = "PATH")]
    pub tunnel_process: Option<PathBuf>,
    /// Path to the App configuration file.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// LAN IPv4 address used in Local invitations.
    #[arg(long, value_name = "IPV4")]
    pub lan_address: Option<String>,
    /// Local Piik server port.
    #[arg(long, default_value_t = DEFAULT_LOCAL_PORT)]
    pub port: u16,
    /// Gate mode (`PIIK_CLIENT_GATE_NO_BROWSER=true`): no window; the
    /// endpoint and readiness lines go to stdout and stdin stops the App.
    #[arg(skip)]
    pub gate: bool,
    /// Gate mode only (`PIIK_CLIENT_GATE_CONTROL_PORT`): pins native control
    /// to one port for tests on hosts that reserve 39721..=39730. Zero keeps
    /// the published range.
    #[arg(skip)]
    pub gate_control_port: u16,
}

impl Options {
    /// Parses `args` and folds `PIIK_DEBUG` and the gate variables in.
    pub fn parse_from_env<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let mut options = Self::try_parse_from(args)?;
        let env = |name| std::env::var(name).unwrap_or_default();
        options.debug |= debug_enabled(&env("PIIK_DEBUG"));
        options.gate = env("PIIK_CLIENT_GATE_NO_BROWSER") == "true";
        if options.gate {
            options.gate_control_port = gate_control_port(&env("PIIK_CLIENT_GATE_CONTROL_PORT"))?;
        }
        Ok(options)
    }
}

fn gate_control_port(value: &str) -> Result<u16, clap::Error> {
    if value.is_empty() {
        return Ok(0);
    }
    value.parse().map_err(|_| {
        clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "PIIK_CLIENT_GATE_CONTROL_PORT must be a TCP port\n",
        )
    })
}

fn debug_enabled(value: &str) -> bool {
    value
        .split(',')
        .any(|component| component.trim() == "client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_flags() {
        let options = Options::try_parse_from([
            "piik-app",
            "--link",
            "--debug",
            "--log-dir",
            "logs",
            "--capture-process",
            "runtime/native/piik-capture.exe",
            "--tunnel-process",
            "runtime/tunnel/cloudflared.exe",
            "--config",
            "client.json",
            "--lan-address",
            "192.0.2.10",
            "--port",
            "9000",
        ])
        .unwrap();
        assert!(options.link && options.debug && !options.local);
        assert_eq!(options.port, 9000);
        assert_eq!(options.lan_address.as_deref(), Some("192.0.2.10"));
        assert_eq!(
            Options::try_parse_from(["piik-app"]).unwrap().port,
            DEFAULT_LOCAL_PORT
        );
    }

    #[test]
    fn rejects_conflicting_modes() {
        assert!(
            Options::try_parse_from(["piik-app", "--site", "https://a.example", "--local"])
                .is_err()
        );
        assert!(Options::try_parse_from(["piik-app", "--local", "--link"]).is_err());
    }

    #[test]
    fn debug_component_list() {
        assert!(debug_enabled("server, client"));
        assert!(!debug_enabled("clients"));
        assert!(!debug_enabled(""));
    }

    #[test]
    fn gate_control_port_override() {
        assert_eq!(gate_control_port("").unwrap(), 0);
        assert_eq!(gate_control_port("40123").unwrap(), 40123);
        assert!(gate_control_port("loopback").is_err());
        assert!(gate_control_port("70000").is_err());
    }
}
