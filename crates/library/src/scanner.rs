//! Library scanner. Walks the configured folders, reads tags in parallel, and
//! writes the results in batched transactions.
//!
//! It never runs on the UI thread and never holds the database lock while
//! touching a file: tags for a whole batch are read first, then the batch is
//! written in one transaction. Playback is unaffected throughout.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use rayon::prelude::*;

use crate::artwork::{self, ArtworkCache};
use crate::db::{Db, ScannedTrack, Upsert, path_text};
use crate::metadata;
use crate::models::{Folder, Millis, ScanEvent};

/// Rows per transaction. Big enough that the per-commit cost disappears, small
/// enough that a list query never waits long behind the scanner.
const BATCH: usize = 500;

/// Set from the UI to stop a scan early. A cancelled scan keeps everything it
/// has already written.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// One file as the walker found it, before its tags have been read.
struct Found {
    path: PathBuf,
    size: i64,
    mtime: Millis,
}

/// Scans every folder. `report` is called from the scanning thread; the caller
/// forwards the events to the UI.
///
/// Returns the number of tracks added, updated and marked missing.
pub fn scan(
    db: &Mutex<Db>,
    artwork: &ArtworkCache,
    folders: &[Folder],
    cancel: &Cancel,
    report: &(dyn Fn(ScanEvent) + Sync),
) -> Result<(usize, usize, usize)> {
    let mut found = Vec::new();
    for folder in folders {
        if cancel.is_cancelled() {
            break;
        }
        walk(&folder.path, &mut found);
    }

    drop_excluded(db, &mut found)?;

    report(ScanEvent::Started { total: found.len() });

    let removed = mark_vanished(db, folders, &found)?;
    let (added, updated) = ingest(db, artwork, &found, cancel, report)?;
    prune(db);

    report(ScanEvent::Finished {
        added,
        updated,
        removed,
        cancelled: cancel.is_cancelled(),
    });
    Ok((added, updated, removed))
}

/// Re-reads only the paths a watcher reported, rather than the whole library.
///
/// This is what a file appearing, changing or disappearing costs: a stat for
/// each path in the batch. Walking every folder to find the one file that moved
/// is what makes copying an album in feel like the app is grinding — each file
/// landing would kick off another full sweep.
///
/// Returns the number of tracks added, updated and marked missing.
pub fn sync_paths(
    db: &Mutex<Db>,
    artwork: &ArtworkCache,
    touched: &[PathBuf],
    gone: &[PathBuf],
    cancel: &Cancel,
) -> Result<(usize, usize, usize)> {
    // Removals first. A rename arrives as a removal plus a creation, and
    // `Db::find_moved` only considers rows already marked missing — so the old
    // row has to be missing before the new path is read, or the track comes back
    // as a stranger and leaves its play count behind. This is the same order the
    // full scan uses.
    let removed = mark_gone(db, gone)?;

    let mut found = Vec::new();
    for path in touched {
        // A file can be created and deleted again before the batch arrives, and
        // a directory can be reported alongside the files inside it.
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if !meta.is_file() || !metadata::is_audio_file(path) {
            continue;
        }
        found.push(Found {
            path: path.clone(),
            size: meta.len() as i64,
            mtime: modified_ms(&meta),
        });
    }
    drop_excluded(db, &mut found)?;

    // No progress events: this is background housekeeping, and flashing the
    // scan bar every time a file lands would be worse than saying nothing.
    let (added, updated) = ingest(db, artwork, &found, cancel, &|_| {})?;
    prune(db);
    Ok((added, updated, removed))
}

/// Drops albums, artists and genres that no track points at any more.
///
/// Every read of a file can be the last one that mentioned a name: retagging a
/// song out of an album leaves that album behind with nothing in it, and the
/// album page then lists a record that does not exist. Only removing a folder
/// used to clear those, so they accumulated.
///
/// Housekeeping, so a failure is logged rather than thrown: the scan itself
/// succeeded, and the stale rows can go on the next pass.
fn prune(db: &Mutex<Db>) {
    let db = db.lock().expect("library mutex poisoned");
    if let Err(error) = db.prune_orphans() {
        log::warn!("scanner: cannot drop empty albums and artists: {error:#}");
    }
}

