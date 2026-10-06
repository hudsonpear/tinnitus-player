//! The player: the queue, the transport, and the bridge to the audio engine.
//!
//! This is the "Player API" layer from the design. The UI talks to this; this
//! talks to `audio::Engine`. The engine knows nothing about queues, and the UI
//! knows nothing about decks.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use audio::dsp::{CrossfadeSettings, EqSettings, ReplayGainSettings, TrackGain, replaygain};
use audio::{Engine, PlaybackEvent};
use gpui::{Context, Entity, EventEmitter, Task};
use library::models::{Listen, Track, TrackId, now_ms};
use library::{Db, db::queries};
use playlist::{Advance, Queue, Repeat};

use crate::settings::{Session, Settings};

/// How often playback events are drained while something is playing. The engine
/// reports position five times a second, so this only has to be finer than that.
const POLL_PLAYING: Duration = Duration::from_millis(30);
/// Idle polling. Nothing is arriving, so there is no reason to spin.
const POLL_IDLE: Duration = Duration::from_millis(250);
/// How long a per-song volume or curve waits before it is written. Long enough
/// that a slider drag is one write rather than a hundred.
const SAVE_AUDIO_AFTER: Duration = Duration::from_millis(600);

/// Anything the rest of the app might want to react to.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// The current track changed (or there is no longer one).
    TrackChanged(Option<TrackId>),
    /// Something the user should see: a file that would not decode, a lost
    /// audio device.
    Trouble(String),
}

pub struct Player {
    engine: Engine,
    db: Arc<Mutex<Db>>,

    queue: Queue,
    /// Rows for the part of the queue the user has actually looked at, by track
    /// id. The queue itself holds ids so that queueing a whole library stays
    /// cheap; the panel still has to show titles, and this is what it reads.
    queue_rows: std::collections::HashMap<TrackId, Track>,
    current: Option<Track>,
    /// Seconds.
    position: f64,
    length: f64,
    playing: bool,
    /// While the user drags the seek bar the UI shows their finger, not the
    /// engine's position, which is still reporting the old spot.
    scrubbing: Option<f64>,

    volume: f32,
    /// What a full slider is worth. Kept beside the volume rather than read
    /// from the settings on every change, because the engine is told the
    /// product of the two and nothing else ever sees it.
    max_volume: f32,
    muted: bool,
    speed: f32,
    replay_gain: ReplayGainSettings,

    /// Keep a volume and a curve per song. Off by default, and off means this
    /// whole mechanism costs one boolean check.
    remember_audio: bool,
    /// The curve actually in force. With per-song audio on this is the current
    /// song's, which is not the one in the settings file — and the equalizer
    /// screen has to draw what is being heard.
    eq: EqSettings,
    /// The volume and curve from the settings file — what a song with nothing
    /// remembered plays at. Without these, one song's setting would leak into
    /// every song after it.
    global_volume: f32,
    global_eq: EqSettings,
    /// A song's settings waiting to be written, and the song they belong to.
    /// Dragging a slider fires sixty times a second; this is what stops that
    /// being sixty writes.
    pending_audio: Option<(TrackId, library::models::TrackAudio)>,
    _save_audio: Option<Task<()>>,

    /// The file name of something playing that has no library row behind it.
    loose: Option<String>,

    /// Milliseconds of the current track actually listened to, for the history.
    listened_ms: f64,
    trouble: Option<String>,

    /// Peaks for the file playing now, and which file they belong to. Empty
    /// until the scan finishes, which is what the seek bar falls back on.
    waveform: Vec<f32>,
    waveform_for: Option<PathBuf>,
    /// Only scanned when something is actually drawing it: reading a whole file
    /// to draw a bar nobody asked for is work for nothing.
    wants_waveform: bool,
    _waveform: Option<Task<()>>,

    _events: Task<()>,
}

impl EventEmitter<PlayerEvent> for Player {}

impl Player {
    pub fn new(db: Arc<Mutex<Db>>, settings: &Settings, cx: &mut Context<Self>) -> Self {
        let (engine, events) = Engine::start(
            match settings.muted {
                true => 0.0,
                false => crate::settings::effective_gain(settings.volume, settings.max_volume),
            },
            settings.equalizer.clone(),
        );
        engine.set_speed(settings.speed);
        engine.set_crossfade(settings.crossfade);

        let mut queue = Queue::new();
        queue.set_shuffle(settings.shuffle);
        queue.set_repeat(settings.repeat);

        Self {
            _events: Self::pump(events, cx),
            engine,
            db,
            queue,
            queue_rows: Default::default(),
            current: None,
            position: 0.0,
            length: 0.0,
            playing: false,
            scrubbing: None,
            volume: settings.volume,
            max_volume: settings.max_volume,
            muted: settings.muted,
            speed: settings.speed,
            replay_gain: settings.replay_gain,
            remember_audio: settings.remember_per_track_audio,
            eq: settings.equalizer.clone(),
            global_volume: settings.volume,
            global_eq: settings.equalizer.clone(),
            pending_audio: None,
            _save_audio: None,
            loose: None,
            listened_ms: 0.0,
            trouble: None,
            waveform: vec![],
            waveform_for: None,
            wants_waveform: settings.timeline == crate::settings::Timeline::Waveform,
            _waveform: None,
        }
    }

