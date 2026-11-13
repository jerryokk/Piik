//! Configuration tests by setting family. Shared fixtures and loaders live
//! here.

#[path = "config/ice.rs"]
mod ice;
#[path = "config/load.rs"]
mod load;
#[path = "config/local.rs"]
mod local;
#[path = "config/origins.rs"]
mod origins;
#[path = "config/rejects.rs"]
mod rejects;
#[path = "config/stun.rs"]
mod stun;

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use piik_config::{
    Config, DirectTls, Environment, LocalOptions, SfuConfig, TlsSource, ice_config, load, local,
    nat_prediction_stun_urls, origin, origin_from_host, parse_stun_urls,
};
use piik_protocol::{MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS, MAX_VIEWERS_PER_ROOM_LIMIT, encode};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

const PRODUCTION: [(&str, &str); 4] = [
    ("PIIK_ENV", "production"),
    ("PUBLIC_BASE_URL", "https://share.test"),
    ("SITE_ACCESS_PASSWORD", "host-password-12"),
    ("STUN_URLS", "stun:stun.test:3478"),
];

/// The production base with `overrides` applied, later keys winning.
fn production(overrides: &[(&str, &str)]) -> HashMap<String, String> {
    let mut merged = env(&PRODUCTION);
    merged.extend(env(overrides));
    merged
}

fn stun_list(count: usize, prefix: &str) -> String {
    (0..count)
        .map(|index| format!("stun:{prefix}-{index}.test:3478"))
        .collect::<Vec<_>>()
        .join(",")
}

fn must_load(environment: &HashMap<String, String>) -> Config {
    load(environment).unwrap_or_else(|error| panic!("load: unexpected error {error}"))
}

fn set(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn addresses(values: &[&str]) -> Vec<SocketAddr> {
    values.iter().map(|value| value.parse().unwrap()).collect()
}
