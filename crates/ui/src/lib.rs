//! The design system.
//!
//! Knows about GPUI and the theme, and nothing else — no app state, no audio,
//! no database. Anything that needs those lives in `views`.

pub mod color_picker;
pub mod input;
pub mod menu;
pub mod metrics;
pub mod scrollbar;
pub mod slider;
pub mod table;
pub mod theme;
pub mod widgets;

pub use color_picker::ColorPicker;
pub use input::{Field, FieldEvent};
pub use menu::{Menu, MenuEntry};
pub use metrics::{LEADING, Metrics, NARROW, Text, VERY_NARROW, clock};
pub use scrollbar::{REGION as SCROLL_REGION, Scrollbar, scrolled};
pub use slider::Slider;
pub use table::{Column, Table};
pub use theme::{
    ActiveTheme, DEFAULT_ACCENT, Density, Effects, Look, MAX_FONT, MIN_FONT, Rendering, Rounding,
    Theme, ThemeOverrides,
};
pub use widgets::{Button, Icon, Separator, Tabs, Tooltip, Vacancy, eyebrow, faint, heading};
