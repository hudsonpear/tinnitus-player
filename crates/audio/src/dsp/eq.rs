//! Graphic equalizer: one peaking biquad per band, plus a preamp.
//!
//! Coefficients are computed off the audio thread and published through an
//! `ArcSwap`, so the filter's `next()` never allocates, never locks, and never
//! blocks. Changing a slider swaps a pointer; the audio thread picks it up on
//! its next frame.

use std::num::NonZero;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rodio::Source;
use rodio::source::SeekError;
use serde::{Deserialize, Serialize};

/// Band centre frequencies, in hertz.
pub const BANDS: [f32; 9] = [
    60.0, 170.0, 310.0, 600.0, 1_000.0, 3_000.0, 6_000.0, 12_000.0, 16_000.0,
];

/// Bandwidth of each peaking filter. The bands are spaced about 1.5 octaves
/// apart, and a Q of 1 gives them enough overlap to sum smoothly.
const Q: f32 = 1.0;

/// Gain limits, in decibels. Wider than this and the preamp cannot keep the
/// signal out of the clipper.
pub const MIN_GAIN_DB: f32 = -12.0;
pub const MAX_GAIN_DB: f32 = 12.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqSettings {
    pub enabled: bool,
    /// Decibels, applied before the bands.
    pub preamp: f32,
    /// One gain in decibels per entry of `BANDS`.
    pub gains: [f32; BANDS.len()],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp: 0.0,
            gains: [0.0; BANDS.len()],
        }
    }
}

impl EqSettings {
    pub fn from_preset(preset: Preset) -> Self {
        Self {
            enabled: true,
            preamp: preset.preamp(),
            gains: preset.gains(),
        }
    }

    /// True when the settings would leave the signal untouched, so the filter
    /// can be bypassed entirely rather than run at unity.
    pub fn is_flat(&self) -> bool {
        !self.enabled || (self.preamp == 0.0 && self.gains.iter().all(|gain| *gain == 0.0))
    }

    pub fn clamped(mut self) -> Self {
        self.preamp = self.preamp.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        for gain in &mut self.gains {
            *gain = gain.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    Flat,
    Rock,
    Pop,
    Classical,
    Jazz,
    Electronic,
    Vocal,
    BassBoost,
}

impl Preset {
    pub const ALL: [Preset; 8] = [
        Preset::Flat,
        Preset::Rock,
        Preset::Pop,
        Preset::Classical,
        Preset::Jazz,
        Preset::Electronic,
        Preset::Vocal,
        Preset::BassBoost,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::Rock => "Rock",
            Self::Pop => "Pop",
            Self::Classical => "Classical",
            Self::Jazz => "Jazz",
            Self::Electronic => "Electronic",
            Self::Vocal => "Vocal",
            Self::BassBoost => "Bass Boost",
        }
    }

    //                        60   170   310   600    1k    3k    6k   12k   16k
    pub fn gains(self) -> [f32; BANDS.len()] {
        match self {
            Self::Flat => [0.0; BANDS.len()],
            Self::Rock => [5.0, 3.5, -1.0, -2.0, -0.5, 2.0, 4.0, 5.0, 5.0],
            Self::Pop => [-1.5, 1.0, 3.5, 4.0, 3.0, 0.0, -1.0, -1.5, -2.0],
            Self::Classical => [3.5, 2.5, 0.0, 0.0, 0.0, 0.0, -2.0, -3.0, -4.0],
            Self::Jazz => [3.0, 2.0, 0.5, 1.5, -1.0, -1.0, 0.5, 2.0, 3.0],
            Self::Electronic => [5.0, 4.0, 0.5, -1.5, -2.5, 0.5, 2.0, 4.5, 5.5],
            Self::Vocal => [-3.0, -2.0, 0.5, 3.5, 4.5, 4.0, 2.0, 0.0, -1.0],
            Self::BassBoost => [8.0, 6.0, 3.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        }
    }

    /// Presets that add energy pull the preamp down so they cannot clip.
    pub fn preamp(self) -> f32 {
        let peak = self
            .gains()
            .into_iter()
            .fold(0.0f32, |highest, gain| highest.max(gain));
        -peak.max(0.0) * 0.6
    }
}

/// Second-order section coefficients, already normalised by `a0`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// RBJ cookbook peaking filter.
    fn peaking(frequency: f32, sample_rate: f32, q: f32, gain_db: f32) -> Self {
        // Above Nyquist a peaking filter is meaningless; pass the signal through.
        if frequency >= sample_rate / 2.0 {
            return Self::identity();
        }
        let amplitude = 10f32.powf(gain_db / 40.0);
        let omega = 2.0 * std::f32::consts::PI * frequency / sample_rate;
        let (sin, cos) = omega.sin_cos();
        let alpha = sin / (2.0 * q);

        let a0 = 1.0 + alpha / amplitude;
        Self {
            b0: (1.0 + alpha * amplitude) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * amplitude) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / amplitude) / a0,
        }
    }

    fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

