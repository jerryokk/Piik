//! The Browser UI embedded at build time from `web/dist`.

use std::borrow::Cow;

use crate::assets::MemoryAssets;

/// Every embedded file, dot-prefixed markers included.
static EMBEDDED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/webassets.rs"));

/// The embedded Vite build, or `None` when none was embedded (a clean
/// checkout holds only `.gitkeep`). The probe is `index.html`, never an entry
/// count.
pub fn embedded() -> Option<MemoryAssets> {
    if !EMBEDDED.iter().any(|(name, _)| *name == "index.html") {
        return None;
    }
    let mut assets = MemoryAssets::new();
    for (name, data) in EMBEDDED {
        assets.0.insert((*name).to_owned(), Cow::Borrowed(*data));
    }
    Some(assets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Assets as _;

    #[test]
    fn embedded_assets_track_index_html() {
        let index = EMBEDDED.iter().any(|(name, _)| *name == "index.html");
        match embedded() {
            Some(assets) => {
                assert!(index, "assets without index.html");
                assert!(assets.read("index.html").is_some());
            }
            None => assert!(!index, "index.html is embedded but no assets were returned"),
        }
    }

    #[test]
    fn gitkeep_is_embedded() {
        assert!(
            EMBEDDED.iter().any(|(name, _)| *name == ".gitkeep"),
            "dist/.gitkeep must stay tracked and embedded"
        );
    }
}
