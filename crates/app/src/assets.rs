//! Where GPUI gets its assets. Everything is compiled in, so there is no folder
//! to ship alongside the binary and no missing-file failure mode.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // The app mark is a raster and everything else is an SVG, so the
        // non-SVG assets answer first and the icon set answers the rest.
        if let Some(bytes) = icons::bytes(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        match icons::source(path) {
            Some(svg) => Ok(Some(Cow::Borrowed(svg.as_bytes()))),
            // `None` rather than an error: GPUI logs a missing asset and draws
            // nothing, which is a blank icon rather than a dead window.
            None => {
                log::warn!("assets: nothing registered at {path}");
                Ok(None)
            }
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(icons::names()
            .map(icons::path)
            .filter(|name| name.starts_with(path))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_through_the_asset_source() {
        let assets = Assets;
        for name in icons::names() {
            let loaded = assets.load(&icons::path(name)).unwrap();
            assert!(loaded.is_some(), "{name} did not load");
        }
    }

    #[test]
    fn the_app_mark_loads_and_is_a_png() {
        let loaded = Assets.load(icons::BRAND).unwrap().expect("the mark");
        // The title bar draws this, so a wrong or truncated blob is a blank
        // corner rather than a failure anything would report.
        assert_eq!(&loaded[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn a_missing_asset_is_none_rather_than_an_error() {
        assert!(Assets.load("icon/does-not-exist").unwrap().is_none());
    }

    #[test]
    fn listing_the_icon_prefix_finds_them_all() {
        let listed = Assets.list(icons::PREFIX).unwrap();
        assert_eq!(listed.len(), icons::names().count());
    }
}
