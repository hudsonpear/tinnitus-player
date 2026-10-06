//! The shape of a whole file: what a waveform seek bar is drawn from.
//!
//! Unrelated to `spectrum`, which watches what is coming out of the speakers
//! right now. This reads the file from start to end once and hands back a value
//! per slice for a view to draw.
//!
//! The value is loudness (RMS), not the loudest sample. Modern masters are
//! compressed hard, so the loudest sample in any given slice is near the
//! loudest sample in the track almost everywhere: a peak-per-slice waveform
//! comes out as a fat even sausage with no detail in it. Loudness varies the
//! way the music does, which is what makes the shape recognisable.

use std::path::Path;

use anyhow::{Context as _, Result};
use rodio::Source as _;

/// How much audio each value stands for. Twenty milliseconds is about twelve
/// thousand values for an average song — still small to keep, and fine enough
/// that a drum hit survives being squeezed into a seek bar.
const SLICE_MS: u32 = 20;

/// Reads a file and returns the loudness of each slice, each `0.0..=1.0`.
///
/// Decoding the whole file takes a moment, so this belongs on a background
/// thread; it holds only the slice values, never the samples.
pub fn scan(path: &Path) -> Result<Vec<f32>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let bytes = file.metadata().ok().map(|meta| meta.len());
    let reader = std::io::BufReader::new(file);

    let mut builder = rodio::Decoder::builder().with_data(reader);
    if let Some(bytes) = bytes {
        builder = builder.with_byte_len(bytes);
    }
    let source = builder
        .build()
        .with_context(|| format!("cannot decode {}", path.display()))?;

    let rate = source.sample_rate().get();
    let channels = u32::from(source.channels().get());
    // Both are non-zero by construction, but the arithmetic below divides by
    // them, and a decoder is not something to take on trust.
    let per_slice = (rate.max(1) / (1000 / SLICE_MS)).max(1) * channels.max(1);

    let mut peaks = Vec::new();
    let mut energy = 0.0f64;
    let mut seen = 0u32;
    let flush = |energy: f64, seen: u32| match seen {
        0 => 0.0,
        _ => ((energy / f64::from(seen)).sqrt() as f32).min(1.0),
    };
    for sample in source {
        energy += f64::from(sample) * f64::from(sample);
        seen += 1;
        if seen >= per_slice {
            peaks.push(flush(energy, seen));
            energy = 0.0;
            seen = 0;
        }
    }
    if seen > 0 {
        peaks.push(flush(energy, seen));
    }
    Ok(peaks)
}

/// Squeezes the slices into `bars` values and scales them to fill the height.
///
/// Averaging rather than taking the loudest: these are loudnesses, and the
/// loudest of a group is the one that throws the shape away. The result is then
/// normalised against its own peak, so a quiet recording draws as big as a loud
/// one — the seek bar is showing where the song is busy, not how hot it was
/// mastered. Fewer slices than bars are stretched instead, so a very short file
/// still fills the bar.
pub fn resample(peaks: &[f32], bars: usize) -> Vec<f32> {
    if bars == 0 {
        return vec![];
    }
    if peaks.is_empty() {
        return vec![0.0; bars];
    }

    let mut scaled: Vec<f32> = (0..bars)
        .map(|bar| {
            let start = (bar * peaks.len() / bars).min(peaks.len() - 1);
            let end = ((bar + 1) * peaks.len() / bars).clamp(start + 1, peaks.len());
            let group = &peaks[start..end];
            group.iter().sum::<f32>() / group.len() as f32
        })
        .collect();

    let loudest = scaled.iter().copied().fold(0.0f32, f32::max);
    if loudest > 0.0 {
        for value in &mut scaled {
            *value = contrast(*value / loudest);
        }
    }
    scaled
}

/// Pushes the quiet parts down and the loud parts up.
///
/// Straight loudness, drawn as it comes, is a flat-topped slab: everything in a
/// modern master sits between half and full. Pulling the ends apart is what
/// makes a verse look like a verse and a chorus like a chorus. Anything that
/// compresses instead — a square root, say — buries the difference completely.
///
/// The result runs past 1.0 on purpose, up to `CREST`. Clamping it back is the
/// same flat top by another route: with a loud master three quarters of the
/// song lands on the ceiling and the shape is gone. The drawing leaves room for
/// the overshoot instead.
fn contrast(value: f32) -> f32 {
    match value {
        quiet if quiet < 0.3 => quiet * 0.8,
        middle if middle < 0.7 => middle * 1.1,
        loud => loud * 1.3,
    }
}

/// The tallest a shaped value can be — what `contrast` does to a peak of 1.0.
/// Whatever draws these has to fit this, not 1.0, inside its height.
pub const CREST: f32 = 1.3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_louder_stretch_draws_taller_than_a_quiet_one() {
        // Quiet first half, loud second: the shape has to show that, and the
        // loudest part has to reach the top of the bar.
        let mut peaks = vec![0.05; 100];
        peaks[50..].fill(0.4);
        let bars = resample(&peaks, 10);

        assert_eq!(bars.len(), 10);
        assert_eq!(bars[9], CREST, "the loudest stretch reaches full height");
        assert!(
            bars[0] < bars[9] * 0.5,
            "and the quiet one stays well under"
        );
        assert!(bars[0] > 0.0, "a quiet passage is still part of the song");
    }

    #[test]
    fn a_quiet_recording_draws_as_big_as_a_loud_one() {
        // Same shape, thirty times quieter. The seek bar shows where the song
        // is busy, not how hot it was mastered.
        let loud = resample(&[0.9, 0.3, 0.6, 0.9], 4);
        let quiet = resample(&[0.03, 0.01, 0.02, 0.03], 4);
        for (a, b) in loud.iter().zip(quiet.iter()) {
            assert!((a - b).abs() < 0.001, "{a} and {b} should draw alike");
        }
    }

    #[test]
    fn a_short_file_still_fills_the_bar() {
        let bars = resample(&[0.4, 0.8], 6);
        assert_eq!(bars.len(), 6);
        assert!(bars.iter().all(|value| *value > 0.0));
    }

    /// Prints the shape of a real file, for eyeballing what the seek bar will
    /// draw. Ignored by default: it needs a file, and which file is the
    /// runner's business.
    ///
    /// `TINNITUS_WAVE_FILE="D:\\song.mp3" cargo test -p audio --lib shape_of -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn shape_of_a_real_file() {
        let Some(path) = std::env::var_os("TINNITUS_WAVE_FILE") else {
            panic!("set TINNITUS_WAVE_FILE to the file to scan");
        };
        let slices = scan(Path::new(&path)).expect("cannot scan");
        let bars = resample(&slices, 96);

        let mut sorted = bars.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        println!(
            "{} slices, {:.1}s — low {:.2}, quarter {:.2}, middle {:.2}, high {:.2}",
            slices.len(),
            slices.len() as f32 * 0.02,
            sorted[0],
            sorted[sorted.len() / 4],
            sorted[sorted.len() / 2],
            sorted[sorted.len() - 1],
        );
        for row in (0..16).rev() {
            let line: String = bars
                .iter()
                .map(|value| match (value / CREST * 16.0) as usize >= row {
                    true => '#',
                    false => ' ',
                })
                .collect();
            println!("|{line}|");
        }
    }

    #[test]
    fn nothing_to_draw_is_a_flat_line_rather_than_a_panic() {
        assert_eq!(resample(&[], 4), vec![0.0; 4]);
        assert!(resample(&[0.5], 0).is_empty());
    }
}