/// Marks the rows at these paths missing, and everything under them when a
/// whole directory went away.
fn mark_gone(db: &Mutex<Db>, gone: &[PathBuf]) -> Result<usize> {
    if gone.is_empty() {
        return Ok(0);
    }
    let db = db.lock().expect("library mutex poisoned");
    let conn = db.conn();

    let mut ids = Vec::new();
    for path in gone {
        ids.extend(crate::db::queries::ids_under_path(conn, path)?);
    }
    ids.sort_unstable();
    ids.dedup();

    Db::mark_missing(conn, &ids)?;
    Ok(ids.len())
}

/// Drops the files the user took out of the library on purpose.
///
/// They are still sitting in a watched folder, so without this every scan finds
/// them again and puts the rows straight back.
fn drop_excluded(db: &Mutex<Db>, found: &mut Vec<Found>) -> Result<()> {
    let excluded: std::collections::HashSet<String> = {
        let db = db.lock().expect("library mutex poisoned");
        crate::db::queries::excluded_paths(db.conn())?
            .into_iter()
            .map(|path| path.to_lowercase())
            .collect()
    };
    if excluded.is_empty() {
        return Ok(());
    }
    found.retain(|file| match path_text(&file.path) {
        Ok(text) => !excluded.contains(&text.to_lowercase()),
        // A path we cannot even render is one `ingest` will refuse too.
        Err(_) => true,
    });
    Ok(())
}

/// Collects audio files under `root`. Unreadable directories are logged and
/// stepped over: one bad permission must not end the scan.
fn walk(root: &Path, out: &mut Vec<Found>) {
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_hidden(entry.path()));

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!(
                    "scanner: skipping an entry under {}: {error}",
                    root.display()
                );
                continue;
            }
        };
        if !entry.file_type().is_file() || !metadata::is_audio_file(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        out.push(Found {
            path: entry.path().to_path_buf(),
            size: meta.len() as i64,
            mtime: modified_ms(&meta),
        });
    }
}

/// Marks rows whose file is gone. They are not deleted: an unmounted drive must
/// not wipe half the library. `Db::purge_missing` removes them on an explicit
/// clean-up.
fn mark_vanished(db: &Mutex<Db>, folders: &[Folder], found: &[Found]) -> Result<usize> {
    let present: std::collections::HashSet<String> = found
        .iter()
        .filter_map(|file| path_text(&file.path).ok())
        .map(|path| path.to_lowercase())
        .collect();

    let db = db.lock().expect("library mutex poisoned");
    let conn = db.conn();

    let mut vanished = Vec::new();
    for folder in folders {
        let mut statement =
            conn.prepare("SELECT id, path FROM tracks WHERE folder_id = ?1 AND missing = 0")?;
        let rows = statement.query_map([folder.id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, path) = row?;
            if !present.contains(&path.to_lowercase()) {
                vanished.push(id);
            }
        }
    }

    Db::mark_missing(conn, &vanished)?;
    Ok(vanished.len())
}

