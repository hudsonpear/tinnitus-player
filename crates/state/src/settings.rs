//! Settings: one versioned JSON file, written back debounced.
//!
//! Versioned from the first release so a later format change can migrate rather
//! than reset. An unreadable or half-written file falls back to the defaults and
//! says so in the log — losing a preference must never stop the app starting.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use audio::dsp::{CrossfadeSettings, EqSettings, ReplayGainSettings};
use serde::{Deserialize, Serialize};
use ui::theme::{Density, Effects, Look, Rendering, Rounding, ThemeOverrides};

/// Bumped whenever the shape changes in a way `migrate` has to handle.
pub const CURRENT_VERSION: u32 = 1;

/// What the seek bar looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Timeline {
    /// A plain line that fills as the track plays.
    Bar,
    /// The shape of the song itself, lit up to the play head.
    #[default]
    Waveform,
}

impl Timeline {
    pub const ALL: [Self; 2] = [Self::Bar, Self::Waveform];

    pub fn label(self) -> &'static str {
        match self {
            Self::Bar => "Bar",
            Self::Waveform => "Waveform",
        }
    }
}

/// Which side of the transport buttons the seek bar sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TimelinePlace {
    #[default]
    Above,
    Below,
}

impl TimelinePlace {
    pub const ALL: [Self; 2] = [Self::Above, Self::Below];

    pub fn label(self) -> &'static str {
        match self {
            Self::Above => "Above the buttons",
            Self::Below => "Below the buttons",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
    pub always_on_top: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 1120.0,
            height: 720.0,
            maximized: false,
            always_on_top: false,
        }
    }
}

impl WindowState {
    /// True when the saved geometry is usable. A window restored onto a monitor
    /// that is no longer attached is a window the user cannot reach.
    pub fn is_sane(&self) -> bool {
        self.width >= 480.0
            && self.height >= 400.0
            && self.width.is_finite()
            && self.height.is_finite()
            && self.x.is_finite()
            && self.y.is_finite()
    }
}

/// What was playing when the app last closed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub queue: Option<playlist::Queue>,
    pub track: Option<i64>,
    /// Seconds into the track.
    pub position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,

    // appearance
    pub look: Look,
    pub accent: Option<u32>,
    pub font_size: f32,
    pub rounding: Rounding,
    pub density: Density,
    pub sidebar_width: f32,
    /// The seek bar: what it looks like, and which side of the transport
    /// buttons it sits on. `serde(default)` so a settings file written before
    /// these existed still loads.
    #[serde(default)]
    pub timeline: Timeline,
    #[serde(default)]
    pub timeline_place: TimelinePlace,
    /// Which track-list columns are shown, by key.
    pub columns: Vec<String>,
    /// The queue panel down the right-hand side.
    pub queue_panel: bool,
    /// Searches the user actually ran, newest first, for the Search page to
    /// offer back. `serde(default)` so a settings file written before this
    /// existed still loads.
    #[serde(default)]
    pub search_history: Vec<String>,

    // performance
    pub rendering: Rendering,
    pub effects: Effects,

    // playback
    pub volume: f32,
    /// What a full slider means, as a multiplier: 1.0 is the file's own level,
    /// 10.0 is ten times it. The slider keeps its 0.0–1.0 meaning; this is the
    /// ceiling it runs up to. Default 1.0, which is byte-for-byte today's
    /// behaviour.
    pub max_volume: f32,
    pub muted: bool,
    pub speed: f32,
    pub shuffle: bool,
    pub repeat: playlist::Repeat,
    pub equalizer: EqSettings,
    pub replay_gain: ReplayGainSettings,
    pub crossfade: CrossfadeSettings,
    /// Resume where playback stopped, rather than at the start of the track.
    pub resume_playback: bool,
    /// Keep a volume and an equalizer curve per song, restoring them when it
    /// plays again. Off is today's behaviour exactly: one volume, one curve,
    /// for everything.
    pub remember_per_track_audio: bool,

    // library
    pub watch_folders: bool,
    pub scan_at_startup: bool,

    // window
    pub window: WindowState,
    pub session: Session,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,

            look: Look::Dark,
            accent: None,
            font_size: 14.0,
            rounding: Rounding::default(),
            density: Density::default(),
            sidebar_width: 212.0,
            timeline: Timeline::default(),
            timeline_place: TimelinePlace::default(),
            columns: default_columns(),
            queue_panel: true,
            search_history: vec![],

            rendering: Rendering::default(),
            effects: Effects::default(),

            volume: 0.7,
            max_volume: 1.0,
            muted: false,
            speed: 1.0,
            shuffle: false,
            repeat: playlist::Repeat::Off,
            equalizer: EqSettings::default(),
            replay_gain: ReplayGainSettings::default(),
            crossfade: CrossfadeSettings::default(),
            resume_playback: true,
            remember_per_track_audio: false,

            watch_folders: true,
            scan_at_startup: true,

            window: WindowState::default(),
            session: Session::default(),
        }
    }
}

