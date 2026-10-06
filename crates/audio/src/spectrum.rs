//! Spectrum tap for the visualizers.
//!
//! The audio side pushes into a lock-free ring and never allocates or blocks;
//! if the UI stops draining (a hidden visualizer, a stalled frame) the ring
//! simply fills and the pushes are dropped. Audio must never wait on drawing.
//!
//! The FFT happens on the UI side, and only while something is actually being
//! drawn: a hidden visualizer costs one ring push per frame and nothing more.

use std::sync::{Arc, Mutex};

use rtrb::{Consumer, Producer, RingBuffer};
use rustfft::{FftPlanner, num_complex::Complex32};

/// Samples held in the ring. About 0.17 s at 48 kHz, which is enough for a
/// 2048-point window plus slack for a late frame.
const RING: usize = 8192;

/// FFT window. A power of two, and long enough to resolve the bass bands.
const WINDOW: usize = 2048;

/// The producer end, owned by the audio chain.
pub struct Tap {
    producer: Producer<f32>,
    /// Only the first channel is tapped: a spectrum display does not need a
    /// stereo downmix, and this keeps the hot path to one branch.
    channels: u16,
    channel: u16,
}

impl Tap {
    /// Pushes one sample. Called once per sample on the audio thread, so it does
    /// nothing but an index check and a store.
    #[inline]
    pub fn push(&mut self, sample: f32) {
        if self.channel == 0 {
            // A full ring means the UI is not keeping up; dropping is correct.
            let _ = self.producer.push(sample);
        }
        self.channel += 1;
        if self.channel >= self.channels.max(1) {
            self.channel = 0;
        }
    }

    pub fn set_channels(&mut self, channels: u16) {
        if channels != self.channels {
            self.channels = channels.max(1);
            self.channel = 0;
        }
    }
}

/// The consumer end, cloned freely by the UI.
#[derive(Clone)]
pub struct Spectrum {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    consumer: Option<Consumer<f32>>,
    sample_rate: u32,
    window: Vec<f32>,
    scratch: Vec<Complex32>,
    planner: FftPlanner<f32>,
    /// Smoothed magnitudes, so bars fall rather than flicker.
    smoothed: Vec<f32>,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                consumer: None,
                sample_rate: 0,
                window: Vec::with_capacity(WINDOW),
                scratch: vec![Complex32::default(); WINDOW],
                planner: FftPlanner::new(),
                smoothed: vec![],
            })),
        }
    }

    /// Creates the producer for a new output chain. Any previous tap is dropped,
    /// which is what should happen when the output device changes.
    pub fn attach(&self, sample_rate: u32, channels: u16) -> Tap {
        let (producer, consumer) = RingBuffer::new(RING);
        if let Ok(mut inner) = self.inner.lock() {
            inner.consumer = Some(consumer);
            inner.sample_rate = sample_rate;
            inner.window.clear();
            inner.smoothed.clear();
        }
        Tap {
            producer,
            channels: channels.max(1),
            channel: 0,
        }
    }

    /// Drains the ring without analysing it. Call this while the visualizer is
    /// hidden so the ring does not go stale.
    pub fn discard(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(consumer) = inner.consumer.as_mut() {
            while consumer.pop().is_ok() {}
        }
        inner.window.clear();
    }

    /// `bands` magnitudes in 0.0..=1.0, log-spaced across the audible range.
    /// Returns an empty vector when there is nothing playing.
    pub fn bands(&self, bands: usize) -> Vec<f32> {
        let Ok(mut inner) = self.inner.lock() else {
            return vec![];
        };
        inner.bands(bands)
    }

    /// Raw recent samples, for the oscilloscope and waveform views.
    pub fn waveform(&self, points: usize) -> Vec<f32> {
        let Ok(mut inner) = self.inner.lock() else {
            return vec![];
        };
        inner.fill();
        if points == 0 || inner.window.len() < points {
            return vec![];
        }
        let step = inner.window.len() / points;
        (0..points)
            .map(|index| inner.window[index * step])
            .collect()
    }

    /// Peak level of the tapped channel, for VU meters.
    pub fn peak(&self) -> f32 {
        let Ok(mut inner) = self.inner.lock() else {
            return 0.0;
        };
        inner.fill();
        inner
            .window
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
            .min(1.0)
    }
}

impl Inner {
    /// Moves whatever the audio thread has produced into the rolling window.
    fn fill(&mut self) {
        let Some(consumer) = self.consumer.as_mut() else {
            return;
        };
        while let Ok(sample) = consumer.pop() {
            self.window.push(sample);
        }
        if self.window.len() > WINDOW {
            let excess = self.window.len() - WINDOW;
            self.window.drain(..excess);
        }
    }