/// Reads tags for everything that changed and writes it, one batch at a time.
fn ingest(
    db: &Mutex<Db>,
    artwork: &ArtworkCache,
    found: &[Found],
    cancel: &Cancel,
    report: &(dyn Fn(ScanEvent) + Sync),
) -> Result<(usize, usize)> {
    let total = found.len();
    let mut done = 0;
    let mut added = 0;
    let mut updated = 0;

    for batch in batches(found) {
        if cancel.is_cancelled() {
            break;
        }

        // Skip files whose size and mtime still match the row. This is the
        // difference between a re-scan taking seconds and taking minutes.
        let stale: Vec<&Found> = {
            let db = db.lock().expect("library mutex poisoned");
            batch
                .iter()
                .filter(|file| {
                    !Db::is_unchanged(db.conn(), &file.path, file.size, file.mtime).unwrap_or(false)
                })
                .collect()
        };

        // Tags and cover bytes are read off the lock, in parallel. A file that
        // fails to parse is dropped with a warning; the scan carries on.
        // A file whose tags will not parse is still indexed: decoders routinely
        // play files that tag readers reject, and a track missing from the
        // library is worse than a track with a filename for a title.
        let probed: Vec<(&Found, metadata::Probed)> = stale
            .into_par_iter()
            .map(|file| (file, metadata::probe_lenient(&file.path)))
            .collect();

        {
            let mut db = db.lock().expect("library mutex poisoned");
            let shared = shareable_folders(db.conn(), batch, &probed)?;

            db.transaction(|conn| {
                for (file, probed) in &probed {
                    let path = path_text(&file.path)?;
                    let folder_id = crate::db::queries::folder_for_path(conn, &path)?;

                    let artwork_id = cover(conn, artwork, &file.path, probed, &shared);

                    // A file that vanished and reappeared with the same shape is
                    // the same track moved, so its row (and play count) follows.
                    // Only a path the library has never seen can be the far end
                    // of a move: a file that already has a row of its own is
                    // simply itself, changed.
                    if Db::id_for_path(conn, &file.path)?.is_none()
                        && let Some(moved) =
                            Db::find_moved(conn, file.size, probed.properties.duration)?
                    {
                        Db::relocate_track(conn, moved, &file.path)?;
                    }

                    let scanned = ScannedTrack {
                        path: file.path.clone(),
                        folder_id,
                        tags: probed.tags.clone(),
                        properties: probed.properties,
                        codec: probed.codec.clone(),
                        file_size: file.size,
                        mtime: file.mtime,
                        artwork_id,
                    };
                    match Db::upsert_track(conn, &scanned)? {
                        Upsert::Inserted(_) => added += 1,
                        Upsert::Updated(_) => updated += 1,
                    }
                }
                Ok(())
            })
            .context("cannot write a batch of scanned tracks")?;
        }

        done += batch.len();
        let current: PathBuf = batch
            .last()
            .map(|file| file.path.clone())
            .unwrap_or_default();
        report(ScanEvent::Progress {
            done,
            total,
            current,
        });
    }

    Ok((added, updated))
}

/// Batches for the write loop, never splitting a directory across two of them.
///
/// The folder-art decision is made per directory from the tags of the files in
/// it, so a directory cut in half would be judged twice on half the evidence.
/// `found` comes out of the walker in directory order, which is what makes this
/// no more than extending a batch to the next boundary.
fn batches(found: &[Found]) -> Vec<&[Found]> {
    let mut batches = Vec::new();
    let mut start = 0;
    while start < found.len() {
        let mut end = (start + BATCH).min(found.len());
        while end < found.len() && found[end].path.parent() == found[end - 1].path.parent() {
            end += 1;
        }
        batches.push(&found[start..end]);
        start = end;
    }
    batches
}

/// Directories whose files may share the art sitting beside them.
///
/// A `cover.jpg` belongs to the album it was downloaded with. In a folder that
/// holds one album, that is every file in it. In a folder someone has dropped
/// two hundred unrelated singles into, it is one of them — and putting it on the
/// other hundred and ninety-nine is worse than showing no art at all, because a
/// wrong cover reads as a wrong track.
///
/// So a directory shares its art when its files agree on one album *or* on one
/// artist, and refuses only when they agree on neither. Both halves earn their
/// place: a compilation disagrees about the artist by design and still has one
/// cover, and an artist's folder holding three of their records disagrees about
/// the album while the cover is still theirs. It takes a folder that is neither
/// one record nor one artist — a heap of downloads — for the cover to belong to
/// somebody who is not there, and that is the case worth refusing.
///
/// A directory with no tags at all shares too: there is nobody there to
/// attribute the cover to wrongly.
///
/// The evidence is the tags just read for the files that changed, plus the rows
/// already stored for the ones that did not — a re-scan that touches one file
/// must still see the whole directory it sits in.
fn shareable_folders(
    conn: &rusqlite::Connection,
    batch: &[Found],
    probed: &[(&Found, metadata::Probed)],
) -> Result<HashSet<PathBuf>> {
    let mut albums: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut artists: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut folders: HashSet<PathBuf> = HashSet::new();

    let mut record = |path: &Path, album: Option<&str>, artist: Option<&str>| {
        let Some(folder) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        folders.insert(folder.clone());
        if let Some(album) = name(album) {
            albums.entry(folder.clone()).or_default().insert(album);
        }
        if let Some(artist) = name(artist) {
            artists.entry(folder).or_default().insert(artist);
        }
    };

    let fresh: HashSet<&Path> = probed.iter().map(|(file, _)| file.path.as_path()).collect();
    for (file, probed) in probed {
        record(
            &file.path,
            probed.tags.album.as_deref(),
            // What a compilation agrees on is its album artist; fall back to the
            // track artist for the files that carry no such tag.
            probed
                .tags
                .album_artist
                .as_deref()
                .or(probed.tags.artist.as_deref()),
        );
    }

    let known: Vec<&Path> = batch
        .iter()
        .map(|file| file.path.as_path())
        .filter(|path| !fresh.contains(path))
        .collect();
    for (path, album, artist) in crate::db::queries::identities(conn, &known)? {
        record(&path, album.as_deref(), artist.as_deref());
    }

    folders.retain(|folder| {
        let agree = |seen: &HashMap<PathBuf, HashSet<String>>| {
            seen.get(folder).is_none_or(|names| names.len() <= 1)
        };
        agree(&albums) || agree(&artists)
    });
    Ok(folders)
}

