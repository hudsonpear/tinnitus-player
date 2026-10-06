//! The audio engine: one thread, one command channel, one event channel.
//!
//! Nothing in here knows about GPUI, the database, or the queue. It is told
//! which file to play and which file comes next; deciding *what* comes next is
//! the player layer's job. That split is what lets audio keep running when the
//! renderer is in trouble.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use rodio::Source as _;

use crate::dsp::{CrossfadeSettings, EqHandle, EqSettings, clamp_speed, crossfade_gains};
use crate::output::{DECKS, MAX_GAIN, Output, Volume};
use crate::spectrum::Spectrum;

/// How often the engine wakes to check position, track boundaries and the output
/// device. 20 ms is well under a frame and costs nothing measurable.
const TICK: Duration = Duration::from_millis(20);

/// How often a position update is sent to the UI. More often than this just
/// makes the UI redraw for no visible gain.
const POSITION_INTERVAL: Duration = Duration::from_millis(200);

/// How close to the end of a track the next one is put on the deck.
///
/// Gapless means appending the next file behind the current one, and rodio has
/// no way to take a queued source back off again: dropping it means rebuilding
/// the deck, which restarts what the user is listening to. So the append is left
/// until the boundary is actually near. Anything the queue does before that —
/// shuffle, repeat, reordering — is then a change of a `pending` slot and never
/// touches playback. Twelve seconds is far more than a decoder needs to open a
/// file, and short enough that a queue change almost never lands inside it.
const GAPLESS_LEAD: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
pub enum Command {
    /// Start playing `path`, optionally from `at`, optionally paused.
    Load {
        path: PathBuf,
        at: Option<Duration>,
        /// Linear ReplayGain multiplier for this track.
        gain: f32,
        /// Start paused. Used when restoring state at startup.
        paused: bool,
    },
    /// Get the next track ready so the boundary is seamless.
    Preload {
        path: PathBuf,
        gain: f32,
    },
    /// Forget a preloaded track, e.g. because the queue changed.
    ClearPreload,
    Play,
    Pause,
    Stop,
    Seek(Duration),
    SetVolume(f32),
    SetSpeed(f32),
    SetEq(EqSettings),
    SetCrossfade(CrossfadeSettings),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEvent {
    Loading(PathBuf),
    Playing(Duration),
    Paused(Duration),
    Position(Duration),
    Length(Duration),
    /// The current track finished. `continued` is true when a preloaded track
    /// took over, so the player knows whether it still has to start one.
    Ended {
        continued: bool,
    },
    Stopped,
    /// This file could not be decoded. The player skips it and keeps going.
    Unavailable {
        path: PathBuf,
        error: String,
    },
    /// The output device changed or failed and was reopened.
    OutputChanged {
        device: String,
    },
    /// No audio device at all. Everything else still works.
    OutputLost {
        error: String,
    },
}

/// The handle the rest of the app holds. Every method is a channel send, so
/// nothing here can ever block the UI thread.
#[derive(Clone)]
pub struct Engine {
    commands: Sender<Command>,
    spectrum: Spectrum,
    eq: EqHandle,
}

impl Engine {
    /// Starts the engine thread. It opens the audio device itself, so this
    /// returns before any device work happens and cannot fail here.
    pub fn start(volume: f32, eq: EqSettings) -> (Self, Receiver<PlaybackEvent>) {
        let (commands, command_rx) = channel();
        let (events, event_rx) = channel();
        let spectrum = Spectrum::new();
        let eq = EqHandle::new(eq);

        let thread_spectrum = spectrum.clone();
        let thread_eq = eq.clone();
        let spawned = std::thread::Builder::new()
            .name("tinnitus-audio".to_owned())
            .spawn(move || run(volume, command_rx, events, thread_spectrum, thread_eq));
        if let Err(error) = spawned {
            log::error!("audio: cannot spawn the engine thread: {error}");
        }

        (
            Self {
                commands,
                spectrum,
                eq,
            },
            event_rx,
        )
    }

    fn send(&self, command: Command) {
        // A closed channel means the engine thread is gone; there is nothing
        // useful to do about it at a call site like `pause()`.
        if self.commands.send(command).is_err() {
            log::warn!("audio: the engine is not running");
        }
    }

