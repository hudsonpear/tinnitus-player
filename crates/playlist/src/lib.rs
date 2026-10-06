//! Playlists and the playback queue as plain data.
//!
//! No GPUI, no database, no audio. The queue is a list of track ids with an
//! ordering; the format module turns playlist files into paths and back. Both
//! are pure enough to be tested without a disk or a sound card.

pub mod formats;
pub mod queue;

pub use formats::{Entry, Format};
pub use queue::{Advance, Queue, Repeat, TrackId};