/// A tag value as it is compared: trimmed, folded, and empty treated as absent.
fn name(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_lowercase())
}

/// Embedded picture first, folder art second, nothing third. A failure to store
/// art is never a failure to store the track.
fn cover(
    conn: &rusqlite::Connection,
    artwork: &ArtworkCache,
    path: &Path,
    probed: &metadata::Probed,
    shared: &HashSet<PathBuf>,
) -> Option<i64> {
    if let Some(picture) = &probed.picture {
        match artwork.store(conn, &picture.data, picture.mime.as_deref(), "embedded") {
            Ok(id) => return Some(id),
            Err(error) => log::debug!("scanner: cover in {} unusable: {error:#}", path.display()),
        }
    }

    // The file carries no picture of its own, so the only art on offer is
    // whatever sits beside it — which is only this track's art if the folder
    // holds one record rather than a heap of unrelated songs.
    let folder = path.parent()?;
    if !shared.contains(folder) {
        return None;
    }

    let beside = artwork::folder_art(path)?;
    let data = std::fs::read(&beside).ok()?;
    match artwork.store(conn, &data, None, "folder") {
        Ok(id) => Some(id),
        Err(error) => {
            log::debug!(
                "scanner: folder art {} unusable: {error:#}",
                beside.display()
            );
            None
        }
    }
}

/// Skips dotted directories and Windows' hidden system folders, which never hold
/// a music library and can be enormous.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != "." && name != "..")
}

