//! Capabilities, site access and the room HTTP API.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use piik_config::Environment;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use url::Url;

use super::{
    ALLOWED_ORIGIN, Harness, Req, RoomRequest, TEST_ACCESS_PASSWORD, call, cookie_pair, free_port,
    new_server, no_media, options, sfu_config, start, test_config,
};
use crate::server::build_store;

#[path = "app/app_capabilities.rs"]
mod app_capabilities;
#[path = "app/app_composition.rs"]
mod app_composition;
#[path = "app/app_rooms.rs"]
mod app_rooms;
#[path = "app/app_site_access.rs"]
mod app_site_access;

/// A settable server clock shared with the tests.
fn clock(now: &Arc<AtomicI64>) -> crate::Clock {
    let now = now.clone();
    Arc::new(move || now.load(Ordering::SeqCst))
}
