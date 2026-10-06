//! The icon set.
//!
//! Icons are our own 24x24 SVGs, written inline rather than shipped as a folder
//! of files: there is no build script, no asset tree to keep in step with the
//! code, and a missing icon is a visible `None` rather than a blank square.
//!
//! Call sites spell `icons::path("play")`. The app's asset source resolves that
//! through `source()`.

use gpui::SharedString;

/// The prefix GPUI sees. The app's `AssetSource` strips it and calls `source`.
pub const PREFIX: &str = "icon/";

/// The app mark, for `img(icons::BRAND)`.
///
/// Unlike everything else here this is a raster: the mark is artwork rather than
/// a glyph, so it stays the PNG it was drawn as. It is still compiled in, so it
/// cannot go missing at runtime any more than the SVGs can.
pub const BRAND: &str = "image/tinnitus.png";

const BRAND_PNG: &[u8] = include_bytes!("../assets/tinnitus.png");

/// The bytes behind a non-SVG asset path, or `None` if there is no such asset.
pub fn bytes(path: &str) -> Option<&'static [u8]> {
    match path {
        BRAND => Some(BRAND_PNG),
        _ => None,
    }
}

/// The asset path for an icon name, for `svg().path(...)`.
pub fn path(name: impl AsRef<str>) -> SharedString {
    SharedString::from(format!("{PREFIX}{}", name.as_ref()))
}

/// The SVG text for an icon, or `None` if there is no such icon.
pub fn source(name: &str) -> Option<&'static str> {
    let name = name.strip_prefix(PREFIX).unwrap_or(name);
    ICONS
        .iter()
        .find(|(icon, _)| *icon == name)
        .map(|(_, svg)| *svg)
}

pub fn names() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(name, _)| *name)
}

/// Wraps path data in a 24x24 stroked SVG. `currentColor` is what lets an icon
/// take the colour of the element it sits in.
macro_rules! stroked {
    ($body:expr) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" "#,
            r#"stroke="currentColor" stroke-width="1.6" stroke-linecap="round" "#,
            r#"stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

/// Filled shapes: the transport controls read better solid at small sizes.
macro_rules! filled {
    ($body:expr) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" "#,
            r#"fill="currentColor" stroke="none">"#,
            $body,
            "</svg>"
        )
    };
}