fn modified_ms(meta: &std::fs::Metadata) -> Millis {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_millis() as Millis)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::now_ms;

    /// A minimal but real WAV file, so lofty has something it can actually parse.
    fn wav(path: &Path, seconds: u32) {
        let rate = 8000u32;
        let frames = rate * seconds;
        let data_len = frames * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, data_len as usize));
        std::fs::write(path, out).unwrap();
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        music: PathBuf,
        db: Mutex<Db>,
        artwork: ArtworkCache,
        folder: Folder,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        let db = Db::in_memory().unwrap();
        let id = db.add_folder(&music, true).unwrap();
        let artwork = ArtworkCache::new(dir.path().join("art")).unwrap();
        Fixture {
            folder: Folder {
                id,
                path: music.clone(),
                watch: true,
                added_at: now_ms(),
            },
            music,
            db: Mutex::new(db),
            artwork,
            _dir: dir,
        }
    }

    fn run(f: &Fixture) -> (usize, usize, usize) {
        scan(
            &f.db,
            &f.artwork,
            std::slice::from_ref(&f.folder),
            &Cancel::new(),
            &|_| {},
        )
        .unwrap()
    }

    fn count(f: &Fixture, sql: &str) -> i64 {
        f.db.lock()
            .unwrap()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn finds_files_and_skips_unchanged_ones_on_rescan() {
        let f = fixture();
        wav(&f.music.join("one.wav"), 1);
        wav(&f.music.join("two.wav"), 1);
        std::fs::write(f.music.join("notes.txt"), "not audio").unwrap();

        let (added, updated, removed) = run(&f);
        assert_eq!((added, updated, removed), (2, 0, 0));
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 2);

        // Second pass: nothing changed on disk, so nothing is re-read.
        let (added, updated, _) = run(&f);
        assert_eq!((added, updated), (0, 0));
    }

    #[test]
    fn scans_nested_directories() {
        let f = fixture();
        let deep = f.music.join("artist").join("album");
        std::fs::create_dir_all(&deep).unwrap();
        wav(&deep.join("track.wav"), 1);

        run(&f);
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 1);
    }

    #[test]
    fn a_deleted_file_is_marked_missing_not_dropped() {
        let f = fixture();
        let path = f.music.join("gone.wav");
        wav(&path, 1);
        run(&f);

        std::fs::remove_file(&path).unwrap();
        let (_, _, removed) = run(&f);
        assert_eq!(removed, 1);
        // The row survives, so an unmounted drive does not erase the library.
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 1);
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM tracks WHERE missing = 1"),
            1
        );

        let purged = f.db.lock().unwrap().purge_missing(None).unwrap();
        assert_eq!(purged, 1);
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 0);
    }

    #[test]
    fn a_moved_file_keeps_its_play_count() {
        let f = fixture();
        let from = f.music.join("move.wav");
        wav(&from, 2);
        run(&f);

        f.db.lock()
            .unwrap()
            .conn()
            .execute("UPDATE tracks SET play_count = 7", [])
            .unwrap();

        let into = f.music.join("sub");
        std::fs::create_dir_all(&into).unwrap();
        std::fs::rename(&from, into.join("move.wav")).unwrap();

        run(&f);
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 1);
        assert_eq!(count(&f, "SELECT play_count FROM tracks"), 7);
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM tracks WHERE missing = 1"),
            0
        );
    }

    /// Re-reads only the given paths, the way the watcher drives it.
    fn sync(f: &Fixture, touched: &[PathBuf], gone: &[PathBuf]) -> (usize, usize, usize) {
        sync_paths(&f.db, &f.artwork, touched, gone, &Cancel::new()).unwrap()
    }

    #[test]
    fn a_new_file_is_taken_in_without_rereading_the_library() {
        let f = fixture();
        wav(&f.music.join("old.wav"), 1);
        run(&f);

        let fresh = f.music.join("fresh.wav");
        wav(&fresh, 2);

        // Only the one path is handed over — the other file is never looked at.
        let (added, updated, removed) = sync(&f, &[fresh], &[]);
        assert_eq!((added, updated, removed), (1, 0, 0));
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 2);
    }

    #[test]
    fn a_deleted_file_is_marked_missing_rather_than_dropped() {
        let f = fixture();
        let path = f.music.join("bye.wav");
        wav(&path, 1);
        run(&f);

        std::fs::remove_file(&path).unwrap();
        let (_, _, removed) = sync(&f, &[], &[path]);
        assert_eq!(removed, 1);
        // Kept, not deleted: an unmounted drive must not wipe half a library.
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 1);
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM tracks WHERE missing = 1"),
            1
        );
    }

    #[test]
    fn a_whole_folder_going_away_takes_its_tracks_with_it() {
        let f = fixture();
        let sub = f.music.join("album");
        std::fs::create_dir_all(&sub).unwrap();
        wav(&sub.join("one.wav"), 1);
        wav(&sub.join("two.wav"), 2);
        run(&f);

        std::fs::remove_dir_all(&sub).unwrap();
        // The watcher reports the directory, not each file inside it.
        let (_, _, removed) = sync(&f, &[], &[sub]);
        assert_eq!(removed, 2);
    }

    #[test]
    fn a_rename_seen_by_the_watcher_keeps_the_play_count() {
        // A rename reaches us as a removal plus a creation. The old row has to be
        // marked missing before the new path is read, or the move is not spotted
        // and the track comes back as a stranger with its history gone.
        let f = fixture();
        let from = f.music.join("before.wav");
        wav(&from, 2);
        run(&f);
        f.db.lock()
            .unwrap()
            .conn()
            .execute("UPDATE tracks SET play_count = 7", [])
            .unwrap();

        let to = f.music.join("after.wav");
        std::fs::rename(&from, &to).unwrap();

        sync(&f, &[to], &[from]);
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 1);
        assert_eq!(count(&f, "SELECT play_count FROM tracks"), 7);
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM tracks WHERE missing = 1"),
            0
        );
    }

    #[test]
    fn a_removed_track_is_not_taken_back_in_by_a_sync() {
        let f = fixture();
        let path = f.music.join("unwanted.wav");
        wav(&path, 1);
        run(&f);

        // "Remove from Library" leaves the file on disk, so the watcher reports
        // it the next time anything touches it.
        let id: i64 =
            f.db.lock()
                .unwrap()
                .conn()
                .query_row("SELECT id FROM tracks", [], |row| row.get(0))
                .unwrap();
        crate::db::queries::remove_track(f.db.lock().unwrap().conn(), id).unwrap();

        let (added, _, _) = sync(&f, &[path], &[]);
        assert_eq!(added, 0);
        assert_eq!(count(&f, "SELECT COUNT(*) FROM tracks"), 0);
    }

    #[test]
    fn a_file_with_unreadable_tags_is_still_indexed() {
        let f = fixture();
        wav(&f.music.join("good.wav"), 1);
        std::fs::write(f.music.join("bad.mp3"), b"\x00\x01 definitely not audio").unwrap();

        // Both are indexed. Tag readers are stricter than decoders, so a file
        // that fails to parse here may still play perfectly; hiding it from the
        // library would lose it entirely.
        let (added, _, _) = run(&f);
        assert_eq!(added, 2);

        let untagged: (String, f64) =
            f.db.lock()
                .unwrap()
                .conn()
                .query_row(
                    "SELECT title, duration FROM tracks WHERE path LIKE '%bad.mp3'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
        // Falls back to the file name, with the duration left for playback.
        assert_eq!(untagged.0, "bad");
        assert_eq!(untagged.1, 0.0);
    }

    #[test]
    fn cancelling_stops_early_and_keeps_what_it_wrote() {
        let f = fixture();
        wav(&f.music.join("a.wav"), 1);
        let cancel = Cancel::new();
        cancel.cancel();

        let (added, _, _) = scan(
            &f.db,
            &f.artwork,
            std::slice::from_ref(&f.folder),
            &cancel,
            &|_| {},
        )
        .unwrap();
        assert_eq!(added, 0);
    }

    fn found(path: &str) -> Found {
        Found {
            path: PathBuf::from(path),
            size: 1_000,
            mtime: 1,
        }
    }

    fn tagged(album: Option<&str>, artist: Option<&str>) -> metadata::Probed {
        metadata::Probed {
            tags: crate::models::TrackTags {
                album: album.map(str::to_owned),
                artist: artist.map(str::to_owned),
                ..Default::default()
            },
            properties: Default::default(),
            codec: None,
            picture: None,
        }
    }

    /// `shareable_folders` over a batch where every file was just read.
    fn shareable(files: Vec<(Found, metadata::Probed)>) -> HashSet<PathBuf> {
        let db = Db::in_memory().unwrap();
        let batch: Vec<Found> = files
            .iter()
            .map(|(file, _)| Found {
                path: file.path.clone(),
                size: file.size,
                mtime: file.mtime,
            })
            .collect();
        let probed: Vec<(&Found, metadata::Probed)> = files
            .iter()
            .map(|(file, probed)| {
                (
                    file,
                    metadata::Probed {
                        tags: probed.tags.clone(),
                        properties: probed.properties,
                        codec: probed.codec.clone(),
                        picture: None,
                    },
                )
            })
            .collect();
        shareable_folders(db.conn(), &batch, &probed).unwrap()
    }

    #[test]
    fn an_album_folder_shares_its_cover() {
        let shared = shareable(vec![
            (
                found(r"d:\m\pf\dsotm\01.mp3"),
                tagged(Some("The Dark Side of the Moon"), Some("Pink Floyd")),
            ),
            (
                found(r"d:\m\pf\dsotm\02.mp3"),
                tagged(Some("The Dark Side of the Moon"), Some("Pink Floyd")),
            ),
        ]);
        assert!(shared.contains(Path::new(r"d:\m\pf\dsotm")));
    }

    #[test]
    fn a_folder_of_unrelated_singles_does_not() {
        // The bug this rule exists for: one cover.jpg in a downloads folder,
        // handed to two dozen artists who have nothing to do with it.
        let shared = shareable(vec![
            (
                found(r"d:\m\downloads\a.mp3"),
                tagged(Some("Superunknown"), Some("Soundgarden")),
            ),
            (
                found(r"d:\m\downloads\b.mp3"),
                tagged(Some("Rumours"), Some("Fleetwood Mac")),
            ),
        ]);
        assert!(!shared.contains(Path::new(r"d:\m\downloads")));
    }

    #[test]
    fn one_album_by_several_artists_is_still_one_album() {
        // A compilation disagrees about the artist by design, so the artist
        // alone must not veto a folder that agrees on its album.
        let shared = shareable(vec![
            (
                found(r"d:\m\comp\01.mp3"),
                tagged(Some("Now That's What I Call Music"), Some("Blur")),
            ),
            (
                found(r"d:\m\comp\02.mp3"),
                tagged(Some("Now That's What I Call Music"), Some("Oasis")),
            ),
        ]);
        assert!(shared.contains(Path::new(r"d:\m\comp")));
    }

    #[test]
    fn an_artists_own_folder_keeps_its_cover_across_their_records() {
        // One artist, several albums — a picture of them is not attributable to
        // anyone else, so refusing it would only lose art the user had.
        let shared = shareable(vec![
            (
                found(r"d:\m\Metallica\a.mp3"),
                tagged(Some("Ride the Lightning"), Some("Metallica")),
            ),
            (
                found(r"d:\m\Metallica\b.mp3"),
                tagged(Some("Master of Puppets"), Some("Metallica")),
            ),
        ]);
        assert!(shared.contains(Path::new(r"d:\m\Metallica")));
    }

    #[test]
    fn a_folder_with_nothing_tagged_still_shares() {
        // Nothing here can be attributed to the wrong artist, because nothing
        // here claims an artist at all.
        let shared = shareable(vec![
            (found(r"d:\m\rip\01.mp3"), tagged(None, None)),
            (found(r"d:\m\rip\02.mp3"), tagged(None, None)),
        ]);
        assert!(shared.contains(Path::new(r"d:\m\rip")));
    }

    #[test]
    fn one_untagged_file_does_not_rescue_a_mixed_folder() {
        let shared = shareable(vec![
            (found(r"d:\m\mixed\a.mp3"), tagged(None, None)),
            (
                found(r"d:\m\mixed\b.mp3"),
                tagged(Some("Superunknown"), Some("Soundgarden")),
            ),
            (
                found(r"d:\m\mixed\c.mp3"),
                tagged(Some("Rumours"), Some("Fleetwood Mac")),
            ),
        ]);
        assert!(!shared.contains(Path::new(r"d:\m\mixed")));
    }

    #[test]
    fn batches_never_split_a_directory() {
        // One directory holding more than a whole batch, followed by another:
        // the first batch has to run past BATCH rather than judge half a folder.
        let mut files: Vec<Found> = (0..BATCH + 40)
            .map(|index| found(&format!(r"d:\m\big\{index}.mp3")))
            .collect();
        files.push(found(r"d:\m\small\1.mp3"));

        let batches = batches(&files);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), BATCH + 40);
        assert_eq!(batches[1].len(), 1);
        for batch in &batches {
            let folders: HashSet<_> = batch.iter().map(|file| file.path.parent()).collect();
            assert_eq!(folders.len(), 1, "a directory was split across batches");
        }
    }

    #[test]
    fn a_folder_cover_still_reaches_an_album_folder() {
        let f = fixture();
        let album = f.music.join("album");
        std::fs::create_dir_all(&album).unwrap();
        wav(&album.join("01.wav"), 1);
        wav(&album.join("02.wav"), 1);

        let image = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 200]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        std::fs::write(album.join("cover.jpg"), bytes).unwrap();

        run(&f);
        assert_eq!(
            count(
                &f,
                "SELECT COUNT(*) FROM tracks WHERE artwork_id IS NOT NULL"
            ),
            2,
            "an untagged album folder lost the cover sitting in it"
        );
    }

    #[test]
    fn reports_progress_to_the_caller() {
        let f = fixture();
        wav(&f.music.join("a.wav"), 1);
        let events = Mutex::new(Vec::new());
        scan(
            &f.db,
            &f.artwork,
            std::slice::from_ref(&f.folder),
            &Cancel::new(),
            &|event| events.lock().unwrap().push(event),
        )
        .unwrap();

        let events = events.into_inner().unwrap();
        assert!(matches!(
            events.first(),
            Some(ScanEvent::Started { total: 1 })
        ));
        assert!(matches!(
            events.last(),
            Some(ScanEvent::Finished { added: 1, .. })
        ));
    }
}
