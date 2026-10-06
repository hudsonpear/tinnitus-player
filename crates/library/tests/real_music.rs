//! Scans a real folder of real audio files.
//!
//! The unit tests build synthetic WAVs, which proves the plumbing but not that
//! we read what actual encoders write. Point `TINNITUS_TEST_MUSIC` at a folder of
//! music and this exercises the whole scan against it:
//!
//! ```text
//! TINNITUS_TEST_MUSIC="D:\music" cargo test -p library --test real_music -- --nocapture
//! ```
//!
//! With the variable unset it says it is skipping and passes, so the suite stays
//! green on a machine with no music on it.

use std::path::PathBuf;
use std::sync::Mutex;

use library::models::{Folder, LibraryView, Sort, now_ms};
use library::{ArtworkCache, Cancel, Db};

fn music_folder() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("TINNITUS_TEST_MUSIC")?);
    path.is_dir().then_some(path)
}

/// An in-memory library with `music` registered as a folder.
struct Fixture {
    db: Mutex<Db>,
    artwork: ArtworkCache,
    folder: Folder,
    _cache: tempfile::TempDir,
}

impl Fixture {
    fn new(music: PathBuf) -> Self {
        let cache = tempfile::tempdir().unwrap();
        let db = Db::in_memory().unwrap();
        let id = db.add_folder(&music, true).unwrap();
        Self {
            folder: Folder {
                id,
                path: music,
                watch: true,
                added_at: now_ms(),
            },
            artwork: ArtworkCache::new(cache.path().to_path_buf()).unwrap(),
            db: Mutex::new(db),
            _cache: cache,
        }
    }

    fn scan(&self) -> (usize, usize, usize) {
        library::scan(
            &self.db,
            &self.artwork,
            std::slice::from_ref(&self.folder),
            &Cancel::new(),
            &|_| {},
        )
        .unwrap()
    }
}

#[test]
fn scans_a_real_folder_and_reads_its_tags() {
    let Some(music) = music_folder() else {
        eprintln!("skipping: set TINNITUS_TEST_MUSIC to a folder of audio files");
        return;
    };
    let fixture = Fixture::new(music.clone());

    let (added, updated, removed) = fixture.scan();
    assert!(
        added > 0,
        "found no audio in {} — is it really a music folder?",
        music.display()
    );
    assert_eq!((updated, removed), (0, 0));

    {
        let db = fixture.db.lock().unwrap();
        let tracks = library::db::queries::tracks_page(
            db.conn(),
            &LibraryView::AllSongs,
            Sort::default(),
            0,
            added,
        )
        .unwrap();
        assert_eq!(tracks.len(), added);

        for track in &tracks {
            // Every track must be identifiable and point at a real file.
            assert!(
                track.path.is_file(),
                "{} is not a file",
                track.path.display()
            );
            assert!(
                !track.title.is_empty(),
                "{} has no title",
                track.path.display()
            );
            assert!(track.file_size > 0);

            eprintln!(
                "{:>7.1}s  {:<28} {:<24} {}",
                track.duration,
                truncate(&track.title, 28),
                truncate(&track.artist, 24),
                track.codec.as_deref().unwrap_or("?")
            );
        }

        // A duration of zero means the tags would not parse. Those files are
        // indexed on purpose — but if *every* file came out that way, the tag
        // reader is broken rather than the files.
        let timed = tracks.iter().filter(|track| track.duration > 0.0).count();
        assert!(
            timed > 0,
            "not one of {} files yielded a duration",
            tracks.len()
        );
        if timed < tracks.len() {
            eprintln!(
                "note: {} of {} files had unreadable tags and were indexed without a duration",
                tracks.len() - timed,
                tracks.len()
            );
        }
    }

    // A second pass must find nothing new: this is what keeps re-scans cheap.
    let (added_again, _, removed_again) = fixture.scan();
    assert_eq!(added_again, 0, "a re-scan re-added tracks");
    assert_eq!(removed_again, 0, "a re-scan lost tracks");
}

#[test]
fn search_finds_a_real_track_by_a_word_from_its_title() {
    let Some(music) = music_folder() else {
        eprintln!("skipping: set TINNITUS_TEST_MUSIC to a folder of audio files");
        return;
    };
    let fixture = Fixture::new(music);
    fixture.scan();

    let db = fixture.db.lock().unwrap();
    let first =
        library::db::queries::tracks_page(db.conn(), &LibraryView::AllSongs, Sort::default(), 0, 1)
            .unwrap();
    let Some(track) = first.first() else { return };

    // The first real word of a real title, typed the way a user would.
    let Some(word) = track
        .title
        .split(|c: char| !c.is_alphanumeric())
        .find(|word| word.chars().count() > 2)
    else {
        return;
    };

    let hits = library::db::queries::search(db.conn(), word, 20).unwrap();
    assert!(
        hits.tracks.iter().any(|hit| hit.id == track.id),
        "searching for {word:?} did not find {:?}",
        track.title
    );
}

fn truncate(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        true => text.chars().take(width - 1).collect::<String>() + "…",
        false => text.to_owned(),
    }
}
