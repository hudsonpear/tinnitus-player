//! ReplayGain: volume normalisation from values already in the file's tags.
//!
//! Nothing here analyses audio. Gains are read at scan time and stored in the
//! database; playback only converts decibels to a linear multiplier, which is a
//! constant for the whole track and therefore cannot click.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplayGainMode {
    #[default]
    Off,
    /// Level every track the same. Good for shuffle, flattens an album's dynamics.
    Track,
    /// Level whole albums, preserving the loudness relationships inside one.
    Album,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReplayGainSettings {
    pub mode: ReplayGainMode,
    /// Decibels added on top of the tag's value.
    pub preamp: f32,
    /// Pull the gain back when the tagged peak says the result would clip.
    pub prevent_clipping: bool,
    /// Decibels applied to tracks that carry no ReplayGain tags at all, so a
    /// half-tagged library does not lurch in volume.
    pub fallback: f32,
}

impl Default for ReplayGainSettings {
    fn default() -> Self {
        Self {
            mode: ReplayGainMode::Off,
            preamp: 0.0,
            prevent_clipping: true,
            fallback: 0.0,
        }
    }
}

/// What the scanner found in one file's tags.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrackGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// The linear multiplier to apply to a track. `1.0` means "leave it alone".
pub fn gain_for(settings: &ReplayGainSettings, tags: &TrackGain) -> f32 {
    if settings.mode == ReplayGainMode::Off {
        return 1.0;
    }

    // Album mode falls back to the track value rather than doing nothing: a
    // single loose track still deserves to be levelled.
    let (gain_db, peak) = match settings.mode {
        ReplayGainMode::Album => (
            tags.album_gain_db.or(tags.track_gain_db),
            tags.album_peak.or(tags.track_peak),
        ),
        _ => (tags.track_gain_db, tags.track_peak),
    };

    let Some(gain_db) = gain_db else {
        return decibels_to_linear(settings.fallback);
    };

    let mut gain = decibels_to_linear(gain_db + settings.preamp);

    if settings.prevent_clipping {
        // A peak of 0.9 can only take 1/0.9 before it hits full scale.
        if let Some(peak) = peak.filter(|peak| *peak > 0.0) {
            gain = gain.min(1.0 / peak);
        }
    }

    // Even without a peak tag, refuse absurd amplification.
    gain.clamp(0.0, 4.0)
}

pub fn decibels_to_linear(decibels: f32) -> f32 {
    10f32.powf(decibels / 20.0)
}

pub fn linear_to_decibels(linear: f32) -> f32 {
    match linear > 0.0 {
        true => 20.0 * linear.log10(),
        false => f32::NEG_INFINITY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged() -> TrackGain {
        TrackGain {
            track_gain_db: Some(-6.0),
            track_peak: Some(0.9),
            album_gain_db: Some(-3.0),
            album_peak: Some(0.99),
        }
    }

    #[test]
    fn off_changes_nothing() {
        let settings = ReplayGainSettings::default();
        assert_eq!(gain_for(&settings, &tagged()), 1.0);
    }

    #[test]
    fn track_mode_uses_the_track_gain() {
        let settings = ReplayGainSettings {
            mode: ReplayGainMode::Track,
            ..Default::default()
        };
        let expected = decibels_to_linear(-6.0);
        assert!((gain_for(&settings, &tagged()) - expected).abs() < 1e-6);
    }

    #[test]
    fn album_mode_uses_the_album_gain() {
        let settings = ReplayGainSettings {
            mode: ReplayGainMode::Album,
            ..Default::default()
        };
        let expected = decibels_to_linear(-3.0);
        assert!((gain_for(&settings, &tagged()) - expected).abs() < 1e-6);
    }

    #[test]
    fn album_mode_falls_back_to_the_track_gain() {
        let settings = ReplayGainSettings {
            mode: ReplayGainMode::Album,
            ..Default::default()
        };
        let loose = TrackGain {
            album_gain_db: None,
            album_peak: None,
            ..tagged()
        };
        let expected = decibels_to_linear(-6.0);
        assert!((gain_for(&settings, &loose) - expected).abs() < 1e-6);
    }

    #[test]
    fn clip_prevention_caps_a_boost() {
        let settings = ReplayGainSettings {
            mode: ReplayGainMode::Track,
            preamp: 12.0,
            prevent_clipping: true,
            fallback: 0.0,
        };
        // +6 dB net would be 2.0x, but a peak of 0.9 only allows 1.111x.
        let quiet = TrackGain {
            track_gain_db: Some(-6.0),
            track_peak: Some(0.9),
            ..Default::default()
        };
        let gain = gain_for(&settings, &quiet);
        assert!(
            (gain - 1.0 / 0.9).abs() < 1e-6,
            "expected the peak to cap the gain, got {gain}"
        );

        // With clip prevention off the full boost comes through.
        let unclipped = ReplayGainSettings {
            prevent_clipping: false,
            ..settings
        };
        assert!(gain_for(&unclipped, &quiet) > 1.9);
    }

    #[test]
    fn an_untagged_track_uses_the_fallback() {
        let settings = ReplayGainSettings {
            mode: ReplayGainMode::Track,
            fallback: -6.0,
            ..Default::default()
        };
        let bare = TrackGain::default();
        let expected = decibels_to_linear(-6.0);
        assert!((gain_for(&settings, &bare) - expected).abs() < 1e-6);
    }

    #[test]
    fn absurd_tags_cannot_blow_the_output_up() {
        let settings = ReplayGainSettings {
            mode: ReplayGainMode::Track,
            prevent_clipping: false,
            ..Default::default()
        };
        let hostile = TrackGain {
            track_gain_db: Some(120.0),
            ..Default::default()
        };
        assert_eq!(gain_for(&settings, &hostile), 4.0);
    }

    #[test]
    fn decibels_round_trip() {
        for db in [-24.0f32, -6.0, 0.0, 6.0] {
            let back = linear_to_decibels(decibels_to_linear(db));
            assert!((back - db).abs() < 1e-4, "{db} came back as {back}");
        }
        assert_eq!(linear_to_decibels(0.0), f32::NEG_INFINITY);
    }
}