/// Per-channel delay line for one biquad.
#[derive(Debug, Clone, Copy, Default)]
struct State {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl State {
    #[inline]
    fn step(&mut self, filter: &Biquad, input: f32) -> f32 {
        let output = filter.b0 * input + filter.b1 * self.x1 + filter.b2 * self.x2
            - filter.a1 * self.y1
            - filter.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

/// The published filter shape: what the audio thread reads.
#[derive(Debug, Clone)]
struct Curve {
    bypass: bool,
    preamp: f32,
    sections: Vec<Biquad>,
    sample_rate: u32,
}

impl Curve {
    fn bypass() -> Self {
        Self {
            bypass: true,
            preamp: 1.0,
            sections: vec![],
            sample_rate: 0,
        }
    }

    fn build(settings: &EqSettings, sample_rate: u32) -> Self {
        if settings.is_flat() {
            let mut curve = Self::bypass();
            curve.sample_rate = sample_rate;
            return curve;
        }
        Self {
            bypass: false,
            preamp: 10f32.powf(settings.preamp / 20.0),
            sections: BANDS
                .iter()
                .zip(settings.gains)
                .filter(|(_, gain)| *gain != 0.0)
                .map(|(frequency, gain)| Biquad::peaking(*frequency, sample_rate as f32, Q, gain))
                .collect(),
            sample_rate,
        }
    }
}

/// The handle the UI holds. Cloning it is cheap; every clone points at the same
/// published curve.
#[derive(Clone)]
pub struct EqHandle {
    settings: Arc<ArcSwap<EqSettings>>,
    curve: Arc<ArcSwap<Curve>>,
}

impl EqHandle {
    pub fn new(settings: EqSettings) -> Self {
        Self {
            curve: Arc::new(ArcSwap::from_pointee(Curve::bypass())),
            settings: Arc::new(ArcSwap::from_pointee(settings.clamped())),
        }
    }

    /// Recomputes and publishes the curve. Called from the UI thread; the audio
    /// thread only ever loads.
    pub fn set(&self, settings: EqSettings) {
        let settings = settings.clamped();
        let sample_rate = self.curve.load().sample_rate;
        self.settings.store(Arc::new(settings.clone()));
        if sample_rate > 0 {
            self.curve
                .store(Arc::new(Curve::build(&settings, sample_rate)));
        }
    }

    pub fn settings(&self) -> EqSettings {
        EqSettings::clone(&self.settings.load())
    }

    /// Called by the filter when it learns the stream's sample rate, which is
    /// only known once audio is flowing.
    fn retune(&self, sample_rate: u32) {
        let settings = self.settings.load();
        self.curve
            .store(Arc::new(Curve::build(&settings, sample_rate)));
    }
}

impl Default for EqHandle {
    fn default() -> Self {
        Self::new(EqSettings::default())
    }
}

/// The `Source` wrapper that actually filters. One per output chain.
pub struct Equalizer<I> {
    input: I,
    handle: EqHandle,
    /// `states[channel][section]`
    states: Vec<Vec<State>>,
    curve: Arc<Curve>,
    channel: u16,
    channels: u16,
    sample_rate: u32,
}

impl<I: Source> Equalizer<I> {
    pub fn new(input: I, handle: EqHandle) -> Self {
        Self {
            curve: handle.curve.load_full(),
            input,
            handle,
            states: vec![],
            channel: 0,
            channels: 0,
            sample_rate: 0,
        }
    }

    /// Re-reads the published curve and resizes the delay lines. Runs once per
    /// frame, not per sample, and allocates only when the stream format or the
    /// number of active bands actually changes.
    fn resync(&mut self) {
        let channels = self.input.channels().get();
        let sample_rate = self.input.sample_rate().get();

        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.handle.retune(sample_rate);
        }

        let published = self.handle.curve.load_full();
        let sections_changed = published.sections.len() != self.curve.sections.len();
        self.curve = published;

        if channels != self.channels || sections_changed || self.states.len() != channels as usize {
            self.channels = channels;
            self.states =
                vec![vec![State::default(); self.curve.sections.len()]; channels as usize];
        }
    }
}

impl<I: Source> Iterator for Equalizer<I> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.input.next()?;

        if self.channel == 0 {
            self.resync();
        }
        let channel = self.channel as usize;
        self.channel = (self.channel + 1) % self.channels.max(1);

        if self.curve.bypass {
            return Some(sample);
        }

        let mut value = sample * self.curve.preamp;
        if let Some(states) = self.states.get_mut(channel) {
            for (state, filter) in states.iter_mut().zip(&self.curve.sections) {
                value = state.step(filter, value);
            }
        }
        // A resonant band can push past full scale; hard-limit rather than let
        // the output device wrap around into a click.
        Some(value.clamp(-1.0, 1.0))
    }
}

impl<I: Source> Source for Equalizer<I> {
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> NonZero<u16> {
        self.input.channels()
    }

