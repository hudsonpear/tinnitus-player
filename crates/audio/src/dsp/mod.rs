//! Signal processing that sits between the decoder and the output device.

pub mod eq;
pub mod replaygain;

use serde::{Deserialize, Serialize};

pub use eq::{EqHandle, EqSettings, Equalizer, Preset};
pub use replaygain::{ReplayGainMode, ReplayGainSettings, TrackGain};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CrossfadeSettings {
    pub enabled: bool,
    /// Seconds of overlap between two tracks.
    pub seconds: f32,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            seconds: 5.0,
        }
    }
}

impl CrossfadeSettings {
    pub const MAX_SECONDS: f32 = 12.0;

    pub fn duration(&self) -> Option<std::time::Duration> {
        (self.enabled && self.seconds > 0.0)
            .then(|| std::time::Duration::from_secs_f32(self.seconds.clamp(0.5, Self::MAX_SECONDS)))
    }
}

/// Equal-power crossfade. `progress` runs 0.0 to 1.0 across the overlap and the
/// pair is `(outgoing, incoming)`.
///
/// A linear fade dips in the middle, because two uncorrelated signals sum in
/// power rather than in amplitude; the sine/cosine pair keeps the total constant.
pub fn crossfade_gains(progress: f32) -> (f32, f32) {
    let progress = progress.clamp(0.0, 1.0);
    let angle = progress * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

/// Playback speeds the UI offers. The engine accepts any positive factor; this
/// is only the list of presets.
pub const SPEEDS: [f32; 7] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// ponytail: speed is rodio's resampling speed, so it shifts pitch like a record
/// player. Pitch-preserving playback needs a time-stretcher (`rubato` or
/// soundtouch) between the decoder and the mixer; add one only if the pitch
/// shift is judged unacceptable.
pub fn clamp_speed(factor: f32) -> f32 {
    factor.clamp(0.25, 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crossfade_holds_its_power_across_the_overlap() {
        for step in 0..=20 {
            let progress = step as f32 / 20.0;
            let (out, incoming) = crossfade_gains(progress);
            let power = out * out + incoming * incoming;
            assert!(
                (power - 1.0).abs() < 1e-5,
                "power dipped to {power} at {progress}"
            );
        }
    }

    #[test]
    fn a_crossfade_starts_and_ends_where_it_should() {
        assert_eq!(crossfade_gains(0.0), (1.0, 0.0));
        let (out, incoming) = crossfade_gains(1.0);
        assert!(out.abs() < 1e-6 && (incoming - 1.0).abs() < 1e-6);
        // Out of range values are clamped, not extrapolated.
        assert_eq!(crossfade_gains(-5.0), crossfade_gains(0.0));
        assert_eq!(crossfade_gains(5.0), crossfade_gains(1.0));
    }

    #[test]
    fn crossfade_is_off_and_bounded_by_default() {
        assert_eq!(CrossfadeSettings::default().duration(), None);

        let long = CrossfadeSettings {
            enabled: true,
            seconds: 999.0,
        };
        assert_eq!(
            long.duration(),
            Some(std::time::Duration::from_secs_f32(
                CrossfadeSettings::MAX_SECONDS
            ))
        );
    }

    #[test]
    fn speed_is_clamped_to_something_playable() {
        assert_eq!(clamp_speed(1.0), 1.0);
        assert_eq!(clamp_speed(0.0), 0.25);
        assert_eq!(clamp_speed(100.0), 4.0);
        assert!(SPEEDS.contains(&1.0));
    }
}
