//! The App launcher: the Browser launcher page and its local API on a
//! loopback port, its saved mode and startup-Site preferences, and the App
//! entry URLs that mark a page as running inside the App.
//!
//! The launcher never grants control access by itself: Site approval goes
//! through [`Options::change_site`], which the App wires to its configuration
//! and to `piik_loopback::Server::set_allowed_origins`.

#![forbid(unsafe_code)]

mod assets;
mod entry;
mod preference;
mod server;

pub use assets::{Assets, DirAssets, LAUNCHER_INDEX, MemoryAssets};
pub use entry::{entry_url, local_entry_url};
pub use preference::{load_mode, load_startup_site, save_mode, save_startup_site};
pub use server::{
    BoxError, ChangeSite, EnableDebug, Error, LanAddresses, Mode, Options, SaveStartupSite,
    Selection, Server,
};