/// How many past searches the Search page offers back.
pub const SEARCH_HISTORY: usize = 8;

/// The volume ceiling cannot go below unity — a "max volume" that made the
/// slider quieter would just be a second volume control.
pub const MIN_MAX_VOLUME: f32 = 1.0;
/// And no higher than the engine's own hard stop.
pub const MAX_MAX_VOLUME: f32 = 10.0;

/// The ceilings offered in Settings, as multipliers. 100% through 1000%.
pub const MAX_VOLUMES: [f32; 10] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];

/// What the engine is actually told, given where the slider is and how high the
/// user has let it reach. Pure: the slider's own value never moves.
pub fn effective_gain(volume: f32, ceiling: f32) -> f32 {
    volume.clamp(0.0, 1.0) * ceiling.clamp(MIN_MAX_VOLUME, MAX_MAX_VOLUME)
}

pub fn default_columns() -> Vec<String> {
    ["art", "title", "artist", "album", "duration", "favorite"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

impl Settings {
    pub fn path() -> PathBuf {
        library::data_dir().join("settings.json")
    }

    /// Reads the settings file. Anything unreadable yields the defaults, so a
    /// corrupted file costs the user their preferences and nothing else.
    pub fn load(path: &Path) -> Self {
        match Self::try_load(path) {
            Ok(settings) => settings,
            Err(error) => {
                if path.exists() {
                    log::warn!(
                        "settings: {} is unusable, starting from the defaults: {error:#}",
                        path.display()
                    );
                }
                Self::default()
            }
        }
    }

    fn try_load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).context("settings are not valid JSON")?;
        let migrated = migrate(value)?;
        let settings: Self =
            serde_json::from_value(migrated).context("settings do not match this version")?;
        Ok(settings.sanitized())
    }

    /// Writes through a temporary file so a crash mid-write cannot leave a
    /// truncated settings file behind.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("cannot serialize settings")?;
        let staging = path.with_extension("json.tmp");
        std::fs::write(&staging, text)
            .with_context(|| format!("cannot write {}", staging.display()))?;
        std::fs::rename(&staging, path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }

    /// Clamps everything that came from a file we do not control.
    pub fn sanitized(mut self) -> Self {
        self.version = CURRENT_VERSION;
        self.volume = clamp(self.volume, 0.0, 1.0, 0.7);
        // A hand-edited ceiling is the one setting in here that can damage
        // hearing or a speaker, so it is clamped like everything else and
        // falls back to "no boost" rather than to anything louder.
        self.max_volume = clamp(self.max_volume, MIN_MAX_VOLUME, MAX_MAX_VOLUME, 1.0);
        self.speed = clamp(self.speed, 0.25, 4.0, 1.0);
        self.font_size = clamp(self.font_size, ui::MIN_FONT, ui::MAX_FONT, 14.0);
        self.sidebar_width = clamp(self.sidebar_width, 150.0, 400.0, 212.0);
        self.equalizer = self.equalizer.clamped();
        self.crossfade.seconds = clamp(
            self.crossfade.seconds,
            0.5,
            CrossfadeSettings::MAX_SECONDS,
            5.0,
        );
        // Ratings are gone; a settings file written before they were removed
        // would otherwise still ask for a column nothing draws.
        self.columns.retain(|column| column != "rating");
        if self.columns.is_empty() {
            self.columns = default_columns();
        }
        // The favourite heart arrived after the first settings files were
        // written. It is how a track is favourited from a list, so a file that
        // predates it gains the column rather than losing the feature.
        if !self.columns.iter().any(|column| column == "favorite") {
            self.columns.push("favorite".to_owned());
        }
        // A hand-edited file could name a thousand past searches, and the page
        // only ever shows a handful.
        self.search_history.truncate(SEARCH_HISTORY);
        if !self.window.is_sane() {
            self.window = WindowState::default();
        }
        self
    }

    pub fn theme_overrides(&self) -> ThemeOverrides {
        ThemeOverrides {
            accent: self.accent,
            font_size: self.font_size,
            rounding: self.rounding,
            density: self.density,
        }
    }

    /// The effect budget actually in force, given the rendering mode and what
    /// the adapter turned out to be.
    pub fn effective_effects(&self, hardware: bool) -> Effects {
        self.effects.for_rendering(self.rendering, hardware)
    }
}

/// Brings an older settings document up to `CURRENT_VERSION`.
///
/// There is only one version so far, so this checks rather than converts — but
/// the seam exists, which is the point: version 2 adds an arm here instead of a
/// rewrite.
fn migrate(mut value: serde_json::Value) -> Result<serde_json::Value> {
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;

    anyhow::ensure!(
        version <= CURRENT_VERSION,
        "settings are version {version}, newer than this build understands ({CURRENT_VERSION})"
    );

    if let Some(object) = value.as_object_mut() {
        object.insert("version".into(), CURRENT_VERSION.into());
    }
    Ok(value)
}