    /// Drains the engine's event channel onto the UI thread.
    ///
    /// The channel is a plain `std::sync::mpsc`, so this polls rather than
    /// awaits. Polling backs off to four times a second when nothing is playing.
    fn pump(events: Receiver<PlaybackEvent>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let mut drained = Vec::new();
                while let Ok(event) = events.try_recv() {
                    drained.push(event);
                }

                let playing = this
                    .update(cx, |player, cx| {
                        for event in drained {
                            player.apply(event, cx);
                        }
                        player.playing
                    })
                    .unwrap_or(false);

                let wait = match playing {
                    true => POLL_PLAYING,
                    false => POLL_IDLE,
                };
                cx.background_executor().timer(wait).await;
            }
        })
    }

    fn apply(&mut self, event: PlaybackEvent, cx: &mut Context<Self>) {
        match event {
            PlaybackEvent::Loading(_) => {
                self.position = 0.0;
                self.listened_ms = 0.0;
            }
            PlaybackEvent::Playing(position) => {
                self.playing = true;
                self.advance_listened(position.as_secs_f64());
            }
            PlaybackEvent::Paused(position) => {
                self.playing = false;
                self.advance_listened(position.as_secs_f64());
            }
            PlaybackEvent::Position(position) => self.advance_listened(position.as_secs_f64()),
            PlaybackEvent::Length(length) => self.set_length(length.as_secs_f64()),
            PlaybackEvent::Ended { continued } => self.finished(continued, cx),
            PlaybackEvent::Stopped => {
                self.playing = false;
                self.position = 0.0;
            }
            PlaybackEvent::Unavailable { path, error } => {
                self.trouble = Some(format!("{}: {error}", name_of(&path)));
                cx.emit(PlayerEvent::Trouble(
                    self.trouble.clone().unwrap_or_default(),
                ));
                // A broken file must not end the listening session.
                self.skip(Advance::Ended, cx);
            }
            PlaybackEvent::OutputChanged { device } => {
                log::info!("player: now playing through {device}");
            }
            PlaybackEvent::OutputLost { error } => {
                self.playing = false;
                self.trouble = Some(format!("No audio output: {error}"));
                cx.emit(PlayerEvent::Trouble(
                    self.trouble.clone().unwrap_or_default(),
                ));
            }
        }
        cx.notify();
    }

    /// Takes the engine's idea of how long the track is, if we have no better.
    ///
    /// The decoder estimates a length from the bitrate, which is wrong for a VBR
    /// file — one the scanner read as 4:58 came back as 1:27, so the seek bar ran
    /// out halfway through the song and the clock counted past the end. The
    /// scanner's figure comes from the file's own headers, so it wins whenever
    /// there is one; a loose file with no library row still needs the estimate.
    fn set_length(&mut self, reported: f64) {
        let known = self
            .current
            .as_ref()
            .map(|track| track.duration)
            .unwrap_or(0.0);
        self.length = best_length(known, reported);
    }

    /// Position updates are also how we measure how much of a track was heard.
    fn advance_listened(&mut self, position: f64) {
        let moved = (position - self.position).abs();
        // A jump is a seek, not listening.
        if moved < 2.0 {
            self.listened_ms += moved * 1000.0;
        }
        self.position = position;
    }

    /// The engine finished a track. `continued` means it already started the one
    /// we had preloaded.
    fn finished(&mut self, continued: bool, cx: &mut Context<Self>) {
        self.record_listen();

        let next = self.queue.advance(Advance::Ended);
        match (next, continued) {
            (Some(id), true) => {
                // The engine is already playing it; just catch the UI up. It
                // never goes through `start`, so this is the only place its play
                // can be stamped.
                let at = self.stamp_played(id);
                self.current = self.lookup(id).map(|mut track| {
                    track.last_played = Some(at);
                    track
                });
                // Its length comes from the row, the same as `start` does it —
                // the engine's own figure is only trusted for files with no row,
                // so nothing else would move the clock off the last track's.
                self.length = self.current.as_ref().map(|t| t.duration).unwrap_or(0.0);
                self.listened_ms = 0.0;
                self.position = 0.0;
                // This path never goes through `start`, so it is the other
                // place a song's own volume and curve have to be put in force —
                // miss it and gapless playback is the one way to leak the last
                // song's settings into the next.
                self.apply_track_audio(id);
                cx.emit(PlayerEvent::TrackChanged(Some(id)));
                self.prime_next();
            }
            (Some(id), false) => self.start(id, None, false, cx),
            (None, _) => {
                self.playing = false;
                self.current = None;
                self.position = 0.0;
                cx.emit(PlayerEvent::TrackChanged(None));
            }
        }
    }

    // -- transport --------------------------------------------------------

    /// Replaces the queue and starts playing at `index`.
    pub fn play_all(&mut self, tracks: Vec<TrackId>, index: usize, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        self.record_listen();
        self.queue.fill(tracks, index);
        match self.queue.current() {
            Some(id) => self.start(id, None, false, cx),
            None => self.stop(cx),
        }
    }

    /// "Shuffle All": replaces the queue, shuffles it, and plays whatever came
    /// out on top rather than the first track of the list.
    pub fn shuffle_all(&mut self, tracks: Vec<TrackId>, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        self.record_listen();
        self.queue.fill_shuffled(tracks);
        match self.queue.current() {
            Some(id) => self.start(id, None, false, cx),
            None => self.stop(cx),
        }
    }

    pub fn play_now(&mut self, track: TrackId, cx: &mut Context<Self>) {
        self.play_all(vec![track], 0, cx);
    }

    /// Plays a file handed to us from outside the library — dropped on the
    /// window, opened from the file dialog, or named on the command line.
    ///
    /// A file the library already knows plays as itself, with its history and
    /// rating. One it does not know still plays; it simply has no row behind it,
    /// so it shows its file name and earns no play count. Refusing to open a
    /// file because it has not been indexed would be worse than either.
    pub fn play_loose_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(track) = self.lookup_path(&path) {
            self.play_all(vec![track.id], 0, cx);
            return;
        }

        self.record_listen();
        self.queue.clear();
        self.current = None;
        // No row, so nothing to remember against: a loose file plays at the
        // global settings, and the last song's volume does not follow it in.
        if self.remember_audio {
            self.restore_global_audio();
        }
        self.loose = Some(name_of(&path));
        self.length = 0.0;
        self.position = 0.0;
        self.listened_ms = 0.0;
        self.playing = true;
        self.engine.load(path.clone(), 1.0);
        if self.wants_waveform {
            self.scan_waveform(path, cx);
        }
        cx.emit(PlayerEvent::TrackChanged(None));
        cx.notify();
    }

    /// Opens an M3U, M3U8, PLS or XSPF file: the whole list goes into the queue
    /// and the first entry starts playing.
    ///
    /// ponytail: the file is read on the UI thread. A playlist is a few hundred
    /// lines of text and this happens once, on a drop or an "Open with" — if
    /// someone ever hands us a hundred-thousand-line list, move it onto the
    /// background executor the way `Library::import_playlist` already does.
    pub fn play_playlist_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let entries = match playlist::formats::read(&path) {
            Ok(entries) => entries,
            Err(error) => {
                cx.emit(PlayerEvent::Trouble(format!(
                    "cannot read {}: {error:#}",
                    path.display()
                )));
                return;
            }
        };
        if entries.is_empty() {
            cx.emit(PlayerEvent::Trouble(format!(
                "{} has no playable entries",
                path.display()
            )));
            return;
        }
        self.play_paths(entries.into_iter().map(|entry| entry.path).collect(), cx);
    }

    /// Plays a list of files from outside the library: a playlist's entries, or
    /// a multiple selection from the file dialog.
    ///
    /// The queue holds track ids, so a file with no row cannot go in it. Those
    /// are counted and reported rather than dropped in silence, and a list where
    /// nothing at all is known still plays its first file — refusing to open
    /// anything because it has not been indexed would be worse.
    pub fn play_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut ids = Vec::with_capacity(paths.len());
        let mut unknown = 0usize;
        for path in &paths {
            match self.lookup_path(path) {
                Some(track) => ids.push(track.id),
                None => unknown += 1,
            }
        }

        if ids.is_empty() {
            let Some(first) = paths.into_iter().next() else {
                return;
            };
            self.play_loose_file(first, cx);
            if unknown > 1 {
                cx.emit(PlayerEvent::Trouble(format!(
                    "None of those {unknown} files are in the library, so only the first is \
                     playing. Add their folder to queue the rest."
                )));
            }
            return;
        }

        self.play_all(ids, 0, cx);
        if unknown > 0 {
            cx.emit(PlayerEvent::Trouble(format!(
                "{unknown} of those files are not in the library and were left out of the queue."
            )));
        }
    }

    pub fn play_next(&mut self, tracks: &[TrackId], cx: &mut Context<Self>) {
        let was_empty = self.queue.is_empty();
        self.queue.play_next(tracks);
        self.prime_next();
        if was_empty && let Some(id) = self.queue.current() {
            self.start(id, None, false, cx);
        }
        cx.notify();
    }

    pub fn enqueue(&mut self, tracks: &[TrackId], cx: &mut Context<Self>) {
        let was_empty = self.queue.is_empty();
        self.queue.append(tracks);
        self.prime_next();
        if was_empty && let Some(id) = self.queue.current() {
            self.start(id, None, false, cx);
        }
        cx.notify();
    }

    /// How many tracks "Play Similar" adds to the queue.
    pub const SIMILAR: usize = 25;

    /// Appends tracks that resemble `seed`, skipping anything already queued.
    ///
    /// The over-fetch is the point: the queue is in memory, so the database
    /// cannot be asked to exclude it without binding every queued id. Four times
    /// the wanted count leaves room for a queue that already holds most of what
    /// would have been suggested.
    pub fn play_similar(&mut self, seed: TrackId, cx: &mut Context<Self>) {
        let found = {
            let Ok(db) = self.db.lock() else { return };
            queries::similar_to(db.conn(), seed, Self::SIMILAR * 4)
        };
        let candidates = match found {
            Ok(candidates) => candidates,
            Err(error) => {
                log::warn!("player: cannot find tracks like {seed}: {error:#}");
                cx.emit(PlayerEvent::Trouble(format!(
                    "Cannot look for similar tracks: {error}"
                )));
                return;
            }
        };

        let queued: std::collections::HashSet<TrackId> =
            self.queue.in_play_order().into_iter().collect();
        let picks: Vec<TrackId> = candidates
            .into_iter()
            .filter(|id| !queued.contains(id))
            .take(Self::SIMILAR)
            .collect();

        if picks.is_empty() {
            cx.emit(PlayerEvent::Trouble(
                "Nothing else in the library is like this track, or it is all queued already"
                    .to_owned(),
            ));
            return;
        }
        self.enqueue(&picks, cx);
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        match (self.playing, self.current.is_some()) {
            (true, _) => self.engine.pause(),
            (false, true) => self.engine.play(),
            // Pressing play with nothing loaded starts the queue if there is one.
            (false, false) => {
                if let Some(id) = self.queue.current() {
                    self.start(id, None, false, cx);
                }
            }
        }
    }

    pub fn pause(&mut self) {
        self.engine.pause();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.record_listen();
        self.engine.stop();
        self.playing = false;
        self.current = None;
        self.loose = None;
        self.position = 0.0;
        self.scrubbing = None;
        self.length = 0.0;
        cx.emit(PlayerEvent::TrackChanged(None));
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        self.skip(Advance::User, cx);
    }

    /// Restarts the track if we are more than a few seconds in — the same rule
    /// every physical CD player has used for forty years.
    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if self.position > 3.0 {
            self.seek(0.0);
            return;
        }
        self.record_listen();
        match self.queue.previous() {
            Some(id) => self.start(id, None, false, cx),
            None => self.stop(cx),
        }
    }

    fn skip(&mut self, reason: Advance, cx: &mut Context<Self>) {
        self.record_listen();
        match self.queue.advance(reason) {
            Some(id) => self.start(id, None, false, cx),
            None => self.stop(cx),
        }
    }

    /// Jumps to a position in the queue, e.g. a double-click in the queue panel.
    pub fn jump_to(&mut self, index: usize, cx: &mut Context<Self>) {
        self.record_listen();
        if let Some(id) = self.queue.jump_to(index) {
            self.start(id, None, false, cx);
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        let seconds = seconds.max(0.0).min(self.length.max(0.0));
        self.position = seconds;
        self.engine.seek(Duration::from_secs_f64(seconds));
    }

    /// 0.0..=1.0 of the track. What the seek bar hands us.
    pub fn seek_fraction(&mut self, fraction: f32) {
        if self.length > 0.0 {
            self.seek(self.length * f64::from(fraction.clamp(0.0, 1.0)));
        }
    }

    /// Shows a dragged position without seeking on every pixel of movement.
    pub fn scrub_to(&mut self, fraction: f32, cx: &mut Context<Self>) {
        if self.length > 0.0 {
            self.scrubbing = Some(self.length * f64::from(fraction.clamp(0.0, 1.0)));
            cx.notify();
        }
    }

    pub fn commit_scrub(&mut self, cx: &mut Context<Self>) {
        if let Some(position) = self.scrubbing.take() {
            self.seek(position);
            cx.notify();
        }
    }

    // -- settings ---------------------------------------------------------

    /// What the engine is told: the slider multiplied by the user's ceiling.
    /// The one place the two are combined, so the slider stays 0.0–1.0
    /// everywhere else.
    fn gain(&self) -> f32 {
        crate::settings::effective_gain(self.volume, self.max_volume)
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0.0, 1.0);
        // Touching the volume slider is an unmute.
        self.muted = false;
        self.engine.set_volume(self.gain());
        match self.per_track_target() {
            Some(id) => self.stage_audio(id, |audio| audio.volume = Some(volume), cx),
            None => self.global_volume = self.volume,
        }
        cx.notify();
    }

    /// Mute is deliberately global. It is "silence, now", not a property of the
    /// song that happens to be playing.
    pub fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.muted = muted;
        self.engine.set_mute(muted, self.gain());
        cx.notify();
    }

    /// Raises or lowers what a full slider is worth. Applies immediately and
    /// deliberately does not move the slider — the user set that where they
    /// wanted it, and a ceiling change is not a volume change.
    pub fn set_max_volume(&mut self, ceiling: f32, cx: &mut Context<Self>) {
        self.max_volume = ceiling.clamp(
            crate::settings::MIN_MAX_VOLUME,
            crate::settings::MAX_MAX_VOLUME,
        );
        if !self.muted {
            self.engine.set_volume(self.gain());
        }
        cx.notify();
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.set_muted(!self.muted, cx);
    }

    pub fn set_speed(&mut self, speed: f32, cx: &mut Context<Self>) {
        self.speed = audio::clamp_speed(speed);
        self.engine.set_speed(self.speed);
        cx.notify();
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        let was_next = self.queue.peek_next();
        self.queue.set_shuffle(!self.queue.shuffle());
        self.resync_next(was_next);
        cx.notify();
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        let was_next = self.queue.peek_next();
        self.queue.set_repeat(self.queue.repeat().next());
        self.resync_next(was_next);
        cx.notify();
    }

    /// Tells the engine about a new "next track", but only when it really is a
    /// new one. Cycling repeat from Off to All in the middle of an album does not
    /// change what plays next, and the engine must not be asked to unpick a
    /// preload for nothing — dropping one can cost a deck rebuild.
    fn resync_next(&mut self, was_next: Option<TrackId>) {
        if self.queue.peek_next() == was_next {
            return;
        }
        self.engine.clear_preload();
        self.prime_next();
    }

    pub fn set_equalizer(&mut self, settings: EqSettings, cx: &mut Context<Self>) {
        self.engine.set_eq(settings.clone());
        self.eq = settings.clone();
        match self.per_track_target() {
            Some(id) => {
                let json = serde_json::to_string(&settings).ok();
                self.stage_audio(id, move |audio| audio.eq = json, cx);
            }
            None => self.global_eq = settings,
        }
        cx.notify();
    }

    /// The curve being heard, which with per-song audio on is the current
    /// song's rather than the settings file's.
    pub fn equalizer(&self) -> &EqSettings {
        &self.eq
    }

    /// Turns per-song audio on or off.
    ///
    /// Switching it off does not delete anything — the rows stay, so turning it
    /// back on restores what was there rather than starting from nothing.
    pub fn set_remember_audio(&mut self, remember: bool, cx: &mut Context<Self>) {
        if self.remember_audio == remember {
            return;
        }
        self.flush_track_audio();
        self.remember_audio = remember;
        match (remember, self.current.as_ref().map(|track| track.id)) {
            // Newly on: whatever this song remembers takes effect now rather
            // than at the next track.
            (true, Some(id)) => self.apply_track_audio(id),
            // Newly off: back to one volume and one curve for everything.
            (false, _) => self.restore_global_audio(),
            _ => {}
        }
        cx.notify();
    }

    /// Puts the settings-file volume and curve back into force. What a song
    /// with nothing remembered plays at, and what a loose file — which has no
    /// row to remember anything against — always plays at.
    fn restore_global_audio(&mut self) {
        self.flush_track_audio();
        self.volume = self.global_volume;
        self.engine.set_volume(match self.muted {
            true => 0.0,
            false => self.gain(),
        });
        self.engine.set_eq(self.global_eq.clone());
        self.eq = self.global_eq.clone();
    }

    // -- per-song volume and equalizer ------------------------------------

    /// The track a volume or equalizer change should be remembered against, or
    /// `None` when the change belongs to the global settings instead.
    ///
    /// A loose file has no row to hang a setting on, so it gets the global
    /// values and changes them, exactly as it always did.
    pub fn per_track_target(&self) -> Option<TrackId> {
        match self.remember_audio {
            true => self.current.as_ref().map(|track| track.id),
            false => None,
        }
    }

    /// Folds a change into whatever is waiting to be written and restarts the
    /// clock on it.
    fn stage_audio(
        &mut self,
        id: TrackId,
        change: impl FnOnce(&mut library::models::TrackAudio),
        cx: &mut Context<Self>,
    ) {
        // A change to a different song means the previous one is finished with.
        if self
            .pending_audio
            .as_ref()
            .is_some_and(|(held, _)| *held != id)
        {
            self.flush_track_audio();
        }
        let (_, audio) = self
            .pending_audio
            .get_or_insert_with(|| (id, library::models::TrackAudio::default()));
        change(audio);

        let db = self.db.clone();
        let pending = self.pending_audio.clone();
        self._save_audio = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_AUDIO_AFTER).await;
            cx.background_executor()
                .spawn(async move {
                    if let Some((id, audio)) = pending {
                        write_track_audio(&db, id, &audio);
                    }
                })
                .await;
        }));
    }

    /// Writes anything waiting, now. The debounced task would not fire in time
    /// on a track change or on the way out of the app.
    pub fn flush_track_audio(&mut self) {
        self._save_audio = None;
        if let Some((id, audio)) = self.pending_audio.take() {
            write_track_audio(&self.db, id, &audio);
        }
    }

    /// Puts a song's remembered volume and curve into force — or, where it has
    /// none, puts the global ones back.
    ///
    /// That second half is the whole reason this is one function: without it,
    /// the volume set for one song would carry on into every song after it.
    fn apply_track_audio(&mut self, id: TrackId) {
        if !self.remember_audio {
            return;
        }
        self.flush_track_audio();

        let stored = {
            match self.db.lock() {
                Ok(db) => queries::track_audio(db.conn(), id).unwrap_or_else(|error| {
                    log::warn!("player: cannot read the settings for track {id}: {error:#}");
                    None
                }),
                Err(_) => None,
            }
        };
        let stored = stored.unwrap_or_default();

        self.volume = stored.volume.unwrap_or(self.global_volume).clamp(0.0, 1.0);
        self.engine.set_volume(match self.muted {
            true => 0.0,
            false => self.gain(),
        });

        let eq = stored
            .eq
            .as_deref()
            .and_then(|json| match serde_json::from_str::<EqSettings>(json) {
                Ok(eq) => Some(eq.clamped()),
                Err(error) => {
                    log::warn!("player: track {id} has an unreadable equalizer curve: {error:#}");
                    None
                }
            })
            .unwrap_or_else(|| self.global_eq.clone());
        self.engine.set_eq(eq.clone());
        self.eq = eq;
    }

    /// Forgets what one song remembers, so it plays at the global settings.
    pub fn forget_track_audio(&mut self, id: TrackId, cx: &mut Context<Self>) {
        // Anything still queued for this song would write the row straight back.
        if self
            .pending_audio
            .as_ref()
            .is_some_and(|(held, _)| *held == id)
        {
            self.pending_audio = None;
            self._save_audio = None;
        }
        if let Ok(db) = self.db.lock()
            && let Err(error) = queries::forget_track_audio(db.conn(), id)
        {
            log::warn!("player: cannot forget the settings for track {id}: {error:#}");
            cx.emit(PlayerEvent::Trouble(format!(
                "Cannot forget this song's volume and equalizer: {error}"
            )));
            return;
        }
        // Playing right now, so the global values come back immediately rather
        // than the next time it is put on.
        if self.current.as_ref().map(|track| track.id) == Some(id) {
            self.apply_track_audio(id);
        }
        cx.notify();
    }

    /// True when this song has a remembered volume or curve — which is what
    /// decides whether the menu offers to forget it.
    pub fn remembers_audio_for(&self, id: TrackId) -> bool {
        if !self.remember_audio {
            return false;
        }
        if self
            .pending_audio
            .as_ref()
            .is_some_and(|(held, _)| *held == id)
        {
            return true;
        }
        let Ok(db) = self.db.lock() else {
            return false;
        };
        queries::track_audio(db.conn(), id)
            .ok()
            .flatten()
            .is_some_and(|audio| !audio.is_empty())
    }

    pub fn set_replay_gain(&mut self, settings: ReplayGainSettings, cx: &mut Context<Self>) {
        self.replay_gain = settings;
        // The new gain applies from the next track; re-applying it mid-track
        // would be an audible jump.
        cx.notify();
    }

    pub fn set_crossfade(&mut self, settings: CrossfadeSettings, cx: &mut Context<Self>) {
        self.engine.set_crossfade(settings);
        self.prime_next();
        cx.notify();
    }

    // -- queue ------------------------------------------------------------

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn remove_from_queue(&mut self, index: usize, cx: &mut Context<Self>) {
        let was_next = self.queue.peek_next();
        self.queue.remove(index);
        self.resync_next(was_next);
        cx.notify();
    }

    pub fn move_in_queue(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let was_next = self.queue.peek_next();
        self.queue.move_entry(from, to);
        self.resync_next(was_next);
        cx.notify();
    }

    pub fn clear_queue(&mut self, cx: &mut Context<Self>) {
        self.queue.clear();
        self.queue_rows.clear();
        self.engine.clear_preload();
        cx.notify();
    }

    /// Fills the queue-row cache for the window of the queue that is on screen,
    /// so the panel can show titles rather than ids. Rows are keyed by track id,
    /// so reordering or shuffling the queue does not invalidate them.
    ///
    /// ponytail: a batched read by primary key on the UI thread, bounded to what
    /// is visible and skipping anything already cached. If it ever stalls behind
    /// a scan's write transaction, move it onto the background executor that the
    /// library's paging already uses.
    pub fn ensure_queue_rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        let end = range.end.min(self.queue.len());
        let wanted: Vec<TrackId> = (range.start.min(end)..end)
            .filter_map(|index| self.queue.id_at(index))
            .filter(|id| !self.queue_rows.contains_key(id))
            .collect();
        if wanted.is_empty() {
            return;
        }

        let rows = {
            let Ok(db) = self.db.lock() else { return };
            queries::tracks_by_id(db.conn(), &wanted)
        };
        match rows {
            Ok(rows) => {
                for track in rows {
                    self.queue_rows.insert(track.id, track);
                }
                cx.notify();
            }
            Err(error) => log::warn!("player: cannot read the queue: {error:#}"),
        }
    }

    /// Records a change of favourite in the rows the player holds.
    ///
    /// The queue's rows are a cache of their own, and so is the playing track —
    /// neither is refreshed by a write the library makes, so the heart in the
    /// queue and the one on the player bar would otherwise stay as they were
    /// until the row happened to be read again.
    pub fn mark_favorite(&mut self, id: TrackId, favorite: bool, cx: &mut Context<Self>) {
        if let Some(track) = self.queue_rows.get_mut(&id) {
            track.favorite = favorite;
        }
        if let Some(track) = self.current.as_mut().filter(|track| track.id == id) {
            track.favorite = favorite;
        }
        cx.notify();
    }

    /// Whether a track is a favourite, if the player holds a row for it.
    pub fn is_favorite(&self, id: TrackId) -> Option<bool> {
        self.current
            .as_ref()
            .filter(|track| track.id == id)
            .or_else(|| self.queue_rows.get(&id))
            .map(|track| track.favorite)
    }

    /// The row at a position in the queue, if it has been read yet. `None` means
    /// "still loading", and the panel draws a placeholder.
    pub fn queue_row(&self, index: usize) -> Option<&Track> {
        self.queue_rows.get(&self.queue.id_at(index)?)
    }

    // -- state for the UI -------------------------------------------------

    pub fn current(&self) -> Option<&Track> {
        self.current.as_ref()
    }

    /// What the player bar should show: the track's title, or the file name when
    /// something outside the library is playing, or nothing at all.
    pub fn now_playing(&self) -> Option<String> {
        match &self.current {
            Some(track) => Some(track.title.clone()),
            None => self.loose.clone(),
        }
    }

    /// The line under the title. Empty for a file with no row behind it.
    pub fn now_playing_detail(&self) -> Option<String> {
        let track = self.current.as_ref()?;
        Some(track.artist.clone()).filter(|artist| !artist.trim().is_empty())
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Seconds, following the user's finger while they scrub.
    pub fn position(&self) -> f64 {
        self.scrubbing.unwrap_or(self.position)
    }

    pub fn length(&self) -> f64 {
        self.length
    }

    /// 0.0..=1.0 for the seek bar.
    pub fn progress(&self) -> f32 {
        match self.length > 0.0 {
            true => (self.position() / self.length).clamp(0.0, 1.0) as f32,
            false => 0.0,
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// What a full slider is worth, for the label beside it.
    pub fn max_volume(&self) -> f32 {
        self.max_volume
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    pub fn shuffle(&self) -> bool {
        self.queue.shuffle()
    }

    pub fn repeat(&self) -> Repeat {
        self.queue.repeat()
    }

    pub fn trouble(&self) -> Option<&str> {
        self.trouble.as_deref()
    }

    pub fn dismiss_trouble(&mut self, cx: &mut Context<Self>) {
        self.trouble = None;
        cx.notify();
    }

    pub fn spectrum(&self) -> &audio::Spectrum {
        self.engine.spectrum()
    }

    /// What to write into the settings file at shutdown.
    pub fn session(&self) -> Session {
        Session {
            queue: Some(self.queue.clone()),
            track: self.current.as_ref().map(|track| track.id),
            position: self.position,
        }
    }

    /// Restores a saved session. The track is loaded paused at its old position:
    /// an app that starts making noise on its own is an app people uninstall.
    pub fn restore(&mut self, session: &Session, resume_position: bool, cx: &mut Context<Self>) {
        let Some(queue) = session.queue.clone() else {
            return;
        };
        self.queue = queue;
        // Tracks removed from the library since last time are dropped here
        // rather than failing one by one at playback.
        let known = self.known_ids();
        self.queue.retain_known(&|id| known.contains(&id));

        let Some(id) = session.track.or_else(|| self.queue.current()) else {
            return;
        };
        let at = match resume_position && session.position > 1.0 {
            true => Some(session.position),
            false => None,
        };
        self.start(id, at, true, cx);
    }

    pub fn shutdown(&self) {
        self.engine.shutdown();
    }

    /// The shape of the file playing now, one peak per slice. Empty while the
    /// scan is still running, or when nothing is drawing waveforms.
    pub fn waveform(&self) -> &[f32] {
        &self.waveform
    }

    /// Turned on by the seek-bar setting. Switching it on mid-song scans what is
    /// already playing rather than waiting for the next track.
    pub fn set_waveform_wanted(&mut self, wanted: bool, cx: &mut Context<Self>) {
        if self.wants_waveform == wanted {
            return;
        }
        self.wants_waveform = wanted;
        match wanted {
            true => {
                if let Some(path) = self.current_path() {
                    self.scan_waveform(path, cx);
                }
            }
            false => {
                self._waveform = None;
                self.waveform.clear();
                self.waveform_for = None;
                cx.notify();
            }
        }
    }

    /// Reads the file's peaks on a background thread.
    ///
    /// Decoding a whole song takes a moment, so the bar draws flat until this
    /// lands. A scan already running for another file is dropped: only what is
    /// playing now is worth the CPU.
    fn scan_waveform(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.waveform_for.as_deref() == Some(path.as_path()) {
            return;
        }
        self.waveform.clear();
        self.waveform_for = Some(path.clone());
        cx.notify();

        self._waveform = Some(cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { audio::peaks::scan(&path) }
                })
                .await;

            this.update(cx, |player, cx| {
                // The track may have moved on while this ran; peaks for a file
                // nobody is playing would draw the wrong song.
                if player.waveform_for.as_deref() != Some(path.as_path()) {
                    return;
                }
                match scanned {
                    Ok(peaks) => player.waveform = peaks,
                    Err(error) => {
                        log::warn!(
                            "player: cannot read the waveform of {}: {error:#}",
                            path.display()
                        );
                        player.waveform.clear();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The file playing now, library row or loose file alike.
    fn current_path(&self) -> Option<PathBuf> {
        self.current.as_ref().map(|track| track.path.clone())
    }

    // -- internals --------------------------------------------------------

    /// Loads a track into the engine and makes it current.
    fn start(&mut self, id: TrackId, at: Option<f64>, paused: bool, cx: &mut Context<Self>) {
        let Some(track) = self.lookup(id) else {
            log::warn!("player: track {id} is no longer in the library");
            self.skip(Advance::Ended, cx);
            return;
        };

        let gain = replaygain::gain_for(&self.replay_gain, &gain_tags(&track));
        match at {
            Some(seconds) => self.engine.load_at(
                track.path.clone(),
                Duration::from_secs_f64(seconds),
                gain,
                paused,
            ),
            None if paused => self
                .engine
                .load_at(track.path.clone(), Duration::ZERO, gain, true),
            None => self.engine.load(track.path.clone(), gain),
        }

        self.loose = None;
        self.length = track.duration;
        self.position = at.unwrap_or(0.0);
        // A drag that was never released must not follow the user into the next
        // track: the seek bar reads the scrub position in preference to the real
        // one, and a stale one would leave it stuck wherever it was let go.
        self.scrubbing = None;
        self.listened_ms = 0.0;
        self.playing = !paused;
        let mut track = track;
        // Stamped here rather than when the listen is recorded: "recently
        // played" is the songs you put on, and that is known now. Waiting meant
        // a track only appeared once it had finished — and never at all if it
        // was skipped.
        if !paused {
            track.last_played = Some(self.stamp_played(id));
        }
        let path = track.path.clone();
        self.current = Some(track);
        // After `current` is set, because what a song remembers is applied to
        // the song that is now playing.
        self.apply_track_audio(id);
        if self.wants_waveform {
            self.scan_waveform(path, cx);
        }
        cx.emit(PlayerEvent::TrackChanged(Some(id)));
        self.prime_next();
        cx.notify();
    }

    /// Tells the engine what comes next, which is what makes the boundary
    /// gapless (or lets a crossfade start on time).
    fn prime_next(&self) {
        let Some(next) = self.queue.peek_next() else {
            return;
        };
        // Repeat-one would ask the engine to preload the file it is already
        // playing; it handles that boundary itself.
        if self.current.as_ref().map(|track| track.id) == Some(next) {
            return;
        }
        let Some(track) = self.lookup(next) else {
            return;
        };
        let gain = replaygain::gain_for(&self.replay_gain, &gain_tags(&track));
        self.engine.preload(track.path, gain);
    }

    /// The library row for a path, if this file has one.
    fn lookup_path(&self, path: &std::path::Path) -> Option<Track> {
        let db = self.db.lock().ok()?;
        match queries::track_by_path(db.conn(), path) {
            Ok(track) => track,
            Err(error) => {
                log::warn!("player: cannot look up {}: {error:#}", path.display());
                None
            }
        }
    }

    /// ponytail: a point lookup by primary key, run on the UI thread. It is one
    /// indexed row and happens once per track change. If the scanner's write
    /// transactions ever make this visibly stall, move it behind the same
    /// background task the library uses for its pages.
    fn lookup(&self, id: TrackId) -> Option<Track> {
        let db = self.db.lock().ok()?;
        match queries::track(db.conn(), id) {
            Ok(track) => track,
            Err(error) => {
                log::warn!("player: cannot read track {id}: {error:#}");
                None
            }
        }
    }

    fn known_ids(&self) -> std::collections::HashSet<TrackId> {
        let Ok(db) = self.db.lock() else {
            return Default::default();
        };
        let ids = self.queue.in_play_order();
        match queries::tracks_by_id(db.conn(), &ids) {
            Ok(tracks) => tracks.into_iter().map(|track| track.id).collect(),
            Err(error) => {
                log::warn!("player: cannot check the restored queue: {error:#}");
                ids.into_iter().collect()
            }
        }
    }

    /// Records that a track was put on, and returns the moment it happened.
    ///
    /// Only `last_played` — the play count is the listen's business, so Most
    /// Played stays a count of songs heard rather than songs started.
    fn stamp_played(&self, id: TrackId) -> i64 {
        let at = now_ms();
        let Ok(db) = self.db.lock() else { return at };
        if let Err(error) = queries::mark_played(db.conn(), id, at) {
            log::warn!("player: cannot record that {id} was played: {error:#}");
        }
        at
    }

    /// Writes one row of listening history. Called whenever a track stops being
    /// the current one, so a skip is recorded as a skip.
    fn record_listen(&mut self) {
        let Some(track) = &self.current else { return };
        if self.listened_ms < 1000.0 {
            return;
        }
        let completion = match track.duration > 0.0 {
            true => (self.listened_ms / 1000.0 / track.duration).clamp(0.0, 1.0) as f32,
            false => 0.0,
        };
        let listen = Listen {
            track_id: track.id,
            played_at: now_ms(),
            listened_ms: self.listened_ms as i64,
            completion,
        };
        self.listened_ms = 0.0;

        let Ok(db) = self.db.lock() else { return };
        if let Err(error) = queries::record_listen(db.conn(), listen) {
            log::warn!("player: cannot record a listen: {error:#}");
        }
    }
}

/// Whether a path is a playlist we can open, rather than a song.
///
/// The check is by extension, the same way `playlist::formats` chooses its
/// parser — so what this says yes to is exactly what `play_playlist_file` can
/// read.
pub fn is_playlist_file(path: &std::path::Path) -> bool {
    playlist::formats::Format::from_path(path).is_some()
}

/// Which of the two lengths to believe: the library's, when it has one.
///
/// The scanner reads the file's headers; the decoder estimates from the bitrate
/// and gets VBR badly wrong. Zero means "not known", which is what a file with
/// no library row behind it has.
/// Writes one song's remembered volume and curve.
///
/// A failure here costs a preference and nothing else, so it is logged rather
/// than surfaced: there is no useful thing the user could do about it, and a
/// notice on every slider drag would be worse than the problem.
fn write_track_audio(db: &Arc<Mutex<Db>>, id: TrackId, audio: &library::models::TrackAudio) {
    let Ok(db) = db.lock() else { return };
    if let Err(error) = queries::save_track_audio(db.conn(), id, audio, library::models::now_ms()) {
        log::warn!("player: cannot save the settings for track {id}: {error:#}");
    }
}

fn best_length(known: f64, reported: f64) -> f64 {
    match known > 0.0 {
        true => known,
        false => reported,
    }
}

fn gain_tags(track: &Track) -> TrackGain {
    TrackGain {
        track_gain_db: track.replay_gain.track_gain,
        track_peak: track.replay_gain.track_peak,
        album_gain_db: track.replay_gain.album_gain,
        album_peak: track.replay_gain.album_peak,
    }
}

fn name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Entities are created by `crate::init`; this alias keeps the signatures short.
pub type PlayerHandle = Entity<Player>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanned_duration_beats_the_decoders_guess() {
        // A VBR file the scanner read as 4:58 was reported by the decoder as
        // 1:27, so the seek bar ran out halfway through and the clock counted
        // past the end of the track.
        assert_eq!(best_length(298.0, 87.0), 298.0);
        // A file with no library row has only the decoder to go on.
        assert_eq!(best_length(0.0, 87.0), 87.0);
        // Neither known: nothing to show, rather than a bogus number.
        assert_eq!(best_length(0.0, 0.0), 0.0);
    }

    #[test]
    fn playlist_files_are_told_apart_from_songs() {
        // The branch that decides whether an opened file fills the queue or is
        // played as one song. Case-insensitive, because Explorer is.
        for name in ["mix.m3u8", "mix.m3u", "mix.M3U8", "mix.pls", "mix.xspf"] {
            assert!(is_playlist_file(&PathBuf::from(name)), "{name}");
        }
        for name in ["song.mp3", "song.flac", "notes.txt", "noextension"] {
            assert!(!is_playlist_file(&PathBuf::from(name)), "{name}");
        }
    }

    #[test]
    fn name_of_prefers_the_file_name() {
        assert_eq!(name_of(&PathBuf::from(r"d:\music\a\b.flac")), "b.flac");
        assert_eq!(name_of(&PathBuf::from("")), "");
    }

    #[test]
    fn gain_tags_carry_every_value_the_row_has() {
        let mut track = sample_track();
        track.replay_gain = library::models::ReplayGainTags {
            track_gain: Some(-6.0),
            track_peak: Some(0.98),
            album_gain: Some(-4.0),
            album_peak: Some(0.99),
        };
        let tags = gain_tags(&track);
        assert_eq!(tags.track_gain_db, Some(-6.0));
        assert_eq!(tags.album_peak, Some(0.99));
    }

    fn sample_track() -> Track {
        Track {
            id: 1,
            path: PathBuf::from(r"d:\m\a.flac"),
            title: "A".into(),
            artist: "B".into(),
            album_artist: None,
            album: None,
            album_id: None,
            artist_id: None,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            composer: None,
            comment: None,
            bpm: None,
            duration: 200.0,
            bitrate: None,
            sample_rate: None,
            channels: None,
            codec: None,
            file_size: 0,
            replay_gain: Default::default(),
            date_added: 0,
            last_played: None,
            play_count: 0,
            rating: 0,
            favorite: false,
            artwork_id: None,
            missing: false,
        }
    }
}
