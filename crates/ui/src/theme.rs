//! Theme tokens and the performance budget.
//!
//! Nothing in the app writes a colour, a radius or a size literal: it all comes
//! from here, so a theme change is one place and a compatibility mode can turn
//! effects off globally.

use gpui::{App, Global, Hsla, Pixels, Rgba, px, rgb};
use serde::{Deserialize, Serialize};

use crate::metrics::{Metrics, Text};

pub const MIN_FONT: f32 = 11.0;
pub const MAX_FONT: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Look {
    #[default]
    Dark,
    Light,
    /// Follow the operating system.
    System,
}

/// Corner rounding, one of the few things the user gets to dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rounding {
    Square,
    #[default]
    Subtle,
    Rounded,
    Round,
}

impl Rounding {
    pub const ALL: [Self; 4] = [Self::Square, Self::Subtle, Self::Rounded, Self::Round];

    pub fn label(self) -> &'static str {
        match self {
            Self::Square => "Square",
            Self::Subtle => "Subtle",
            Self::Rounded => "Rounded",
            Self::Round => "Round",
        }
    }

    pub fn radius(self) -> Pixels {
        match self {
            Self::Square => px(0.),
            Self::Subtle => px(6.),
            Self::Rounded => px(10.),
            Self::Round => px(16.),
        }
    }
}

/// How tightly rows are packed. Independent of font size: some people want
/// large text *and* dense lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

impl Density {
    pub const ALL: [Self; 3] = [Self::Compact, Self::Normal, Self::Comfortable];

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Normal => "Normal",
            Self::Comfortable => "Comfortable",
        }
    }

    pub fn scale(self) -> f32 {
        match self {
            Self::Compact => 0.82,
            Self::Normal => 1.0,
            Self::Comfortable => 1.18,
        }
    }
}

/// Which renderer configuration the user asked for.
///
/// GPUI has no software rasteriser, so "Compatibility" cannot mean CPU
/// rendering. What it does mean is spelled out in `Effects`: every expensive
/// visual is switched off and the repaint rate is capped. The audio engine is
/// unaffected either way — it does not hold a single GPUI handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rendering {
    /// Probe the adapter at start-up and choose.
    #[default]
    Automatic,
    HardwareAccelerated,
    Compatibility,
}

impl Rendering {
    pub const ALL: [Self; 3] = [
        Self::Automatic,
        Self::HardwareAccelerated,
        Self::Compatibility,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::HardwareAccelerated => "Hardware accelerated",
            Self::Compatibility => "Compatibility",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Automatic => "Pick based on the graphics adapter found at start-up.",
            Self::HardwareAccelerated => "Every effect enabled.",
            Self::Compatibility => "No blur, transparency or animation, and a capped repaint rate.",
        }
    }
}

/// The effect budget. Every expensive visual asks this struct first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effects {
    pub animations: bool,
    pub artwork_animations: bool,
    pub visualizations: bool,
    pub blur: bool,
    pub transparency: bool,
}

impl Default for Effects {
    fn default() -> Self {
        // The defaults are deliberately modest: the app should feel immediate,
        // not animated. Blur and transparency are opt-in.
        Self {
            animations: true,
            artwork_animations: false,
            visualizations: true,
            blur: false,
            transparency: false,
        }
    }
}

impl Effects {
    /// Everything off. What Compatibility mode installs.
    pub fn minimal() -> Self {
        Self {
            animations: false,
            artwork_animations: false,
            visualizations: false,
            blur: false,
            transparency: false,
        }
    }

    pub fn for_rendering(self, rendering: Rendering, hardware: bool) -> Self {
        match rendering {
            Rendering::Compatibility => Self::minimal(),
            Rendering::HardwareAccelerated => self,
            // Automatic keeps the user's choices on real hardware and strips
            // them back on a software adapter such as WARP or llvmpipe.
            Rendering::Automatic => match hardware {
                true => self,
                false => Self::minimal(),
            },
        }
    }
}

/// What the user can override on top of a palette.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeOverrides {
    pub accent: Option<u32>,
    pub font_size: f32,
    pub rounding: Rounding,
    pub density: Density,
}

