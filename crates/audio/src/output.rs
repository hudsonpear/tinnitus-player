//! The output chain: decks, mixer, equalizer, master gain, device.
//!
//! ```text
//! deck A (rodio::Player) ─┐
//!                         ├─ mixer ─ Equalizer ─ SmoothGain(+spectrum tap) ─ device
//! deck B (rodio::Player) ─┘
//! ```
//!
//! Two decks exist so a crossfade has somewhere to fade *to*. Ordinary gapless
//! playback uses one deck and rodio's own queue, which is seamless by
//! construction.

use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use rodio::source::SeekError;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Source};

use crate::dsp::{EqHandle, Equalizer};
use crate::spectrum::{Spectrum, Tap};

/// How long a volume change takes to reach its new level. Long enough to avoid
/// a click, short enough that dragging the slider feels immediate.
pub const RAMP: Duration = Duration::from_millis(25);

/// Number of decks. Two is what a crossfade needs; there is no reason for more.
pub const DECKS: usize = 2;

/// The loudest the master gain is ever allowed to go: ten times the signal, the
/// top of the Settings ceiling. Nothing downstream of here has headroom to
/// spare, so this is a hard stop rather than a preference.
pub const MAX_GAIN: f32 = 10.0;

/// Keeps a boosted signal inside ±1 instead of letting it square off.
///
/// `tanh` is the whole trick: bounded, monotonic, and near enough to the
/// identity below about a third of full scale that quiet passages are
/// untouched. Only reached when the gain is above unity, so ordinary playback
/// goes through it never having been asked.
pub fn soft_clip(sample: f32) -> f32 {
    sample.tanh()
}

/// A volume shared between the UI thread and the audio thread, as raw f32 bits
/// in an atomic. No lock on the audio path.
#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn new(gain: f32) -> Self {
        Self(Arc::new(AtomicU32::new(gain.max(0.0).to_bits())))
    }

    pub fn set(&self, gain: f32) {
        self.0.store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

pub struct Output {
    decks: Vec<Arc<rodio::Player>>,
    volume: Volume,
    device: String,
    failed: Arc<AtomicBool>,
    /// Dropping the stream stops the audio, so it is held for as long as the
    /// output lives.
    _stream: MixerDeviceSink,
}

impl Output {
    /// Opens the default device and wires up the whole chain.
    pub fn open(volume: Volume, spectrum: &Spectrum, eq: EqHandle) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no audio output device")?;
        let default = device
            .default_output_config()
            .map_err(|error| anyhow::anyhow!("cannot read the output config: {error}"))?;

        let name = ident(&device);
        // cpal reports plain integers; rodio's mixer wants them proven non-zero.
        // A device claiming zero channels or a zero sample rate is broken, so
        // fall back to something playable rather than panicking on it.
        let channels =
            NonZero::new(default.channels()).unwrap_or(NonZero::new(2).expect("2 is not zero"));
        let sample_rate = NonZero::new(default.sample_rate())
            .unwrap_or(NonZero::new(48_000).expect("48000 is not zero"));
        log::info!(
            "audio: using {name} at {} Hz, {} channels, {}",
            sample_rate.get(),
            channels.get(),
            default.sample_format()
        );

        let failed = Arc::new(AtomicBool::new(false));
        let stream_failed = failed.clone();
        let mut stream = DeviceSinkBuilder::default()
            .with_device(device)
            .with_config(&default.config())
            .with_sample_format(default.sample_format())
            .with_error_callback(move |error| {
                log::warn!("audio: output failed: {error}");
                stream_failed.store(true, Ordering::Release);
            })
            .open_stream()
            .map_err(|error| anyhow::anyhow!("cannot open the audio output: {error}"))?;
        // We log device loss ourselves, with context rodio does not have.
        stream.log_on_drop(false);

        // Our own mixer, so the equalizer and the spectrum tap sit downstream of
        // both decks and run once rather than once per deck.
        let (mixer, mixed) = rodio::mixer::mixer(channels, sample_rate);
        let decks: Vec<Arc<rodio::Player>> = (0..DECKS)
            .map(|_| {
                let (player, source) = rodio::Player::new();
                player.pause();
                mixer.add(source);
                Arc::new(player)
            })
            .collect();

        let tap = spectrum.attach(sample_rate.get(), channels.get());
        let applied = volume.get();
        stream.mixer().add(
            SmoothGain::new(Equalizer::new(mixed, eq), volume.clone(), applied, RAMP).with_tap(tap),
        );

        Ok(Self {
            decks,
            volume,
            device: name,
            failed,
            _stream: stream,
        })
    }

    pub fn deck(&self, index: usize) -> &Arc<rodio::Player> {
        &self.decks[index % DECKS]
    }

    pub fn set_volume(&self, gain: f32) {
        self.volume.set(gain);
    }

    /// True once the device reported an error. Playback cannot recover in place;
    /// the engine restarts the output.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// True when the system's default output is no longer the one we opened —
    /// headphones plugged in, a monitor's speakers going away.
    pub fn changed(&self) -> bool {
        cpal::default_host()
            .default_output_device()
            .map(|device| ident(&device))
            .is_none_or(|device| device != self.device)
    }

    pub fn device_name(&self) -> &str {
        &self.device
    }
}