    fn sample_rate(&self) -> NonZero<u32> {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        // Old samples in the delay lines belong to a different part of the song.
        for states in &mut self.states {
            states.fill(State::default());
        }
        self.input.try_seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a sine through one biquad and reports its RMS gain in decibels.
    fn gain_at(filter: &Biquad, frequency: f32, sample_rate: f32) -> f32 {
        let mut state = State::default();
        let samples = (sample_rate * 0.5) as usize;
        let mut input_energy = 0.0f64;
        let mut output_energy = 0.0f64;

        for index in 0..samples {
            let phase = 2.0 * std::f32::consts::PI * frequency * index as f32 / sample_rate;
            let input = phase.sin();
            let output = state.step(filter, input);
            // Skip the first tenth: the filter has to settle first.
            if index > samples / 10 {
                input_energy += f64::from(input) * f64::from(input);
                output_energy += f64::from(output) * f64::from(output);
            }
        }
        20.0 * (output_energy / input_energy).sqrt().log10() as f32
    }

    #[test]
    fn a_peaking_band_boosts_its_own_frequency() {
        let filter = Biquad::peaking(1000.0, 44100.0, Q, 6.0);
        let boost = gain_at(&filter, 1000.0, 44100.0);
        assert!(
            (boost - 6.0).abs() < 0.5,
            "expected about +6 dB at the centre, got {boost}"
        );
    }

    #[test]
    fn a_peaking_band_leaves_distant_frequencies_alone() {
        let filter = Biquad::peaking(60.0, 44100.0, Q, 12.0);
        let far = gain_at(&filter, 10_000.0, 44100.0);
        assert!(far.abs() < 0.5, "expected no change at 10 kHz, got {far}");
    }

    #[test]
    fn a_cut_attenuates() {
        let filter = Biquad::peaking(3000.0, 44100.0, Q, -9.0);
        let cut = gain_at(&filter, 3000.0, 44100.0);
        assert!((cut + 9.0).abs() < 0.5, "expected about -9 dB, got {cut}");
    }

    #[test]
    fn a_zero_gain_band_is_a_pass_through() {
        let filter = Biquad::peaking(1000.0, 44100.0, Q, 0.0);
        for frequency in [100.0, 1000.0, 8000.0] {
            let gain = gain_at(&filter, frequency, 44100.0);
            assert!(
                gain.abs() < 0.01,
                "unity band changed {frequency} Hz by {gain}"
            );
        }
    }

    #[test]
    fn bands_above_nyquist_are_neutral_rather_than_unstable() {
        // A 16 kHz band at an 8 kHz sample rate has no meaning; it must not blow up.
        let filter = Biquad::peaking(16_000.0, 8_000.0, Q, 12.0);
        assert_eq!(filter, Biquad::identity());

        let mut state = State::default();
        let mut value = 0.0;
        for index in 0..1000 {
            value = state.step(&filter, (index as f32).sin());
        }
        assert!(value.is_finite());
    }

    #[test]
    fn flat_settings_bypass_the_filter_entirely() {
        assert!(EqSettings::default().is_flat());
        assert!(Curve::build(&EqSettings::default(), 44100).bypass);

        let disabled = EqSettings {
            enabled: false,
            preamp: 6.0,
            gains: [6.0; BANDS.len()],
        };
        assert!(disabled.is_flat(), "a disabled EQ must not filter");
    }

    #[test]
    fn only_non_zero_bands_cost_anything() {
        let mut gains = [0.0; BANDS.len()];
        gains[0] = 4.0;
        gains[4] = -2.0;
        let settings = EqSettings {
            enabled: true,
            gains,
            ..Default::default()
        };
        let curve = Curve::build(&settings, 44100);
        assert_eq!(curve.sections.len(), 2);
    }

    #[test]
    fn gains_are_clamped_to_the_usable_range() {
        let wild = EqSettings {
            enabled: true,
            preamp: 100.0,
            gains: [-100.0; BANDS.len()],
        }
        .clamped();
        assert_eq!(wild.preamp, MAX_GAIN_DB);
        assert!(wild.gains.iter().all(|gain| *gain == MIN_GAIN_DB));
    }

    #[test]
    fn boosting_presets_pull_the_preamp_down() {
        assert_eq!(Preset::Flat.preamp(), 0.0);
        assert!(
            Preset::BassBoost.preamp() < 0.0,
            "a preset that adds 8 dB of bass must reduce the preamp"
        );
        for preset in Preset::ALL {
            let settings = EqSettings::from_preset(preset);
            assert_eq!(settings.gains, preset.gains());
        }
    }

    #[test]
    fn the_handle_publishes_what_the_filter_reads() {
        let handle = EqHandle::new(EqSettings::default());
        handle.retune(44100);
        assert!(handle.curve.load().bypass);

        handle.set(EqSettings::from_preset(Preset::Rock));
        assert!(!handle.curve.load().bypass);
        assert_eq!(handle.settings().gains, Preset::Rock.gains());
    }
}