    fn bands(&mut self, bands: usize) -> Vec<f32> {
        self.fill();
        if bands == 0 || self.window.len() < WINDOW || self.sample_rate == 0 {
            return vec![];
        }

        // Hann window, to stop the edges of the block ringing across every bin.
        for (index, slot) in self.scratch.iter_mut().enumerate() {
            let taper =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * index as f32 / WINDOW as f32).cos();
            *slot = Complex32::new(self.window[index] * taper, 0.0);
        }

        let fft = self.planner.plan_fft_forward(WINDOW);
        fft.process(&mut self.scratch);

        // Log-spaced edges from 30 Hz to just under Nyquist: a linear split would
        // give eleven bars to the top octave and one to everything you can hear.
        let nyquist = self.sample_rate as f32 / 2.0;
        let low = 30.0f32;
        let high = nyquist.min(18_000.0).max(low * 2.0);
        let bin_of =
            |frequency: f32| ((frequency / nyquist) * (WINDOW as f32 / 2.0)).round() as usize;

        if self.smoothed.len() != bands {
            self.smoothed = vec![0.0; bands];
        }

        let mut out = Vec::with_capacity(bands);
        for band in 0..bands {
            let start_hz = low * (high / low).powf(band as f32 / bands as f32);
            let end_hz = low * (high / low).powf((band + 1) as f32 / bands as f32);
            let start = bin_of(start_hz).clamp(1, WINDOW / 2 - 1);
            let end = bin_of(end_hz).max(start + 1).min(WINDOW / 2);

            let peak = self.scratch[start..end]
                .iter()
                .map(|value| value.norm())
                .fold(0.0f32, f32::max);

            // Decibels, mapped onto 0..1 over a 60 dB window. Raw amplitude
            // would leave every bar but the bass invisible.
            let normalized = peak / (WINDOW as f32 / 4.0);
            let db = 20.0 * normalized.max(1e-6).log10();
            let level = ((db + 60.0) / 60.0).clamp(0.0, 1.0);

            // Attack fast, release slow: bars jump to a beat and fall smoothly.
            let previous = self.smoothed[band];
            let smoothed = match level > previous {
                true => level,
                false => previous * 0.82 + level * 0.18,
            };
            self.smoothed[band] = smoothed;
            out.push(smoothed);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pushes a sine wave through the tap, one channel.
    fn feed(tap: &mut Tap, frequency: f32, sample_rate: u32, samples: usize) {
        for index in 0..samples {
            let phase = 2.0 * std::f32::consts::PI * frequency * index as f32 / sample_rate as f32;
            tap.push(phase.sin());
        }
    }

    #[test]
    fn nothing_playing_yields_no_bands() {
        let spectrum = Spectrum::new();
        assert!(spectrum.bands(16).is_empty());
        assert_eq!(spectrum.peak(), 0.0);
    }

    #[test]
    fn a_tone_lands_in_the_right_band() {
        let spectrum = Spectrum::new();
        let mut tap = spectrum.attach(44100, 1);
        feed(&mut tap, 8000.0, 44100, WINDOW);

        let bands = spectrum.bands(8);
        assert_eq!(bands.len(), 8);
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index)
            .unwrap();
        // 8 kHz of a 30 Hz..18 kHz log split sits in the top third.
        assert!(loudest >= 5, "8 kHz landed in band {loudest} of 8");
    }

    #[test]
    fn a_full_ring_drops_samples_instead_of_blocking() {
        let spectrum = Spectrum::new();
        let mut tap = spectrum.attach(44100, 1);
        // Far more than the ring holds, with nothing draining it.
        feed(&mut tap, 440.0, 44100, RING * 4);
        // The point is that it returned at all.
        assert!(!spectrum.bands(8).is_empty());
    }

    #[test]
    fn only_the_first_channel_is_tapped() {
        let spectrum = Spectrum::new();
        let mut tap = spectrum.attach(44100, 2);
        // Interleaved: silence on the left, full scale on the right.
        for _ in 0..WINDOW * 2 {
            tap.push(0.0);
            tap.push(1.0);
        }
        assert_eq!(
            spectrum.peak(),
            0.0,
            "the right channel leaked into the tap"
        );
    }

    #[test]
    fn peak_tracks_the_signal() {
        let spectrum = Spectrum::new();
        let mut tap = spectrum.attach(44100, 1);
        for _ in 0..WINDOW {
            tap.push(0.5);
        }
        assert!((spectrum.peak() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn discard_empties_the_ring() {
        let spectrum = Spectrum::new();
        let mut tap = spectrum.attach(44100, 1);
        feed(&mut tap, 440.0, 44100, WINDOW);
        spectrum.discard();
        assert!(spectrum.bands(8).is_empty());
    }

    #[test]
    fn reattaching_forgets_the_previous_stream() {
        let spectrum = Spectrum::new();
        let mut tap = spectrum.attach(44100, 1);
        feed(&mut tap, 440.0, 44100, WINDOW);
        // A device change starts a fresh ring.
        let _new_tap = spectrum.attach(48000, 2);
        assert!(spectrum.bands(8).is_empty());
    }
}
