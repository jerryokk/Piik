//! Pure startup and approval rules: the opening view, `--site` approval and
//! the two origin lists that follow Site approval.

use piik_app_config::{AppConfig, normalize_site, normalize_sites};
use piik_launcher::Mode;
use url::Url;

use crate::options::Options;

/// What the main window opens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Entry {
    /// The launcher page waits for a selection.
    Launcher,
    /// An approved Site; the launcher stays available for rooms.
    Site(String),
    /// `--local` / `--link`: the room starts at once.
    Room(Mode),
}

/// `--local`/`--link` start a room; `--site` (already approved into
/// `config`) or the starred startup Site opens that Site; gate mode opens
/// the first approved Site or a Local room; otherwise the launcher.
pub(super) fn entry(options: &Options, config: &AppConfig, startup_site: &str) -> Entry {
    if options.local {
        Entry::Room(Mode::Local)
    } else if options.link {
        Entry::Room(Mode::Link)
    } else if (options.site.is_some() || options.gate)
        && let Some(site) = config.sites.first()
    {
        Entry::Site(site.clone())
    } else if options.gate {
        // Gate mode is explicit: without a Site it starts a Local room.
        Entry::Room(Mode::Local)
    } else if !startup_site.is_empty() {
        Entry::Site(startup_site.to_owned())
    } else {
        Entry::Launcher
    }
}

/// `--site` approves the origin ahead of earlier grants.
pub(super) fn approve_site(config: &mut AppConfig, site: &str) -> Result<(), String> {
    let invalid = || "Piik Site must be an HTTP or HTTPS origin".to_owned();
    let site = normalize_site(site)
        .ok()
        .filter(|site| !site.is_empty())
        .ok_or_else(invalid)?;
    let mut sites = vec![site];
    sites.extend(config.sites.iter().cloned());
    config.sites = normalize_sites(&sites).map_err(|error| error.to_string())?;
    Ok(())
}

/// Applies one launcher approval change to the Site list.
pub(super) fn change_sites(config: &mut AppConfig, site: &str, allowed: bool) {
    if allowed {
        if !config.sites.iter().any(|value| value == site) {
            config.sites.push(site.to_owned());
        }
    } else {
        config.sites.retain(|value| value != site);
    }
}

/// Loopback control origins: the Local room on both loopback names, every
/// approved Site and the launcher.
pub(super) fn control_origins(sites: &[String], port: u16, launcher: Option<&Url>) -> Vec<String> {
    let mut origins = vec![
        format!("http://localhost:{port}"),
        format!("http://127.0.0.1:{port}"),
    ];
    origins.extend(sites.iter().cloned());
    origins.extend(launcher.map(|url| url.origin().ascii_serialization()));
    origins
}

/// Origins the main window loads in place: the launcher, the Local room and
/// every approved Site. Everything else opens in the system browser.
pub(super) fn navigation_urls(launcher: Option<&Url>, port: u16, sites: &[String]) -> Vec<Url> {
    let room = [
        format!("http://127.0.0.1:{port}/"),
        format!("http://localhost:{port}/"),
    ];
    launcher
        .cloned()
        .into_iter()
        .chain(
            room.iter()
                .chain(sites)
                .filter_map(|value| Url::parse(value).ok()),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::shell::AllowedOrigins;

    fn options(args: &[&str]) -> Options {
        Options::try_parse_from(std::iter::once("piik-app").chain(args.iter().copied())).unwrap()
    }

    fn config(sites: &[&str]) -> AppConfig {
        AppConfig {
            version: piik_app_config::CURRENT_VERSION,
            sites: sites.iter().map(|site| (*site).to_owned()).collect(),
            ..AppConfig::default()
        }
    }

    #[test]
    fn selects_the_opening_view() {
        let none = config(&[]);
        let starred = config(&["https://a.example", "https://b.example"]);
        assert_eq!(entry(&options(&[]), &none, ""), Entry::Launcher);
        assert_eq!(entry(&options(&[]), &starred, ""), Entry::Launcher);
        assert_eq!(
            entry(&options(&[]), &starred, "https://b.example"),
            Entry::Site("https://b.example".into())
        );
        // An explicit room wins over a starred Site.
        assert_eq!(
            entry(&options(&["--local"]), &starred, "https://b.example"),
            Entry::Room(Mode::Local)
        );
        assert_eq!(
            entry(&options(&["--link"]), &none, ""),
            Entry::Room(Mode::Link)
        );
        // `--site` opens the Site it approved, not the starred one.
        let mut approved = starred.clone();
        approve_site(&mut approved, "c.example").unwrap();
        assert_eq!(
            entry(
                &options(&["--site", "c.example"]),
                &approved,
                "https://b.example"
            ),
            Entry::Site("https://c.example".into())
        );
        // Gate mode opens the starred Site, else a Local room, never the launcher.
        let gate = Options {
            gate: true,
            ..options(&[])
        };
        assert_eq!(
            entry(&gate, &starred, "https://b.example"),
            Entry::Site("https://a.example".into())
        );
        assert_eq!(entry(&gate, &none, ""), Entry::Room(Mode::Local));
    }

    #[test]
    fn site_flag_approves_first_and_deduplicates() {
        let mut approved = config(&["https://a.example", "https://b.example"]);
        approve_site(&mut approved, "https://b.example/").unwrap();
        assert_eq!(approved.sites, ["https://b.example", "https://a.example"]);
        for invalid in ["", "ftp://a.example", "https://a.example/path"] {
            assert!(approve_site(&mut approved, invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn approval_updates_both_origin_lists() {
        let launcher = Url::parse("http://127.0.0.1:50100/client").unwrap();
        let mut current = config(&["https://a.example"]);
        change_sites(&mut current, "https://b.example", true);
        change_sites(&mut current, "https://b.example", true);
        change_sites(&mut current, "https://a.example", false);
        let sites = current.sites;
        assert_eq!(sites, ["https://b.example"]);

        assert_eq!(
            control_origins(&sites, 8787, Some(&launcher)),
            [
                "http://localhost:8787",
                "http://127.0.0.1:8787",
                "https://b.example",
                "http://127.0.0.1:50100",
            ]
        );
        assert_eq!(control_origins(&[], 9000, None).len(), 2);

        let window = AllowedOrigins::default();
        window.set(&navigation_urls(Some(&launcher), 8787, &sites));
        for allowed in [
            "http://127.0.0.1:50100/client",
            "http://127.0.0.1:8787/#client-access=x",
            "http://localhost:8787/join",
            "https://b.example/?piik-client-port=39721#piik-client=1",
        ] {
            assert!(window.allows(&Url::parse(allowed).unwrap()), "{allowed}");
        }
        for refused in [
            "https://a.example/",
            "https://192.0.2.10:8787/",
            "http://127.0.0.1:39721/health",
            "https://github.com/",
        ] {
            assert!(!window.allows(&Url::parse(refused).unwrap()), "{refused}");
        }
    }
}