impl Default for ThemeOverrides {
    fn default() -> Self {
        Self {
            accent: None,
            font_size: 14.0,
            rounding: Rounding::default(),
            density: Density::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub dark: bool,

    pub background: Hsla,
    /// Panels and bars that sit on the background.
    pub surface: Hsla,
    /// Cards and rows that sit on a surface.
    pub raised: Hsla,
    pub overlay: Hsla,

    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub faint_foreground: Hsla,

    pub border: Hsla,
    pub border_strong: Hsla,

    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_foreground: Hsla,

    pub hover: Hsla,
    pub active: Hsla,
    pub selected: Hsla,

    pub danger: Hsla,
    pub danger_foreground: Hsla,
    pub warning: Hsla,
    pub success: Hsla,

    pub radius: Pixels,
    pub font_size: Pixels,
    pub metrics: Metrics,
    pub effects: Effects,
}

impl Global for Theme {}

/// The default accent: Tinnitus's iris. The same hue as the mark, pulled back
/// from full saturation — a 98%-saturated blue glows against a near-black
/// background and fights everything drawn on top of it.
pub const DEFAULT_ACCENT: u32 = 0x6a5cd6;

impl Theme {
    pub fn dark(overrides: &ThemeOverrides) -> Self {
        let raw = overrides.accent.unwrap_or(DEFAULT_ACCENT);
        let accent: Hsla = rgb(raw).into();
        Self {
            dark: true,
            background: rgb(0x0f1012).into(),
            surface: rgb(0x16171a).into(),
            raised: rgb(0x1d1f23).into(),
            overlay: rgb(0x202329).into(),

            foreground: rgb(0xe9eaee).into(),
            muted_foreground: rgb(0x9ca0aa).into(),
            faint_foreground: rgb(0x686c76).into(),

            border: rgb(0x26282e).into(),
            border_strong: rgb(0x34373f).into(),

            accent,
            accent_hover: lighten(accent, 0.06),
            accent_foreground: on_accent(raw),

            hover: with_alpha(rgb(0xffffff), 0.05),
            active: with_alpha(rgb(0xffffff), 0.09),
            selected: with_alpha(rgb(raw), 0.16),

            danger: rgb(0xe05a5a).into(),
            danger_foreground: rgb(0x1a0c0c).into(),
            warning: rgb(0xe0a95a).into(),
            success: rgb(0x5ec27a).into(),

            ..Self::shape(overrides)
        }
    }

    pub fn light(overrides: &ThemeOverrides) -> Self {
        // The same accent as the dark palette: dark enough already to carry
        // white text on a light background.
        let raw = overrides.accent.unwrap_or(DEFAULT_ACCENT);
        let accent: Hsla = rgb(raw).into();
        Self {
            dark: false,
            background: rgb(0xf6f7f9).into(),
            surface: rgb(0xffffff).into(),
            raised: rgb(0xf0f1f4).into(),
            overlay: rgb(0xffffff).into(),

            foreground: rgb(0x14161a).into(),
            muted_foreground: rgb(0x5a5f6a).into(),
            faint_foreground: rgb(0x8b909a).into(),

            border: rgb(0xdfe1e6).into(),
            border_strong: rgb(0xc6c9d0).into(),

            accent,
            accent_hover: darken(accent, 0.06),
            accent_foreground: on_accent(raw),

            hover: with_alpha(rgb(0x000000), 0.04),
            active: with_alpha(rgb(0x000000), 0.08),
            selected: with_alpha(rgb(raw), 0.14),

            danger: rgb(0xc63d3d).into(),
            danger_foreground: rgb(0xffffff).into(),
            warning: rgb(0xb37a1c).into(),
            success: rgb(0x2f8f4d).into(),

            ..Self::shape(overrides)
        }
    }

    /// The parts of a theme that do not depend on light or dark. The colour
    /// fields here are placeholders: every one of them is overwritten by the
    /// palette that spreads this value.
    fn shape(overrides: &ThemeOverrides) -> Self {
        let font_size = px(overrides.font_size.clamp(MIN_FONT, MAX_FONT));
        Self {
            dark: true,
            background: gpui::black(),
            surface: gpui::black(),
            raised: gpui::black(),
            overlay: gpui::black(),
            foreground: gpui::white(),
            muted_foreground: gpui::white(),
            faint_foreground: gpui::white(),
            border: gpui::black(),
            border_strong: gpui::black(),
            accent: gpui::white(),
            accent_hover: gpui::white(),
            accent_foreground: gpui::black(),
            hover: gpui::transparent_black(),
            active: gpui::transparent_black(),
            selected: gpui::transparent_black(),
            danger: gpui::white(),
            danger_foreground: gpui::black(),
            warning: gpui::white(),
            success: gpui::white(),

            radius: overrides.rounding.radius(),
            font_size,
            metrics: Metrics::new(font_size, overrides.density),
            effects: Effects::default(),
        }
    }

    pub fn for_look(look: Look, system_dark: bool, overrides: &ThemeOverrides) -> Self {
        match look {
            Look::Dark => Self::dark(overrides),
            Look::Light => Self::light(overrides),
            Look::System => match system_dark {
                true => Self::dark(overrides),
                false => Self::light(overrides),
            },
        }
    }

    /// A font size for one step of the type scale.
    pub fn text(&self, step: Text) -> Pixels {
        px((self.font_size / px(1.) * step.ratio()).round())
    }

    pub fn init(look: Look, system_dark: bool, overrides: &ThemeOverrides, cx: &mut App) {
        cx.set_global(Self::for_look(look, system_dark, overrides));
    }

    pub fn set(
        look: Look,
        system_dark: bool,
        overrides: &ThemeOverrides,
        effects: Effects,
        cx: &mut App,
    ) {
        let mut theme = Self::for_look(look, system_dark, overrides);
        theme.effects = effects;
        cx.set_global(theme);
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark(&ThemeOverrides::default())
    }
}

/// `cx.theme()` everywhere, rather than threading a theme through every call.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

fn with_alpha(color: Rgba, alpha: f32) -> Hsla {
    Hsla {
        a: alpha,
        ..color.into()
    }
}

/// The text that sits on top of the accent.
///
/// The accent is the user's to choose, so this is derived rather than fixed: a
/// dark accent takes near-white text, a bright one takes near-black. A hard-coded
/// pair would be unreadable on half the swatches the picker offers.
fn on_accent(color: u32) -> Hsla {
    // sRGB relative luminance, the same measure the contrast ratio is built on.
    let channel = |shift: u32| {
        let value = ((color >> shift) & 0xff) as f32 / 255.0;
        match value <= 0.04045 {
            true => value / 12.92,
            false => ((value + 0.055) / 1.055).powf(2.4),
        }
    };
    let luminance = 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0);
    // 0.18 is where white and black text contrast equally (about 4.6:1 each), so
    // either side of it is the more readable of the two.
    match luminance > 0.18 {
        true => rgb(0x15100a).into(),
        false => rgb(0xf7f7fb).into(),
    }
}

fn lighten(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: (color.l + amount).min(1.0),
        ..color
    }
}