fn clamp(value: f32, low: f32, high: f32, fallback: f32) -> f32 {
    match value.is_finite() {
        true => value.clamp(low, high),
        false => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let settings = Settings::default();
        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, settings);
    }

    #[test]
    fn a_missing_file_yields_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(&dir.path().join("nope.json"));
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn a_corrupt_file_yields_the_defaults_instead_of_failing_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn an_older_file_keeps_the_fields_it_does_have() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A file from a build that only ever wrote a couple of keys.
        std::fs::write(&path, r#"{"version":1,"volume":0.42,"look":"Light"}"#).unwrap();

        let settings = Settings::load(&path);
        assert_eq!(settings.volume, 0.42);
        assert_eq!(settings.look, Look::Light);
        // Everything absent falls back to its default rather than erroring.
        assert_eq!(settings.speed, 1.0);
        assert_eq!(settings.columns, default_columns());
    }

    #[test]
    fn a_file_from_the_future_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"version":99,"volume":0.1}"#).unwrap();
        // Refused, so it falls back to defaults instead of losing the user's
        // newer settings by writing an old shape over them.
        assert_eq!(Settings::load(&path).volume, Settings::default().volume);
    }

    #[test]
    fn the_ceiling_multiplies_the_slider_without_moving_it() {
        // Default ceiling: the slider means exactly what it always meant.
        assert_eq!(effective_gain(0.7, 1.0), 0.7);
        // 70% of a 300% ceiling is 210% — the slider is still at 0.7.
        assert!((effective_gain(0.7, 3.0) - 2.1).abs() < 1e-6);
        // Silence stays silent however high the ceiling goes.
        assert_eq!(effective_gain(0.0, 10.0), 0.0);
        // A ceiling from a hand-edited file cannot push past the hard stop, and
        // cannot quietly turn the slider down either.
        assert_eq!(effective_gain(1.0, 400.0), MAX_MAX_VOLUME);
        assert_eq!(effective_gain(1.0, 0.1), 1.0);
    }

    #[test]
    fn hostile_values_are_clamped() {
        let wild = Settings {
            volume: 40.0,
            max_volume: 900.0,
            speed: f32::NAN,
            font_size: 900.0,
            sidebar_width: -20.0,
            columns: vec![],
            window: WindowState {
                width: 1.0,
                height: 1.0,
                ..Default::default()
            },
            ..Default::default()
        }
        .sanitized();

        assert_eq!(wild.volume, 1.0);
        assert_eq!(wild.max_volume, MAX_MAX_VOLUME);
        assert_eq!(wild.speed, 1.0);
        assert_eq!(wild.font_size, ui::MAX_FONT);
        assert_eq!(wild.sidebar_width, 150.0);
        assert_eq!(wild.columns, default_columns());
        assert_eq!(wild.window, WindowState::default());
    }

    #[test]
    fn a_retired_column_is_dropped_from_an_older_file() {
        // Ratings are gone. A settings file that still asks for the column must
        // not leave an empty strip down the track list.
        let settings = Settings {
            columns: vec!["title".to_owned(), "rating".to_owned()],
            ..Default::default()
        }
        .sanitized();
        assert_eq!(
            settings.columns,
            vec!["title".to_owned(), "favorite".to_owned()]
        );

        // A file that asked for nothing else falls back to the defaults rather
        // than to an empty list.
        let only_ratings = Settings {
            columns: vec!["rating".to_owned()],
            ..Default::default()
        }
        .sanitized();
        assert_eq!(only_ratings.columns, default_columns());
    }

    #[test]
    fn saving_and_loading_preserves_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut queue = playlist::Queue::new();
        queue.fill(vec![7, 8, 9], 1);
        let settings = Settings {
            volume: 0.33,
            session: Session {
                queue: Some(queue),
                track: Some(8),
                position: 91.5,
            },
            ..Default::default()
        };
        settings.save(&path).unwrap();

        let back = Settings::load(&path);
        assert_eq!(back.volume, 0.33);
        assert_eq!(back.session.track, Some(8));
        assert_eq!(back.session.position, 91.5);
        assert_eq!(
            back.session.queue.map(|queue| queue.in_play_order()),
            Some(vec![7, 8, 9])
        );
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        Settings::default().save(&path).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn compatibility_rendering_overrides_the_stored_effects() {
        let settings = Settings {
            rendering: Rendering::Compatibility,
            effects: Effects {
                blur: true,
                animations: true,
                ..Effects::default()
            },
            ..Default::default()
        };
        assert_eq!(settings.effective_effects(true), Effects::minimal());
    }
}