const ICONS: &[(&str, &str)] = &[
    // -- transport ---------------------------------------------------------
    ("play", filled!(r#"<path d="M8 5.2v13.6L19 12z"/>"#)),
    (
        "pause",
        filled!(r#"<path d="M7 5h3.4v14H7zM13.6 5H17v14h-3.4z"/>"#),
    ),
    (
        "stop",
        filled!(r#"<rect x="6" y="6" width="12" height="12" rx="1.5"/>"#),
    ),
    (
        "next",
        filled!(
            r#"<path d="M6 5.2v13.6L15.5 12z"/><rect x="16.6" y="5" width="2.4" height="14" rx="1"/>"#
        ),
    ),
    (
        "previous",
        filled!(
            r#"<path d="M18 5.2v13.6L8.5 12z"/><rect x="5" y="5" width="2.4" height="14" rx="1"/>"#
        ),
    ),
    (
        "shuffle",
        stroked!(
            r#"<path d="M3 7h3.5l3 4.2M21 7h-4l-8 10H3"/><path d="M15.5 4.5 21 7l-5.5 2.5"/><path d="M15.5 14.5 21 17l-5.5 2.5"/><path d="M21 17h-4"/>"#
        ),
    ),
    (
        "repeat",
        stroked!(
            r#"<path d="M4 12V9a3 3 0 0 1 3-3h11"/><path d="m15.5 3 3 3-3 3"/><path d="M20 12v3a3 3 0 0 1-3 3H6"/><path d="m8.5 21-3-3 3-3"/>"#
        ),
    ),
    (
        "repeat-one",
        stroked!(
            r#"<path d="M4 12V9a3 3 0 0 1 3-3h11"/><path d="m15.5 3 3 3-3 3"/><path d="M20 12v3a3 3 0 0 1-3 3H6"/><path d="m8.5 21-3-3 3-3"/><path d="M12 14.2v-2.4l-1.2.8"/>"#
        ),
    ),
    // -- volume ------------------------------------------------------------
    (
        "volume",
        stroked!(
            r#"<path d="M4 9.5h3.2L12 5.5v13L7.2 14.5H4z"/><path d="M15.5 9.2a4 4 0 0 1 0 5.6"/><path d="M18.2 6.6a7.6 7.6 0 0 1 0 10.8"/>"#
        ),
    ),
    (
        "volume-low",
        stroked!(
            r#"<path d="M4 9.5h3.2L12 5.5v13L7.2 14.5H4z"/><path d="M15.5 9.2a4 4 0 0 1 0 5.6"/>"#
        ),
    ),
    (
        "volume-mute",
        stroked!(
            r#"<path d="M4 9.5h3.2L12 5.5v13L7.2 14.5H4z"/><path d="m16 9.5 5 5M21 9.5l-5 5"/>"#
        ),
    ),
    // -- library -----------------------------------------------------------
    (
        "music",
        stroked!(
            r#"<path d="M9 18V6.5l10-2v11"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="16.5" cy="15.5" r="2.5"/>"#
        ),
    ),
    (
        "album",
        stroked!(r#"<circle cx="12" cy="12" r="8.5"/><circle cx="12" cy="12" r="2.2"/>"#),
    ),
    (
        "artist",
        stroked!(r#"<circle cx="12" cy="8" r="3.6"/><path d="M4.8 20a7.2 7.2 0 0 1 14.4 0"/>"#),
    ),
    ("genre", stroked!(r#"<path d="M4 6h16M4 12h16M4 18h9"/>"#)),
    (
        "calendar",
        stroked!(
            r#"<rect x="3.5" y="5" width="17" height="15.5" rx="2.5"/><path d="M3.5 10h17M8 3.5v3M16 3.5v3"/>"#
        ),
    ),
    (
        "folder",
        stroked!(
            r#"<path d="M3.5 7.5a2 2 0 0 1 2-2h3.3l2 2.6h7.7a2 2 0 0 1 2 2v8.4a2 2 0 0 1-2 2H5.5a2 2 0 0 1-2-2z"/>"#
        ),
    ),
    (
        "playlist",
        stroked!(
            r#"<path d="M4 7h11M4 12h11M4 17h6"/><path d="M18 17V9.5l3-.8"/><circle cx="16.6" cy="17.2" r="1.6"/>"#
        ),
    ),
    (
        "queue",
        stroked!(r#"<path d="M4 7h16M4 12h16M4 17h9"/><path d="M17.5 14.5v5M15 17h5"/>"#),
    ),
    (
        "clock",
        stroked!(r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.2V12l3.2 2"/>"#),
    ),
    (
        "trending",
        stroked!(r#"<path d="M3.5 16.5 9 11l3.5 3.5L20.5 6.5"/><path d="M15.5 6.5h5v5"/>"#),
    ),
    (
        "heart",
        stroked!(
            r#"<path d="M12 20s-7.5-4.6-7.5-9.6A4.4 4.4 0 0 1 12 7.6a4.4 4.4 0 0 1 7.5 2.8C19.5 15.4 12 20 12 20z"/>"#
        ),
    ),
    (
        "heart-filled",
        filled!(
            r#"<path d="M12 20.4s-8-4.9-8-10.2A4.8 4.8 0 0 1 12 7.1a4.8 4.8 0 0 1 8 3.1c0 5.3-8 10.2-8 10.2z"/>"#
        ),
    ),
    (
        "star",
        stroked!(
            r#"<path d="m12 4.2 2.4 4.9 5.4.8-3.9 3.8.9 5.3-4.8-2.5-4.8 2.5.9-5.3-3.9-3.8 5.4-.8z"/>"#
        ),
    ),
    (
        "star-filled",
        filled!(
            r#"<path d="m12 3.8 2.5 5.1 5.6.8-4 3.9 1 5.6-5.1-2.7-5.1 2.7 1-5.6-4-3.9 5.6-.8z"/>"#
        ),
    ),
    // -- chrome ------------------------------------------------------------
    (
        "search",
        stroked!(r#"<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>"#),
    ),
    (
        // A cog: a toothed ring around a hub, not a circle with detached rays.
        // The teeth carry their own stroke width so they read as teeth rather
        // than as more of the ring.
        "settings",
        stroked!(
            r#"<circle cx="12" cy="12" r="7"/><circle cx="12" cy="12" r="2.6"/><path stroke-width="3" d="M12 3.7V2.3M12 21.7v-1.4M20.3 12h1.4M2.3 12h1.4M17.94 6.06l1-1M5.06 18.94l1-1M17.94 17.94l1 1M5.06 5.06l1 1"/>"#
        ),
    ),
    (
        "home",
        stroked!(
            r#"<path d="M3.8 10.5 12 4l8.2 6.5V19a1.8 1.8 0 0 1-1.8 1.8H5.6A1.8 1.8 0 0 1 3.8 19z"/><path d="M9.6 20.8v-6.6h4.8v6.6"/>"#
        ),
    ),
    (
        "equalizer",
        stroked!(
            r#"<path d="M6 20V14M6 10V4M12 20v-9M12 7V4M18 20v-4M18 12V4"/><path d="M3.5 14h5M9.5 11h5M15.5 16h5"/>"#
        ),
    ),
    (
        "grid",
        stroked!(
            r#"<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>"#
        ),
    ),
    (
        "list",
        stroked!(r#"<path d="M8 6h12M8 12h12M8 18h12"/><path d="M4 6h.01M4 12h.01M4 18h.01"/>"#),
    ),
    ("plus", stroked!(r#"<path d="M12 5v14M5 12h14"/>"#)),
    ("minus", stroked!(r#"<path d="M5 12h14"/>"#)),
    ("close", stroked!(r#"<path d="m6 6 12 12M18 6 6 18"/>"#)),
    (
        "maximize",
        stroked!(r#"<rect x="5" y="5" width="14" height="14" rx="2"/>"#),
    ),
    (
        "restore",
        stroked!(
            r#"<rect x="4.5" y="7.5" width="12" height="12" rx="2"/><path d="M8 7.5V6a2 2 0 0 1 2-2h7.5a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2H16"/>"#
        ),
    ),
    (
        "more",
        filled!(
            r#"<circle cx="12" cy="5.6" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="12" cy="18.4" r="1.7"/>"#
        ),
    ),
    (
        "chevron-right",
        stroked!(r#"<path d="m9.5 5.5 6.5 6.5-6.5 6.5"/>"#),
    ),
    (
        "chevron-down",
        stroked!(r#"<path d="m5.5 9.5 6.5 6.5 6.5-6.5"/>"#),
    ),
    (
        "chevron-up",
        stroked!(r#"<path d="m5.5 14.5 6.5-6.5 6.5 6.5"/>"#),
    ),
    (
        "chevron-left",
        stroked!(r#"<path d="m14.5 5.5-6.5 6.5 6.5 6.5"/>"#),
    ),
    (
        "edit",
        stroked!(
            r#"<path d="M4 20h4l10.5-10.5a2.4 2.4 0 0 0-3.4-3.4L4.5 16.6z"/><path d="m14.5 7 2.5 2.5"/>"#
        ),
    ),
    (
        // Two sheets, the back one peeking out behind the front: the shape
        // every desktop uses for "make another one of these".
        "copy",
        stroked!(
            r#"<rect x="9" y="9" width="11.5" height="11.5" rx="2"/><path d="M5.5 15h-.5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2H13a2 2 0 0 1 2 2v.5"/>"#
        ),
    ),
    (
        "trash",
        stroked!(
            r#"<path d="M4.5 6.5h15M9.5 6.5V4.8a1.3 1.3 0 0 1 1.3-1.3h2.4a1.3 1.3 0 0 1 1.3 1.3v1.7"/><path d="M6.5 6.5 7.4 19a1.7 1.7 0 0 0 1.7 1.6h5.8a1.7 1.7 0 0 0 1.7-1.6l.9-12.5"/>"#
        ),
    ),
    (
        "external",
        stroked!(
            r#"<path d="M13.5 4.5H19.5V10.5"/><path d="M19.5 4.5 11 13"/><path d="M18 14.5v4a2 2 0 0 1-2 2H5.5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>"#
        ),
    ),
    (
        // A circled "i": what an About entry wears everywhere.
        "info",
        stroked!(r#"<circle cx="12" cy="12" r="9"/><path d="M12 11v5.5"/><path d="M12 7.6v.6"/>"#),
    ),
    (
        // A tick, for a menu row that names something already done.
        "check",
        stroked!(r#"<path d="M4.5 12.5 9.5 17.5 19.5 6.5"/>"#),
    ),
    (
        "wave",
        stroked!(r#"<path d="M3 12h2.5l2-6 3 13 3-9 2 4H21"/>"#),
    ),
    (
        "refresh",
        stroked!(r#"<path d="M20 11a8 8 0 1 0-.8 4.4"/><path d="M20 5.5V11h-5.5"/>"#),
    ),
    (
        "warning",
        stroked!(r#"<path d="M12 4.5 21 19.5H3z"/><path d="M12 10v4M12 16.6h.01"/>"#),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_resolves_by_name_and_by_asset_path() {
        for name in names() {
            assert!(source(name).is_some(), "{name} has no source");
            let asset = path(name);
            assert!(source(&asset).is_some(), "{asset} did not resolve");
        }
    }

    #[test]
    fn an_unknown_icon_is_none_rather_than_a_panic() {
        assert!(source("no-such-icon").is_none());
        assert!(source("icon/no-such-icon").is_none());
    }

    #[test]
    fn icon_names_are_unique() {
        let mut seen: Vec<&str> = names().collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "two icons share a name");
    }

    #[test]
    fn every_icon_is_well_formed_and_takes_its_colour_from_the_element() {
        for name in names() {
            let svg = source(name).unwrap();
            assert!(svg.starts_with("<svg"), "{name} is not an svg");
            assert!(svg.ends_with("</svg>"), "{name} is not closed");
            assert!(
                svg.contains("viewBox=\"0 0 24 24\""),
                "{name} is not on the 24x24 grid"
            );
            assert!(
                svg.contains("currentColor"),
                "{name} hardcodes a colour instead of inheriting one"
            );
        }
    }
}
