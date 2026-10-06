//! Versioned schema. `PRAGMA user_version` is the version counter; every entry
//! in `MIGRATIONS` moves it forward by one and is applied inside a transaction.
//!
//! Never edit an existing migration once it has shipped — append a new one.

use anyhow::{Context as _, Result};
use rusqlite::Connection;

/// Applied in order. Index + 1 is the resulting `user_version`.
const MIGRATIONS: &[&str] = &[INITIAL, REREAD_FOLDER_ART, EXCLUDED_PATHS, TRACK_AUDIO];

pub fn migrate(conn: &Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("cannot read schema version")?;
    let current = current.max(0) as usize;

    if current > MIGRATIONS.len() {
        anyhow::bail!(
            "database schema is version {current}, newer than this build understands ({})",
            MIGRATIONS.len()
        );
    }

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let version = index + 1;
        conn.execute_batch(&format!(
            "BEGIN;\n{sql}\nPRAGMA user_version = {version};\nCOMMIT;"
        ))
        .with_context(|| format!("migration {version} failed"))?;
        log::info!("library: applied schema migration {version}");
    }

    Ok(())
}

const INITIAL: &str = r#"
CREATE TABLE artists (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE COLLATE NOCASE
);

CREATE TABLE genres (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE COLLATE NOCASE
);

CREATE TABLE artwork (
    id      INTEGER PRIMARY KEY,
    hash    TEXT NOT NULL UNIQUE,
    mime    TEXT,
    width   INTEGER,
    height  INTEGER,
    -- 'embedded' | 'folder'
    source  TEXT NOT NULL DEFAULT 'embedded'
);

CREATE TABLE folders (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    watch       INTEGER NOT NULL DEFAULT 1,
    added_at    INTEGER NOT NULL
);

CREATE TABLE albums (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL COLLATE NOCASE,
    album_artist TEXT COLLATE NOCASE,
    artist_id    INTEGER REFERENCES artists(id) ON DELETE SET NULL,
    year         INTEGER,
    artwork_id   INTEGER REFERENCES artwork(id) ON DELETE SET NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE(name, album_artist)
);

CREATE TABLE tracks (
    id             INTEGER PRIMARY KEY,
    path           TEXT NOT NULL UNIQUE COLLATE NOCASE,
    folder_id      INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    title          TEXT NOT NULL,
    artist         TEXT NOT NULL DEFAULT '',
    album_artist   TEXT,
    album_id       INTEGER REFERENCES albums(id) ON DELETE SET NULL,
    artist_id      INTEGER REFERENCES artists(id) ON DELETE SET NULL,
    genre_id       INTEGER REFERENCES genres(id) ON DELETE SET NULL,
    year           INTEGER,
    track_number   INTEGER,
    disc_number    INTEGER,
    composer       TEXT,
    comment        TEXT,
    bpm            INTEGER,
    duration       REAL NOT NULL DEFAULT 0,
    bitrate        INTEGER,
    sample_rate    INTEGER,
    channels       INTEGER,
    codec          TEXT,
    file_size      INTEGER NOT NULL DEFAULT 0,
    mtime          INTEGER NOT NULL DEFAULT 0,
    rg_track_gain  REAL,
    rg_track_peak  REAL,
    rg_album_gain  REAL,
    rg_album_peak  REAL,
    date_added     INTEGER NOT NULL,
    last_played    INTEGER,
    play_count     INTEGER NOT NULL DEFAULT 0,
    rating         INTEGER NOT NULL DEFAULT 0,
    favorite       INTEGER NOT NULL DEFAULT 0,
    artwork_id     INTEGER REFERENCES artwork(id) ON DELETE SET NULL,
    missing        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX tracks_album      ON tracks(album_id);
CREATE INDEX tracks_artist     ON tracks(artist_id);
CREATE INDEX tracks_genre      ON tracks(genre_id);
CREATE INDEX tracks_folder     ON tracks(folder_id);
CREATE INDEX tracks_added      ON tracks(date_added DESC);
CREATE INDEX tracks_played     ON tracks(last_played DESC);
CREATE INDEX tracks_count      ON tracks(play_count DESC);
CREATE INDEX tracks_favorite   ON tracks(favorite) WHERE favorite = 1;
CREATE INDEX tracks_missing    ON tracks(missing) WHERE missing = 1;
CREATE INDEX tracks_year       ON tracks(year);
CREATE INDEX tracks_title      ON tracks(title COLLATE NOCASE);
-- Move detection looks a vanished file up by its shape rather than its path.
CREATE INDEX tracks_shape      ON tracks(file_size, duration);

CREATE TABLE playlists (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'custom',
    rules        TEXT,
    position     INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    modified_at  INTEGER NOT NULL
);

CREATE TABLE playlist_tracks (
    playlist_id  INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id     INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    added_at     INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, position)
);

