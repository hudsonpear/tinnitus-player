//! SQLite access. Owns the connection, the schema, and every statement.
//!
//! Threading: one `Db` wraps one connection and is `Send` but not `Sync`. Callers
//! keep it behind a mutex and touch it only from a background task, never from the
//! UI thread. WAL means a long read never blocks the scanner's writes.
//!
//! ponytail: single mutex-guarded connection. If scan writes ever visibly stall
//! list queries, split into a writer connection plus a read pool — the query
//! functions already take `&Connection`, so nothing above this module changes.

pub mod playlists;
pub mod queries;
mod schema;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::models::{AudioProperties, Millis, TrackId, TrackTags, now_ms};

pub struct Db {
    conn: Connection,
}

/// Everything the scanner knows about one file, ready to be written.
#[derive(Debug, Clone)]
pub struct ScannedTrack {
    pub path: PathBuf,
    pub folder_id: Option<i64>,
    pub tags: TrackTags,
    pub properties: AudioProperties,
    pub codec: Option<String>,
    pub file_size: i64,
    pub mtime: Millis,
    pub artwork_id: Option<i64>,
}

/// What `upsert_track` did, so the scanner can report accurate counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Inserted(TrackId),
    Updated(TrackId),
}

impl Upsert {
    pub fn id(self) -> TrackId {
        match self {
            Self::Inserted(id) | Self::Updated(id) => id,
        }
    }
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open database {}", path.display()))?;
        Self::prepare(conn)
    }

    /// For tests and for the "library disabled" fallback when the data directory
    /// is unwritable — the app still plays files, it just forgets them.
    pub fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory().context("cannot open in-memory database")?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("cannot enable WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        // Room for the FTS index and the hot pages of a large library.
        conn.pragma_update(None, "cache_size", -64_000i64)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        // Every database reaches this function, so the app's own smart
        // playlists exist wherever there is a library to put them in. It is a
        // no-op after the first run.
        playlists::seed_auto(&conn)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Runs `body` inside one transaction. Used by the scanner to batch writes;
    /// a batch that fails leaves the database exactly as it was.
    pub fn transaction<T>(&mut self, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let tx = self
            .conn
            .transaction()
            .context("cannot begin transaction")?;
        let out = body(&tx)?;
        tx.commit().context("cannot commit transaction")?;
        Ok(out)
    }

    // -- settings ---------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .context("cannot read setting")
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .context("cannot write setting")?;
        Ok(())
    }

    // -- folders ----------------------------------------------------------

    pub fn add_folder(&self, path: &Path, watch: bool) -> Result<i64> {
        let text = path_text(path)?;
        self.conn.execute(
            "INSERT INTO folders(path, watch, added_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET watch = excluded.watch",
            params![text, watch as i64, now_ms()],
        )?;
        self.conn
            .query_row("SELECT id FROM folders WHERE path = ?1", [&text], |row| {
                row.get(0)
            })
            .context("cannot read folder id")
    }

    pub fn remove_folder(&self, id: i64) -> Result<()> {
        // Tracks under the folder go with it; the files themselves are untouched.
        self.conn
            .execute("DELETE FROM tracks WHERE folder_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM folders WHERE id = ?1", [id])?;
        Ok(())
    }

    // -- interning --------------------------------------------------------

    /// Returns the id of a row in a `(id, name UNIQUE)` table, creating it if needed.
    fn intern(conn: &Connection, table: &'static str, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("cannot intern an empty name into {table}");
        }
        // `table` is a compile-time constant chosen by us, never user input.
        conn.execute(
            &format!("INSERT OR IGNORE INTO {table}(name) VALUES (?1)"),
            [name],
        )?;
        conn.query_row(
            &format!("SELECT id FROM {table} WHERE name = ?1 COLLATE NOCASE"),
            [name],
            |row| row.get(0),
        )
        .with_context(|| format!("cannot read id from {table}"))
    }

    pub fn intern_artist(conn: &Connection, name: &str) -> Result<i64> {
        Self::intern(conn, "artists", name)
    }

    pub fn intern_genre(conn: &Connection, name: &str) -> Result<i64> {
        Self::intern(conn, "genres", name)
    }

    /// Albums are keyed by `(name, album_artist)` so two different artists can
    /// each have a "Greatest Hits" without colliding.
    pub fn intern_album(
        conn: &Connection,
        name: &str,
        album_artist: Option<&str>,
        artist_id: Option<i64>,
        year: Option<i32>,
        artwork_id: Option<i64>,
    ) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("cannot intern an album with no name");
        }
        conn.execute(
            "INSERT INTO albums(name, album_artist, artist_id, year, artwork_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name, album_artist) DO UPDATE SET
                 year       = COALESCE(albums.year, excluded.year),
                 artwork_id = COALESCE(albums.artwork_id, excluded.artwork_id),
                 artist_id  = COALESCE(albums.artist_id, excluded.artist_id)",
            params![name, album_artist, artist_id, year, artwork_id, now_ms()],
        )?;
        conn.query_row(
            "SELECT id FROM albums WHERE name = ?1 AND album_artist IS ?2",
            params![name, album_artist],
            |row| row.get(0),
        )
        .context("cannot read album id")
    }

    // -- tracks -----------------------------------------------------------

    /// Insert or update one track by path. Preserves user data (play count,
    /// rating, favorite, date added) across re-scans.
    pub fn upsert_track(conn: &Connection, track: &ScannedTrack) -> Result<Upsert> {
        let path = path_text(&track.path)?;
        let tags = &track.tags;

        let artist_name = tags.artist.as_deref().unwrap_or("").trim();
        let artist_id = match artist_name.is_empty() {
            true => None,
            false => Some(Self::intern_artist(conn, artist_name)?),
        };
        let genre_id = match tags
            .genre
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
        {
            Some(genre) => Some(Self::intern_genre(conn, genre)?),
            None => None,
        };
        let album_artist = tags
            .album_artist
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let album_id = match tags
            .album
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            Some(album) => {
                let album_artist_id = match album_artist {
                    Some(name) => Some(Self::intern_artist(conn, name)?),
                    None => artist_id,
                };
                Some(Self::intern_album(
                    conn,
                    album,
                    album_artist.or(Some(artist_name)).filter(|a| !a.is_empty()),
                    album_artist_id,
                    tags.year,
                    track.artwork_id,
                )?)
            }
            None => None,
        };

        let title = tags
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| file_stem(&track.path));

        let existing: Option<TrackId> = conn
            .query_row("SELECT id FROM tracks WHERE path = ?1", [&path], |row| {
                row.get(0)
            })
            .optional()?;

        let rg = tags.replay_gain;
        let properties = track.properties;

        if let Some(id) = existing {
            conn.execute(
                "UPDATE tracks SET
                     folder_id = ?2, title = ?3, artist = ?4, album_artist = ?5,
                     album_id = ?6, artist_id = ?7, genre_id = ?8, year = ?9,
                     track_number = ?10, disc_number = ?11, composer = ?12, comment = ?13,
                     bpm = ?14, duration = ?15, bitrate = ?16, sample_rate = ?17,
                     channels = ?18, codec = ?19, file_size = ?20, mtime = ?21,
                     rg_track_gain = ?22, rg_track_peak = ?23, rg_album_gain = ?24,
                     -- Assigned outright rather than kept when the new value is
                     -- null: the scanner works out the art for a file every time
                     -- it re-reads it, so holding on to the old id would keep a
                     -- cover the file no longer has any claim to.
                     rg_album_peak = ?25, artwork_id = ?26, missing = 0
                 WHERE id = ?1",
                params![
                    id,
                    track.folder_id,
                    title,
                    artist_name,
                    album_artist,
                    album_id,
                    artist_id,
                    genre_id,
                    tags.year,
                    tags.track_number,
                    tags.disc_number,
                    tags.composer,
                    tags.comment,
                    tags.bpm,
                    properties.duration,
                    properties.bitrate,
                    properties.sample_rate,
                    properties.channels,
                    track.codec,
                    track.file_size,
                    track.mtime,
                    rg.track_gain,
                    rg.track_peak,
                    rg.album_gain,
                    rg.album_peak,
                    track.artwork_id,
                ],
            )?;
            return Ok(Upsert::Updated(id));
        }

        conn.execute(
            "INSERT INTO tracks(
                 path, folder_id, title, artist, album_artist, album_id, artist_id,
                 genre_id, year, track_number, disc_number, composer, comment, bpm,
                 duration, bitrate, sample_rate, channels, codec, file_size, mtime,
                 rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak,
                 date_added, artwork_id
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
             )",
            params![
                path,
                track.folder_id,
                title,
                artist_name,
                album_artist,
                album_id,
                artist_id,
                genre_id,
                tags.year,
                tags.track_number,
                tags.disc_number,
                tags.composer,
                tags.comment,
                tags.bpm,
                properties.duration,
                properties.bitrate,
                properties.sample_rate,
                properties.channels,
                track.codec,
                track.file_size,
                track.mtime,
                rg.track_gain,
                rg.track_peak,
                rg.album_gain,
                rg.album_peak,
                now_ms(),
                track.artwork_id,
            ],
        )?;
        Ok(Upsert::Inserted(conn.last_insert_rowid()))
    }

    /// True when the row for this path already matches the file on disk, so the
    /// scanner can skip re-reading its tags.
    pub fn is_unchanged(conn: &Connection, path: &Path, size: i64, mtime: Millis) -> Result<bool> {
        let text = path_text(path)?;
        let row: Option<(i64, i64, i64)> = conn
            .query_row(
                "SELECT file_size, mtime, missing FROM tracks WHERE path = ?1",
                [&text],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(matches!(row, Some((s, m, 0)) if s == size && m == mtime))
    }

    /// Moves a row to a new path, keeping its id and all user data.
    pub fn relocate_track(conn: &Connection, id: TrackId, path: &Path) -> Result<()> {
        conn.execute(
            "UPDATE tracks SET path = ?2, missing = 0 WHERE id = ?1",
            params![id, path_text(path)?],
        )?;
        Ok(())
    }

    /// The row id for a path, if the library already knows this file.
    pub fn id_for_path(conn: &Connection, path: &Path) -> Result<Option<TrackId>> {
        conn.query_row(
            "SELECT id FROM tracks WHERE path = ?1",
            [path_text(path)?],
            |row| row.get(0),
        )
        .optional()
        .context("cannot look up a track by path")
    }

    /// A track that vanished but whose size and duration match one that appeared
    /// is the same track, moved. Returns the row to relocate, if any.
    ///
    /// Only an unambiguous match counts. Libraries are full of the same file
    /// kept in two places, and those rows are identical in size and duration —
    /// picking between them arbitrarily would hand a track someone else's row,
    /// along with its play count and its cover.
    pub fn find_moved(conn: &Connection, size: i64, duration: f64) -> Result<Option<TrackId>> {
        let mut statement = conn.prepare_cached(
            "SELECT id FROM tracks
             WHERE missing = 1 AND file_size = ?1 AND ABS(duration - ?2) < 0.05
             LIMIT 2",
        )?;
        let candidates = statement
            .query_map(params![size, duration], |row| row.get::<_, TrackId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("cannot look for a moved track")?;

        Ok(match candidates.as_slice() {
            [only] => Some(*only),
            _ => None,
        })
    }

    /// Removes rows that have been `missing` since before `older_than`. Called on
    /// an explicit clean-up, never automatically right after a scan — an unmounted
    /// drive must not erase its half of the library.
    pub fn purge_missing(&self, folder_id: Option<i64>) -> Result<usize> {
        let removed = match folder_id {
            Some(id) => self.conn.execute(
                "DELETE FROM tracks WHERE missing = 1 AND folder_id = ?1",
                [id],
            )?,
            None => self
                .conn
                .execute("DELETE FROM tracks WHERE missing = 1", [])?,
        };
        Ok(removed)
    }

    pub fn mark_missing(conn: &Connection, ids: &[TrackId]) -> Result<()> {
        let mut statement = conn.prepare("UPDATE tracks SET missing = 1 WHERE id = ?1")?;
        for id in ids {
            statement.execute([id])?;
        }
        Ok(())
    }

    /// Drops albums, artists and genres nothing points at any more.
    pub fn prune_orphans(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM albums  WHERE id NOT IN (SELECT album_id  FROM tracks WHERE album_id  IS NOT NULL);
             DELETE FROM artists WHERE id NOT IN (SELECT artist_id FROM tracks WHERE artist_id IS NOT NULL)
                               AND id NOT IN (SELECT artist_id FROM albums WHERE artist_id IS NOT NULL);
             DELETE FROM genres  WHERE id NOT IN (SELECT genre_id  FROM tracks WHERE genre_id  IS NOT NULL);
             DELETE FROM artwork WHERE id NOT IN (SELECT artwork_id FROM tracks WHERE artwork_id IS NOT NULL)
                               AND id NOT IN (SELECT artwork_id FROM albums WHERE artwork_id IS NOT NULL);",
        )?;
        Ok(())
    }
}