fn darken(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: (color.l - amount).max(0.0),
        ..color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_differ_in_the_obvious_way() {
        let overrides = ThemeOverrides::default();
        let dark = Theme::dark(&overrides);
        let light = Theme::light(&overrides);
        assert!(dark.dark && !light.dark);
        assert!(
            dark.background.l < light.background.l,
            "the dark theme must have the darker background"
        );
        assert!(
            dark.foreground.l > dark.background.l,
            "dark text on a dark background would be unreadable"
        );
        assert!(light.foreground.l < light.background.l);
    }

    #[test]
    fn system_look_follows_the_system() {
        let overrides = ThemeOverrides::default();
        assert!(Theme::for_look(Look::System, true, &overrides).dark);
        assert!(!Theme::for_look(Look::System, false, &overrides).dark);
        // An explicit choice ignores the system.
        assert!(Theme::for_look(Look::Dark, false, &overrides).dark);
        assert!(!Theme::for_look(Look::Light, true, &overrides).dark);
    }

    #[test]
    fn font_size_is_clamped_to_something_legible() {
        let tiny = ThemeOverrides {
            font_size: 2.0,
            ..Default::default()
        };
        let huge = ThemeOverrides {
            font_size: 200.0,
            ..Default::default()
        };
        assert_eq!(Theme::dark(&tiny).font_size, px(MIN_FONT));
        assert_eq!(Theme::dark(&huge).font_size, px(MAX_FONT));
    }

    #[test]
    fn the_accent_can_be_overridden() {
        let custom = ThemeOverrides {
            accent: Some(0xff0000),
            ..Default::default()
        };
        let theme = Theme::dark(&custom);
        assert_eq!(theme.accent, Hsla::from(rgb(0xff0000)));
        assert_ne!(theme.accent, Theme::dark(&ThemeOverrides::default()).accent);
    }

    #[test]
    fn accent_text_flips_with_the_accent_it_sits_on() {
        // The default is dark enough to need light text; a pale accent needs the
        // other one, and a button label has to stay readable on both.
        let on_default = on_accent(DEFAULT_ACCENT);
        assert!(on_default.l > 0.8, "light text on the default accent");
        assert!(
            on_accent(0xffd966).l < 0.2,
            "dark text on a pale yellow accent"
        );
        assert_eq!(
            Theme::dark(&ThemeOverrides::default()).accent_foreground,
            on_default
        );
        assert_eq!(
            Theme::light(&ThemeOverrides::default()).accent_foreground,
            on_default
        );
    }

    #[test]
    fn compatibility_mode_turns_every_effect_off() {
        let everything = Effects {
            animations: true,
            artwork_animations: true,
            visualizations: true,
            blur: true,
            transparency: true,
        };
        let budget = everything.for_rendering(Rendering::Compatibility, true);
        assert_eq!(budget, Effects::minimal());

        // Hardware-accelerated keeps whatever the user asked for.
        assert_eq!(
            everything.for_rendering(Rendering::HardwareAccelerated, true),
            everything
        );
    }

    #[test]
    fn automatic_strips_effects_on_a_software_adapter() {
        let everything = Effects {
            animations: true,
            artwork_animations: true,
            visualizations: true,
            blur: true,
            transparency: true,
        };
        assert_eq!(
            everything.for_rendering(Rendering::Automatic, true),
            everything
        );
        assert_eq!(
            everything.for_rendering(Rendering::Automatic, false),
            Effects::minimal()
        );
    }

    #[test]
    fn density_changes_row_height_without_changing_text() {
        let base = |density| ThemeOverrides {
            density,
            ..Default::default()
        };
        let compact = Theme::dark(&base(Density::Compact));
        let comfortable = Theme::dark(&base(Density::Comfortable));
        assert!(compact.metrics.list_row < comfortable.metrics.list_row);
        assert_eq!(compact.font_size, comfortable.font_size);
    }
}