CREATE INDEX playlist_tracks_track ON playlist_tracks(track_id);

CREATE TABLE play_history (
    id           INTEGER PRIMARY KEY,
    track_id     INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    played_at    INTEGER NOT NULL,
    listened_ms  INTEGER NOT NULL DEFAULT 0,
    completion   REAL NOT NULL DEFAULT 0
);

CREATE INDEX play_history_at    ON play_history(played_at DESC);
CREATE INDEX play_history_track ON play_history(track_id);

CREATE TABLE settings (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);

-- Search index, kept in step with `tracks` by the triggers below so a rebuild is
-- never needed after a scan.
--
-- This is a plain (not contentless, not external-content) fts5 table: it stores
-- its own copy of the four indexed columns. That costs a little disk, and buys
-- deletes by rowid, which a contentless table cannot do without replaying the
-- exact original column values.
CREATE VIRTUAL TABLE tracks_fts USING fts5(
    title,
    artist,
    album,
    genre,
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER tracks_fts_insert AFTER INSERT ON tracks BEGIN
    INSERT INTO tracks_fts(rowid, title, artist, album, genre)
    VALUES (
        new.id,
        new.title,
        new.artist,
        COALESCE((SELECT name FROM albums WHERE id = new.album_id), ''),
        COALESCE((SELECT name FROM genres WHERE id = new.genre_id), '')
    );
END;

CREATE TRIGGER tracks_fts_delete AFTER DELETE ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = old.id;
END;

CREATE TRIGGER tracks_fts_update AFTER UPDATE OF title, artist, album_id, genre_id ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = old.id;
    INSERT INTO tracks_fts(rowid, title, artist, album, genre)
    VALUES (
        new.id,
        new.title,
        new.artist,
        COALESCE((SELECT name FROM albums WHERE id = new.album_id), ''),
        COALESCE((SELECT name FROM genres WHERE id = new.genre_id), '')
    );
END;

INSERT INTO playlists (name, kind, position, created_at, modified_at)
VALUES ('Favorites', 'favorites', 0, 0, 0);
"#;

/// Folder art used to go on any file that happened to sit beside a cover image,
/// which put one album's cover on every unrelated single in a downloads folder.
/// The scanner now shares a folder's art only between files that agree on what
/// they are, so every row that got its art the old way gives it up and is read
/// again.
///
/// Zeroing `mtime` is what forces that re-read: the scanner skips a file whose
/// size and mtime still match its row, and would otherwise never look at these
/// again. The next scan puts the real mtime back.
///
/// Nothing is deleted. Play counts, ratings, favourites and history are not
/// touched, and the artwork rows themselves are left for `prune_orphans` so a
/// file that still has a legitimate claim to one keeps it.
/// Files the user took out of the library on purpose.
///
/// "Remove from Library" deletes the row, but the file is still sitting in a
/// watched folder — so the next scan found it again and put it straight back.
/// A scan skips anything listed here, and adding the folder again is the signal
/// that the user wants its contents indexed after all, which clears them.
const EXCLUDED_PATHS: &str = r#"
CREATE TABLE excluded (
    path        TEXT PRIMARY KEY COLLATE NOCASE,
    excluded_at INTEGER NOT NULL
);
"#;

/// A volume and an equalizer curve remembered per song.
///
/// Its own table rather than columns on `tracks`: the scanner rewrites track
/// rows wholesale and has no business knowing this exists. `ON DELETE CASCADE`
/// means forgetting a track forgets its settings with it.
///
/// Both value columns are nullable — a song can have a remembered volume and no
/// curve, or the other way round.
const TRACK_AUDIO: &str = r#"
CREATE TABLE track_audio (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    volume   REAL,
    eq       TEXT,
    saved_at INTEGER NOT NULL
);
"#;

const REREAD_FOLDER_ART: &str = r#"
UPDATE tracks SET artwork_id = NULL, mtime = 0
WHERE artwork_id IN (SELECT id FROM artwork WHERE source = 'folder');

UPDATE albums SET artwork_id = NULL
WHERE artwork_id IN (SELECT id FROM artwork WHERE source = 'folder');
"#;
