//! Sizes. Every one of them derives from the user's font size and density, so
//! there is no hardcoded row height anywhere in the app.

use gpui::{Pixels, px};

use crate::theme::Density;

/// Line height as a multiple of the font size.
pub const LEADING: f32 = 1.3;

/// Width below which the sidebar collapses, and below which the album grid drops
/// to its narrowest column count.
pub const NARROW: Pixels = px(760.);
pub const VERY_NARROW: Pixels = px(560.);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// A row in a dense table.
    pub row: Pixels,
    /// A row in the main track list, which carries a thumbnail.
    pub list_row: Pixels,
    /// A column header.
    pub header: Pixels,
    /// A sidebar entry.
    pub sidebar_row: Pixels,
    pub title_bar: Pixels,
    pub player_bar: Pixels,
    pub control: Pixels,
    pub control_small: Pixels,
    pub field: Pixels,
    pub pad: Pixels,
    pub inset: Pixels,
    pub gap: Pixels,
    /// The small artwork in a list row.
    pub thumb: Pixels,
    /// The artwork in the player bar.
    pub cover: Pixels,
    pub sidebar: Pixels,
    pub sidebar_min: Pixels,
    pub sidebar_max: Pixels,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(px(14.), Density::Normal)
    }
}

impl Metrics {
    pub fn new(base: Pixels, density: Density) -> Self {
        let text = base / px(1.);
        let scale = density.scale();
        // Every vertical measure is the larger of a floor and "this many lines
        // of the current text", so raising the font size never clips a label.
        let roomy =
            |floor: f32, lines: f32| px((floor * scale).max((text * lines).round()).round());

        Self {
            row: roomy(30., 2.0),
            list_row: roomy(48., 3.0),
            header: roomy(28., 1.9),
            sidebar_row: roomy(30., 2.0),
            title_bar: roomy(38., 2.4),
            player_bar: roomy(78., 5.0),
            control: roomy(30., 2.0),
            control_small: roomy(24., 1.6),
            field: roomy(32., 2.2),
            pad: px((8.0 * scale).round()),
            inset: px((16.0 * scale).round()),
            gap: px((6.0 * scale).round()),
            thumb: roomy(34., 2.2),
            cover: roomy(58., 3.8),
            sidebar: px(212.),
            sidebar_min: px(150.),
            sidebar_max: px(400.),
        }
    }
}

/// The type scale. Ratios rather than sizes, so the whole scale moves together
/// when the user changes the base font size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    Tiny,
    Small,
    Label,
    Body,
    Large,
    Title,
    Display,
}

impl Text {
    pub fn ratio(self) -> f32 {
        match self {
            Self::Tiny => 0.77,
            Self::Small => 0.85,
            Self::Label => 0.92,
            Self::Body => 1.0,
            Self::Large => 1.3,
            Self::Title => 1.65,
            Self::Display => 2.1,
        }
    }
}

/// `3:42`, or `1:02:03` for something over an hour. Used everywhere a duration
/// is shown, so the track list and the player bar never disagree.
pub fn clock(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".to_owned();
    }
    let total = seconds.round() as u64;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    match hours {
        0 => format!("{minutes}:{seconds:02}"),
        _ => format!("{hours}:{minutes:02}:{seconds:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_the_way_a_player_should() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(9.0), "0:09");
        assert_eq!(clock(222.0), "3:42");
        assert_eq!(clock(3600.0), "1:00:00");
        assert_eq!(clock(3723.4), "1:02:03");
    }

    #[test]
    fn clock_never_shows_nonsense_for_nonsense() {
        assert_eq!(clock(-5.0), "0:00");
        assert_eq!(clock(f64::NAN), "0:00");
        assert_eq!(clock(f64::INFINITY), "0:00");
    }

    #[test]
    fn a_bigger_font_never_shrinks_a_row() {
        let small = Metrics::new(px(11.), Density::Normal);
        let large = Metrics::new(px(20.), Density::Normal);
        assert!(large.list_row >= small.list_row);
        assert!(large.field >= small.field);
        assert!(large.title_bar >= small.title_bar);
    }

    #[test]
    fn density_scales_rows() {
        let compact = Metrics::new(px(14.), Density::Compact);
        let normal = Metrics::new(px(14.), Density::Normal);
        let comfortable = Metrics::new(px(14.), Density::Comfortable);
        assert!(compact.list_row < normal.list_row);
        assert!(normal.list_row < comfortable.list_row);
    }

    #[test]
    fn rows_stay_tall_enough_for_their_text() {
        for size in [11.0f32, 14.0, 20.0] {
            for density in Density::ALL {
                let metrics = Metrics::new(px(size), density);
                assert!(
                    metrics.row / px(1.) >= size * 1.5,
                    "a {size}px font does not fit in a {:?} row",
                    metrics.row
                );
            }
        }
    }
}
