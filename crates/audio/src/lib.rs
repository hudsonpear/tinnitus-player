//! The audio engine.
//!
//! Deliberately free of every UI dependency: this crate compiles and runs with
//! no window, no renderer and no GPU. If drawing breaks, playback does not.

pub mod dsp;
pub mod engine;
pub mod output;
pub mod peaks;
pub mod spectrum;

pub use dsp::{
    CrossfadeSettings, EqHandle, EqSettings, Preset, ReplayGainMode, ReplayGainSettings, SPEEDS,
    TrackGain, clamp_speed,
};
pub use engine::{Command, Engine, PlaybackEvent};
pub use spectrum::Spectrum;