/// Paths are stored as text. A path that is not valid Unicode cannot round-trip,
/// so it is refused at the door rather than silently mangled by a lossy convert.
pub fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("path is not valid Unicode: {}", path.display()))
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ReplayGainTags;

    fn sample(path: &str, title: &str) -> ScannedTrack {
        ScannedTrack {
            path: PathBuf::from(path),
            folder_id: None,
            tags: TrackTags {
                title: Some(title.to_owned()),
                artist: Some("Some Artist".to_owned()),
                album: Some("Some Album".to_owned()),
                genre: Some("Rock".to_owned()),
                year: Some(1994),
                track_number: Some(3),
                replay_gain: ReplayGainTags {
                    track_gain: Some(-3.2),
                    ..Default::default()
                },
                ..Default::default()
            },
            properties: AudioProperties {
                duration: 212.5,
                bitrate: Some(320),
                sample_rate: Some(44100),
                channels: Some(2),
            },
            codec: Some("mp3".to_owned()),
            file_size: 8_500_000,
            mtime: 1_738_368_000_000,
            artwork_id: None,
        }
    }

    #[test]
    fn migrates_and_upserts() {
        let mut db = Db::in_memory().unwrap();
        let track = sample(r"d:\music\artist\album\01 one.mp3", "One");

        let first = db
            .transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap();
        assert!(matches!(first, Upsert::Inserted(_)));

        // A re-scan of the same path updates rather than duplicating.
        let second = db
            .transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap();
        assert_eq!(second.id(), first.id());
        assert!(matches!(second, Upsert::Updated(_)));

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn user_data_survives_a_rescan() {
        let mut db = Db::in_memory().unwrap();
        let track = sample(r"d:\music\a\b\keep.mp3", "Keep");
        let id = db
            .transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap()
            .id();

        db.conn()
            .execute(
                "UPDATE tracks SET play_count = 12, rating = 4, favorite = 1 WHERE id = ?1",
                [id],
            )
            .unwrap();
        db.transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap();

        let (plays, rating, favorite): (i64, i64, i64) = db
            .conn()
            .query_row(
                "SELECT play_count, rating, favorite FROM tracks WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((plays, rating, favorite), (12, 4, 1));
    }

    #[test]
    fn unchanged_detects_matching_size_and_mtime() {
        let mut db = Db::in_memory().unwrap();
        let track = sample(r"d:\music\a\b\same.mp3", "Same");
        db.transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap();

        let conn = db.conn();
        assert!(Db::is_unchanged(conn, &track.path, track.file_size, track.mtime).unwrap());
        assert!(!Db::is_unchanged(conn, &track.path, track.file_size + 1, track.mtime).unwrap());
        assert!(!Db::is_unchanged(conn, &track.path, track.file_size, track.mtime + 1).unwrap());
    }

    #[test]
    fn a_moved_file_keeps_its_row() {
        let mut db = Db::in_memory().unwrap();
        let track = sample(r"d:\music\old\move me.mp3", "Move Me");
        let id = db
            .transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap()
            .id();

        Db::mark_missing(db.conn(), &[id]).unwrap();
        let found = Db::find_moved(db.conn(), track.file_size, track.properties.duration).unwrap();
        assert_eq!(found, Some(id));

        let moved = PathBuf::from(r"d:\music\new\move me.mp3");
        Db::relocate_track(db.conn(), id, &moved).unwrap();

        let (path, missing): (String, i64) = db
            .conn()
            .query_row(
                "SELECT path, missing FROM tracks WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, path_text(&moved).unwrap());
        assert_eq!(missing, 0);
    }

    #[test]
    fn rejects_a_database_from_the_future() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99i64).unwrap();
        assert!(schema::migrate(&conn).is_err());
    }
}