/// Applies a gain that ramps toward its target instead of jumping, and taps the
/// result for the visualizers.
pub struct SmoothGain<I> {
    input: I,
    volume: Volume,
    tap: Option<Tap>,

    current: f32,
    target: f32,
    step: f32,

    ramp: Duration,
    frames_left: u32,
    ramp_frames: u32,

    channel: u16,
    channels: u16,
    rate: u32,
}

impl<I: Source> SmoothGain<I> {
    pub fn new(input: I, volume: Volume, initial: f32, ramp: Duration) -> Self {
        Self {
            input,
            volume,
            tap: None,
            current: initial,
            target: initial,
            step: 0.0,
            ramp,
            frames_left: 0,
            ramp_frames: 1,
            channel: 0,
            channels: 0,
            rate: 0,
        }
    }

    pub fn with_tap(mut self, tap: Tap) -> Self {
        self.tap = Some(tap);
        self
    }

    fn resync(&mut self) {
        let channels = self.input.channels().get();
        let rate = self.input.sample_rate().get();
        if channels == self.channels && rate == self.rate {
            return;
        }
        self.channels = channels;
        self.rate = rate;
        self.ramp_frames = (self.ramp.as_secs_f64() * rate as f64).round().max(1.0) as u32;
        self.frames_left = self.frames_left.min(self.ramp_frames);
        if let Some(tap) = self.tap.as_mut() {
            tap.set_channels(channels);
        }
    }
}

impl<I: Source> Iterator for SmoothGain<I> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.input.next()?;

        // Gain is recomputed once per frame, not once per sample, so the two
        // channels of a stereo frame never get different gains.
        if self.channel == 0 {
            self.resync();
            let requested = self.volume.get();

            if requested.to_bits() != self.target.to_bits() {
                self.target = requested;
                self.frames_left = self.ramp_frames;
                self.step = (self.target - self.current) / self.ramp_frames as f32;
            }
            if self.frames_left > 0 {
                self.current += self.step;
                self.frames_left -= 1;
                if self.frames_left == 0 {
                    self.current = self.target;
                }
            }
        }

        // Above unity the signal is being pushed past what the file already
        // uses, so it is rounded off rather than clipped square. At or below
        // unity — every ordinary listen — the multiply is all that happens.
        let output = match self.current > 1.0 {
            true => soft_clip(sample * self.current),
            false => sample * self.current,
        };
        if let Some(tap) = self.tap.as_mut() {
            tap.push(output);
        }

        self.channel += 1;
        if self.channel >= self.channels.max(1) {
            self.channel = 0;
        }
        Some(output)
    }
}

impl<I: Source> Source for SmoothGain<I> {
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
        self.input.try_seek(position)
    }
}

/// A stable identity for a device, so "did the default output change?" is a
/// string comparison rather than a guess.
fn ident(device: &cpal::Device) -> String {
    device
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| "unknown output device".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_round_trips_through_the_atomic() {
        let volume = Volume::new(0.5);
        assert_eq!(volume.get(), 0.5);
        volume.set(0.25);
        assert_eq!(volume.get(), 0.25);
        // Negative gain would invert the waveform; it is floored instead.
        volume.set(-3.0);
        assert_eq!(volume.get(), 0.0);
    }

    #[test]
    fn the_soft_clip_bounds_a_boost_without_touching_quiet_audio() {
        // Bounded: ten times full scale still lands inside the range the device
        // can carry, which is the whole point of having it. At the extreme it
        // saturates to exactly ±1 in f32 — full scale, never past it.
        assert!(soft_clip(10.0) <= 1.0 && soft_clip(-10.0) >= -1.0);
        assert!(soft_clip(2.0) < 1.0);
        // Monotonic, and odd — no phase surprise on the negative half.
        assert!(soft_clip(0.5) < soft_clip(0.9));
        assert!((soft_clip(-0.5) + soft_clip(0.5)).abs() < 1e-6);
        // Near the identity where the music actually is.
        assert!((soft_clip(0.1) - 0.1).abs() < 0.001);
        assert_eq!(soft_clip(0.0), 0.0);
    }

    #[test]
    fn a_volume_change_ramps_instead_of_jumping() {
        let source = rodio::source::SineWave::new(440.0);
        let volume = Volume::new(0.0);
        let mut gain = SmoothGain::new(source, volume.clone(), 0.0, RAMP);

        // Prime the format so the ramp length is known.
        gain.next();
        volume.set(1.0);

        let ramp_frames = (RAMP.as_secs_f64() * gain.rate as f64) as usize;
        for _ in 0..4 {
            gain.next();
        }
        assert!(
            (gain.current - 1.0).abs() > 0.5,
            "the gain jumped straight to its target instead of ramping"
        );

        for _ in 0..ramp_frames * 2 {
            gain.next();
        }
        assert!(
            (gain.current - 1.0).abs() < 1e-4,
            "the ramp never reached its target: {}",
            gain.current
        );
    }
}
