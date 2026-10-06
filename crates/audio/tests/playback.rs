//! Plays a real file through the real output device.
//!
//! This is the one test that needs a sound card, so it only runs when
//! `TINNITUS_TEST_MUSIC` points at a folder of audio:
//!
//! ```text
//! TINNITUS_TEST_MUSIC="D:\music" cargo test -p audio --test playback -- --nocapture
//! ```
//!
//! It covers the part no unit test can: that the decoder, the mixer, the
//! equalizer, the gain stage and the device actually fit together and move a
//! playhead.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use audio::dsp::{EqSettings, Preset};
use audio::{Engine, PlaybackEvent};

/// An audio file to play, if the machine has one to offer.
fn a_track() -> Option<PathBuf> {
    let folder = PathBuf::from(std::env::var_os("TINNITUS_TEST_MUSIC")?);
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        ["mp3", "flac", "wav", "ogg", "opus", "m4a"]
                            .contains(&extension.to_ascii_lowercase().as_str())
                    })
        })
        .collect();
    files.sort();
    files.into_iter().next()
}

/// Collects events until `wanted` says stop, or the deadline passes.
fn drain(
    events: &Receiver<PlaybackEvent>,
    within: Duration,
    mut wanted: impl FnMut(&PlaybackEvent) -> bool,
) -> Vec<PlaybackEvent> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let Ok(event) = events.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        let stop = wanted(&event);
        seen.push(event);
        if stop {
            break;
        }
    }
    seen
}

#[test]
fn plays_a_real_file_and_the_playhead_moves() {
    let Some(track) = a_track() else {
        eprintln!("skipping: set TINNITUS_TEST_MUSIC to a folder of audio files");
        return;
    };
    eprintln!("playing {}", track.display());

    // Silent: this runs on a developer's machine, and a test suite that makes
    // noise is a test suite people stop running.
    let (engine, events) = Engine::start(0.0, EqSettings::default());
    engine.load(&track, 1.0);

    let started = drain(&events, Duration::from_secs(5), |event| {
        matches!(event, PlaybackEvent::Playing(_))
    });
    assert!(
        started
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Playing(_))),
        "never started playing; saw {started:?}"
    );
    assert!(
        !started
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Unavailable { .. })),
        "the engine refused the file: {started:?}"
    );

    // Let it actually run, then check the playhead moved.
    let positions = drain(&events, Duration::from_secs(3), |_| false);
    let furthest = positions
        .iter()
        .filter_map(|event| match event {
            PlaybackEvent::Position(at) | PlaybackEvent::Playing(at) => Some(*at),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    assert!(
        furthest > Duration::from_millis(500),
        "the playhead barely moved ({furthest:?}) — audio is not flowing"
    );

    // Pause must stop it moving.
    engine.pause();
    drain(&events, Duration::from_millis(500), |event| {
        matches!(event, PlaybackEvent::Paused(_))
    });
    let after_pause = drain(&events, Duration::from_millis(700), |_| false);
    assert!(
        !after_pause
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Position(_))),
        "the playhead kept moving while paused: {after_pause:?}"
    );

    // Seek, resume, and confirm it picked up where it was told to.
    engine.seek(Duration::from_secs(30));
    engine.play();
    let resumed = drain(
        &events,
        Duration::from_secs(3),
        |event| matches!(event, PlaybackEvent::Position(at) if *at > Duration::from_secs(30)),
    );
    assert!(
        resumed.iter().any(
            |event| matches!(event, PlaybackEvent::Position(at) | PlaybackEvent::Playing(at)
                if *at >= Duration::from_secs(29))
        ),
        "seeking to 30s did not take: {resumed:?}"
    );

    engine.shutdown();
}

#[test]
fn the_equalizer_can_be_changed_while_a_file_plays() {
    let Some(track) = a_track() else {
        eprintln!("skipping: set TINNITUS_TEST_MUSIC to a folder of audio files");
        return;
    };

    let (engine, events) = Engine::start(0.0, EqSettings::default());
    engine.load(&track, 1.0);
    drain(&events, Duration::from_secs(5), |event| {
        matches!(event, PlaybackEvent::Playing(_))
    });

    // Swapping the curve mid-stream must not stall or kill playback: the
    // coefficients are published through an ArcSwap precisely so this is safe.
    for preset in [Preset::BassBoost, Preset::Vocal, Preset::Flat] {
        engine.set_eq(EqSettings::from_preset(preset));
        std::thread::sleep(Duration::from_millis(120));
    }

    let after = drain(&events, Duration::from_secs(2), |_| false);
    assert!(
        after
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Position(_))),
        "playback stopped after changing the equalizer: {after:?}"
    );
    assert!(
        !after
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Stopped)),
        "the engine stopped after an equalizer change"
    );

    engine.shutdown();
}

#[test]
fn a_file_that_is_not_audio_is_reported_and_does_not_wedge_the_engine() {
    let directory = std::env::temp_dir().join("tinnitus-playback-test");
    std::fs::create_dir_all(&directory).unwrap();
    let rubbish = directory.join("not-audio.mp3");
    std::fs::write(&rubbish, b"this is not an mp3").unwrap();

    let (engine, events) = Engine::start(0.0, EqSettings::default());
    engine.load(&rubbish, 1.0);

    let seen = drain(&events, Duration::from_secs(5), |event| {
        matches!(
            event,
            PlaybackEvent::Unavailable { .. } | PlaybackEvent::OutputLost { .. }
        )
    });
    assert!(
        seen.iter().any(|event| matches!(
            event,
            PlaybackEvent::Unavailable { .. } | PlaybackEvent::OutputLost { .. }
        )),
        "a broken file produced no report at all: {seen:?}"
    );

    engine.shutdown();
    std::fs::remove_file(&rubbish).ok();
}
