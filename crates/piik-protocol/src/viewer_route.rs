//! Viewer route selection: constrains only this Viewer's incoming media hop.

use crate::codec::wire_enum;

wire_enum! {
    /// `viewerRoutePreferenceSchema`.
    pub enum ViewerRoutePreference {
        Auto = "auto",
        Peer = "p2p",
        Sfu = "sfu",
    }
}