    pub fn load(&self, path: impl Into<PathBuf>, gain: f32) {
        self.send(Command::Load {
            path: path.into(),
            at: None,
            gain,
            paused: false,
        });
    }

    pub fn load_at(&self, path: impl Into<PathBuf>, at: Duration, gain: f32, paused: bool) {
        self.send(Command::Load {
            path: path.into(),
            at: Some(at),
            gain,
            paused,
        });
    }

    pub fn preload(&self, path: impl Into<PathBuf>, gain: f32) {
        self.send(Command::Preload {
            path: path.into(),
            gain,
        });
    }

    pub fn clear_preload(&self) {
        self.send(Command::ClearPreload);
    }

    pub fn play(&self) {
        self.send(Command::Play);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn seek(&self, position: Duration) {
        self.send(Command::Seek(position));
    }

    /// The gain handed here is the slider multiplied by the user's ceiling, so
    /// it runs past unity whenever that ceiling has been raised. `MAX_GAIN` is
    /// the hard stop; the soft clip downstream keeps the result in range.
    pub fn set_volume(&self, gain: f32) {
        self.send(Command::SetVolume(gain.clamp(0.0, MAX_GAIN)));
    }

    /// Mute is just a volume of zero; the engine has no separate mute state, so
    /// there is nothing that can get out of step with the slider.
    pub fn set_mute(&self, muted: bool, volume: f32) {
        self.set_volume(if muted { 0.0 } else { volume });
    }

    pub fn set_speed(&self, factor: f32) {
        self.send(Command::SetSpeed(clamp_speed(factor)));
    }

    pub fn set_eq(&self, settings: EqSettings) {
        self.send(Command::SetEq(settings));
    }

    pub fn set_crossfade(&self, settings: CrossfadeSettings) {
        self.send(Command::SetCrossfade(settings));
    }

    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }

    pub fn spectrum(&self) -> &Spectrum {
        &self.spectrum
    }

    pub fn eq(&self) -> &EqHandle {
        &self.eq
    }
}

/// A track sitting on a deck.
#[derive(Debug, Clone)]
struct Slot {
    path: PathBuf,
    length: Option<Duration>,
    /// Linear ReplayGain multiplier.
    gain: f32,
}

/// A crossfade in progress.
struct Fade {
    from: usize,
    to: usize,
    started: Instant,
    span: Duration,
    from_gain: f32,
    to_gain: f32,
}

struct EngineState {
    output: Output,
    volume: Volume,
    spectrum: Spectrum,
    eq: EqHandle,

    active: usize,
    playing: bool,
    speed: f32,
    crossfade: CrossfadeSettings,

    current: Option<Slot>,
    /// Appended to the active deck for a gapless boundary.
    gapless: Option<Slot>,
    /// Held back to be started on the other deck when the crossfade begins.
    pending: Option<Slot>,
    fade: Option<Fade>,

    /// Previous `len()` of the active deck, for spotting a track boundary.
    previous_len: usize,
}

fn run(
    volume: f32,
    commands: Receiver<Command>,
    events: Sender<PlaybackEvent>,
    spectrum: Spectrum,
    eq: EqHandle,
) {
    let volume = Volume::new(volume);
    let output = match Output::open(volume.clone(), &spectrum, eq.clone()) {
        Ok(output) => output,
        Err(error) => {
            log::error!("audio: cannot open the output: {error:#}");
            events
                .send(PlaybackEvent::OutputLost {
                    error: format!("{error:#}"),
                })
                .ok();
            // Stay alive and drain commands: the UI must keep working, and a
            // device may appear later.
            drain_without_audio(&commands, &events, volume, &eq);
            return;
        }
    };

    let mut state = EngineState {
        output,
        volume,
        spectrum,
        eq,
        active: 0,
        playing: false,
        speed: 1.0,
        crossfade: CrossfadeSettings::default(),
        current: None,
        gapless: None,
        pending: None,
        fade: None,
        previous_len: 0,
    };

    let report_every = (POSITION_INTERVAL.as_millis() / TICK.as_millis()).max(1) as u32;
    let mut ticks = 0u32;

    loop {
        match commands.recv_timeout(TICK) {
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(command) => {
                if !state.handle(command, &events) {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        ticks += 1;
        let report = ticks >= report_every;
        if report {
            ticks = 0;
        }
        state.tick(report, &events);
    }

    for index in 0..DECKS {
        state.output.deck(index).stop();
    }
    log::info!("audio: engine stopped");
}

/// Keeps answering commands when there is no audio device, so the UI does not
/// wedge waiting on an engine that never replies.
fn drain_without_audio(
    commands: &Receiver<Command>,
    events: &Sender<PlaybackEvent>,
    volume: Volume,
    eq: &EqHandle,
) {
    while let Ok(command) = commands.recv() {
        match command {
            Command::Shutdown => break,
            Command::SetVolume(gain) => volume.set(gain),
            Command::SetEq(settings) => eq.set(settings),
            Command::Load { path, .. } => {
                events
                    .send(PlaybackEvent::Unavailable {
                        path,
                        error: "there is no audio output device".to_owned(),
                    })
                    .ok();
            }
            _ => {}
        }
    }
}

impl EngineState {
    /// Returns false to end the engine loop.
    fn handle(&mut self, command: Command, events: &Sender<PlaybackEvent>) -> bool {
        match command {
            Command::Shutdown => return false,
            Command::Load {
                path,
                at,
                gain,
                paused,
            } => self.load(path, at, gain, paused, events),
            Command::Preload { path, gain } => self.preload(path, gain),
            Command::ClearPreload => self.clear_preload(),
            Command::Play => self.play(events),
            Command::Pause => self.pause(events),
            Command::Stop => self.stop(events),
            Command::Seek(position) => self.seek(position, events),
            Command::SetVolume(gain) => self.output.set_volume(gain),
            Command::SetSpeed(factor) => {
                self.speed = clamp_speed(factor);
                for index in 0..DECKS {
                    self.output.deck(index).set_speed(self.speed);
                }
            }
            Command::SetEq(settings) => self.eq.set(settings),
            Command::SetCrossfade(settings) => {
                // Only a real change may touch the decks. Every settings change
                // pushes the whole playback section at once, so an equalizer
                // nudge arrives here too — and rebuilding the deck for it
                // restarts the track the user is listening to.
                if self.crossfade == settings {
                    return true;
                }
                self.crossfade = settings;
                // Switching modes invalidates whatever was staged for the old
                // one; the player will re-send a preload.
                self.clear_preload();
            }
        }
        true
    }

    fn load(
        &mut self,
        path: PathBuf,
        at: Option<Duration>,
        gain: f32,
        paused: bool,
        events: &Sender<PlaybackEvent>,
    ) {
        events.send(PlaybackEvent::Loading(path.clone())).ok();
        self.cancel_fade();
        self.pending = None;
        self.gapless = None;

        let deck = self.output.deck(self.active).clone();
        deck.clear();
        deck.set_speed(self.speed);
        deck.set_volume(gain.max(0.0));
        self.current = None;
        self.previous_len = 0;

        let length = match append(&deck, &path) {
            Ok(length) => length,
            Err(error) => {
                log::warn!("audio: cannot play {}: {error:#}", path.display());
                events
                    .send(PlaybackEvent::Unavailable {
                        path,
                        error: format!("{error:#}"),
                    })
                    .ok();
                return;
            }
        };

        if let Some(at) = at
            && let Err(error) = deck.try_seek(at)
        {
            log::warn!("audio: cannot start at {}s: {error}", at.as_secs());
        }
        match paused {
            true => deck.pause(),
            false => deck.play(),
        }

        self.previous_len = deck.len();
        self.playing = !paused;
        self.current = Some(Slot {
            path,
            length,
            gain: gain.max(0.0),
        });

        if let Some(length) = length {
            events.send(PlaybackEvent::Length(length)).ok();
        }
        let position = at.unwrap_or_default();
        events
            .send(match self.playing {
                true => PlaybackEvent::Playing(position),
                false => PlaybackEvent::Paused(position),
            })
            .ok();
    }

    /// Notes what comes next. Nothing reaches a deck here: a crossfade starts its
    /// own deck when the overlap begins, and a gapless boundary appends to this
    /// one once `GAPLESS_LEAD` from the end.
    fn preload(&mut self, path: PathBuf, gain: f32) {
        if self.current.is_none() {
            return;
        }
        self.pending = Some(Slot {
            path,
            length: None,
            gain: gain.max(0.0),
        });
    }

    /// Appends the next track behind the current one when the boundary is close.
    /// Called every tick.
    fn stage_gapless(&mut self) {
        if self.crossfade.duration().is_some() || self.gapless.is_some() {
            return;
        }
        let Some(slot) = self.pending.clone() else {
            return;
        };
        let Some(current) = self.current.clone() else {
            return;
        };
        if !gapless_due(self.position(), current.length, GAPLESS_LEAD) {
            return;
        }

        let deck = self.output.deck(self.active).clone();
        match append(&deck, &slot.path) {
            Ok(length) => {
                self.pending = None;
                self.previous_len = deck.len();
                self.gapless = Some(Slot { length, ..slot });
            }
            // A bad next track is not worth interrupting the current one for;
            // it surfaces when the player tries to make it current.
            Err(error) => {
                log::warn!("audio: cannot preload {}: {error:#}", slot.path.display());
                self.pending = None;
            }
        }
    }

    /// Forgets what comes next. Usually free: the next track is only on the deck
    /// during the last `GAPLESS_LEAD` seconds, and outside that window this is a
    /// `pending` slot being dropped.
    fn clear_preload(&mut self) {
        self.pending = None;
        if self.gapless.take().is_none() {
            return;
        }
        // rodio has no "drop the queued source" operation, so the deck is rebuilt
        // with only the current track on it, at the position it had reached.
        let Some(current) = self.current.clone() else {
            return;
        };
        let deck = self.output.deck(self.active).clone();
        let position = deck.get_pos();
        deck.clear();
        if append(&deck, &current.path).is_ok() {
            deck.try_seek(position).ok();
            deck.set_volume(current.gain);
            deck.set_speed(self.speed);
            if self.playing {
                deck.play();
            }
        }
        self.previous_len = deck.len();
    }

    fn play(&mut self, events: &Sender<PlaybackEvent>) {
        if self.current.is_none() {
            return;
        }
        self.playing = true;
        self.output.deck(self.active).play();
        if let Some(fade) = &self.fade {
            self.output.deck(fade.from).play();
        }
        events.send(PlaybackEvent::Playing(self.position())).ok();
    }

    fn pause(&mut self, events: &Sender<PlaybackEvent>) {
        self.playing = false;
        let position = self.position();
        for index in 0..DECKS {
            self.output.deck(index).pause();
        }
        events.send(PlaybackEvent::Paused(position)).ok();
    }

    fn stop(&mut self, events: &Sender<PlaybackEvent>) {
        self.cancel_fade();
        self.playing = false;
        self.current = None;
        self.gapless = None;
        self.pending = None;
        self.previous_len = 0;
        for index in 0..DECKS {
            self.output.deck(index).clear();
        }
        events.send(PlaybackEvent::Stopped).ok();
    }

    fn seek(&mut self, position: Duration, events: &Sender<PlaybackEvent>) {
        if self.current.is_none() {
            return;
        }
        // Seeking invalidates a crossfade that is already under way.
        self.cancel_fade();
        let deck = self.output.deck(self.active);
        if let Err(error) = deck.try_seek(position) {
            log::warn!("audio: cannot seek: {error}");
        }
        events.send(PlaybackEvent::Position(deck.get_pos())).ok();
    }

    fn position(&self) -> Duration {
        self.output.deck(self.active).get_pos()
    }

    fn tick(&mut self, report: bool, events: &Sender<PlaybackEvent>) {
        if self.check_output(events) {
            return;
        }

        self.step_fade();
        self.maybe_start_fade(events);
        self.stage_gapless();

        let deck = self.output.deck(self.active).clone();
        let len = deck.len();

        // A shrinking queue on the deck means the source that was playing ran
        // out and the one appended behind it took over: the gapless boundary.
        if self.current.is_some() && self.playing && len < self.previous_len {
            let continued = self.gapless.is_some();
            events.send(PlaybackEvent::Ended { continued }).ok();

            self.current = self.gapless.take();
            // Anything still waiting to be staged was chosen as the successor to
            // the track that just ended. The player re-primes the moment it sees
            // this event, so the right one arrives in a few milliseconds.
            self.pending = None;
            self.playing = self.current.is_some();
            match &self.current {
                Some(slot) => {
                    deck.set_volume(slot.gain);
                    if let Some(length) = slot.length {
                        events.send(PlaybackEvent::Length(length)).ok();
                    }
                    events.send(PlaybackEvent::Position(deck.get_pos())).ok();
                }
                None => {
                    events.send(PlaybackEvent::Stopped).ok();
                }
            }
        } else if self.playing && report {
            events.send(PlaybackEvent::Position(deck.get_pos())).ok();
        }

        self.previous_len = len;
    }

    /// Reopens the output when the device failed or the system default moved.
    /// Returns true when the caller should skip the rest of this tick.
    fn check_output(&mut self, events: &Sender<PlaybackEvent>) -> bool {
        if !self.output.failed() && !self.output.changed() {
            return false;
        }

        let resume = self.current.clone();
        let position = self.position();
        let was_playing = self.playing;
        log::info!("audio: output changed, reopening");

        match Output::open(self.volume.clone(), &self.spectrum, self.eq.clone()) {
            Ok(output) => {
                self.output = output;
                self.active = 0;
                self.fade = None;
                self.gapless = None;
                self.previous_len = 0;
                events
                    .send(PlaybackEvent::OutputChanged {
                        device: self.output.device_name().to_owned(),
                    })
                    .ok();

                if let Some(slot) = resume {
                    self.load(slot.path, Some(position), slot.gain, !was_playing, events);
                }
            }
            Err(error) => {
                log::error!("audio: cannot reopen the output: {error:#}");
                events
                    .send(PlaybackEvent::OutputLost {
                        error: format!("{error:#}"),
                    })
                    .ok();
                self.playing = false;
            }
        }
        true
    }

    /// Starts the overlap when the current track is close enough to its end.
    fn maybe_start_fade(&mut self, events: &Sender<PlaybackEvent>) {
        if !self.playing || self.fade.is_some() {
            return;
        }
        let Some(span) = self.crossfade.duration() else {
            return;
        };
        let Some(next) = self.pending.clone() else {
            return;
        };
        let Some(length) = self.current.as_ref().and_then(|slot| slot.length) else {
            return;
        };
        if !should_start_fade(self.position(), length, span) {
            return;
        }

        let from = self.active;
        let to = (self.active + 1) % DECKS;
        let incoming = self.output.deck(to).clone();
        incoming.clear();
        incoming.set_speed(self.speed);
        // Starts silent and is faded up by `step_fade`.
        incoming.set_volume(0.0);

        let length = match append(&incoming, &next.path) {
            Ok(length) => length,
            Err(error) => {
                log::warn!(
                    "audio: cannot crossfade into {}: {error:#}",
                    next.path.display()
                );
                self.pending = None;
                return;
            }
        };
        incoming.play();

        self.pending = None;
        self.fade = Some(Fade {
            from,
            to,
            started: Instant::now(),
            span,
            from_gain: self.current.as_ref().map(|slot| slot.gain).unwrap_or(1.0),
            to_gain: next.gain,
        });

        // The incoming track is what the user is now listening to, so the UI
        // switches to it as the overlap begins.
        self.active = to;
        self.previous_len = incoming.len();
        self.current = Some(Slot { length, ..next });

        events.send(PlaybackEvent::Ended { continued: true }).ok();
        if let Some(length) = length {
            events.send(PlaybackEvent::Length(length)).ok();
        }
        events.send(PlaybackEvent::Playing(Duration::ZERO)).ok();
    }

    fn step_fade(&mut self) {
        let Some(fade) = &self.fade else { return };
        let progress =
            fade.started.elapsed().as_secs_f32() / fade.span.as_secs_f32().max(f32::EPSILON);
        let (out, incoming) = crossfade_gains(progress);

        self.output.deck(fade.from).set_volume(out * fade.from_gain);
        self.output
            .deck(fade.to)
            .set_volume(incoming * fade.to_gain);

        if progress >= 1.0 {
            self.output.deck(fade.from).clear();
            self.fade = None;
        }
    }

    fn cancel_fade(&mut self) {
        let Some(fade) = self.fade.take() else { return };
        self.output.deck(fade.from).clear();
        self.output.deck(fade.to).set_volume(fade.to_gain);
    }
}

/// True once the track is within one crossfade span of its end.
///
/// Split out because it is the one piece of the crossfade that can be tested
/// without a sound card.
fn should_start_fade(position: Duration, length: Duration, span: Duration) -> bool {
    length
        .checked_sub(position)
        .is_none_or(|remaining| remaining <= span)
}

/// True once the next track should go on the deck behind this one.
///
/// A track of unknown length has no boundary to aim at, so it is staged at once
/// rather than never — a missing gapless join is worse than an early one.
fn gapless_due(position: Duration, length: Option<Duration>, lead: Duration) -> bool {
    match length {
        Some(length) => position + lead >= length,
        None => true,
    }
}

/// Decodes a file and appends it to a deck, returning its length when known.
fn append(deck: &rodio::Player, path: &Path) -> Result<Option<Duration>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let bytes = file.metadata().ok().map(|meta| meta.len());
    let reader = std::io::BufReader::new(file);

    let mut builder = rodio::Decoder::builder()
        .with_data(reader)
        .with_seekable(true);
    if let Some(bytes) = bytes {
        builder = builder.with_byte_len(bytes);
    }
    let source = builder
        .build()
        .with_context(|| format!("cannot decode {}", path.display()))?;

    let length = source.total_duration();
    deck.append(source);
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_starts_only_inside_the_overlap() {
        let length = Duration::from_secs(180);
        let span = Duration::from_secs(5);

        assert!(!should_start_fade(Duration::from_secs(0), length, span));
        assert!(!should_start_fade(Duration::from_secs(174), length, span));
        assert!(should_start_fade(Duration::from_secs(175), length, span));
        assert!(should_start_fade(Duration::from_secs(179), length, span));
        // Past the end is still "inside", not a panic on the subtraction.
        assert!(should_start_fade(Duration::from_secs(200), length, span));
    }

    #[test]
    fn the_next_track_goes_on_the_deck_only_near_the_boundary() {
        let length = Some(Duration::from_secs(180));

        // Mid-track there is nothing on the deck but the track itself, so a
        // shuffle or repeat change costs nothing and cannot restart playback.
        assert!(!gapless_due(Duration::ZERO, length, GAPLESS_LEAD));
        assert!(!gapless_due(Duration::from_secs(167), length, GAPLESS_LEAD));
        assert!(gapless_due(Duration::from_secs(168), length, GAPLESS_LEAD));
        assert!(gapless_due(Duration::from_secs(180), length, GAPLESS_LEAD));
    }

    #[test]
    fn a_track_of_unknown_length_is_staged_at_once() {
        // Better an early append than a gap at every boundary of a stream whose
        // length the decoder would not tell us.
        assert!(gapless_due(Duration::ZERO, None, GAPLESS_LEAD));
    }

    #[test]
    fn a_track_shorter_than_the_lead_stages_immediately() {
        let length = Some(Duration::from_secs(4));
        assert!(gapless_due(Duration::ZERO, length, GAPLESS_LEAD));
    }

    #[test]
    fn a_track_shorter_than_the_fade_fades_from_the_start() {
        let length = Duration::from_secs(3);
        let span = Duration::from_secs(5);
        assert!(should_start_fade(Duration::ZERO, length, span));
    }

    #[test]
    fn a_missing_file_reports_itself_instead_of_stopping_the_engine() {
        // No device needed: opening a file that is not there fails before rodio
        // touches any hardware.
        let (player, _source) = rodio::Player::new();
        let error = append(&player, Path::new("no-such-file.flac")).unwrap_err();
        assert!(format!("{error:#}").contains("cannot open"));
    }

    #[test]
    fn garbage_is_rejected_by_the_decoder() {
        let dir = std::env::temp_dir().join("tinnitus-engine-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("garbage.mp3");
        std::fs::write(&path, b"this is not audio at all").unwrap();

        let (player, _source) = rodio::Player::new();
        assert!(append(&player, &path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
