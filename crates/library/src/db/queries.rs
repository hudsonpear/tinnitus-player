//! Read side. Every list query is paged: the UI asks for a window of rows around
//! the viewport, never for the whole library.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use rusqlite::{Connection, Row, ToSql, types::Value};

use crate::models::{
    Album, AlbumId, Artist, Folder, LibraryView, PlaylistKind, PlaylistRow, ReplayGainTags,
    SearchResults, Sort, Track, TrackId,
};

/// Columns and joins shared by every query that returns a `Track`.
const TRACK_SELECT: &str = "
SELECT t.id, t.path, t.title, t.artist, t.album_artist, al.name, t.album_id, t.artist_id,
       g.name, t.year, t.track_number, t.disc_number, t.composer, t.comment, t.bpm,
       t.duration, t.bitrate, t.sample_rate, t.channels, t.codec, t.file_size,
       t.rg_track_gain, t.rg_track_peak, t.rg_album_gain, t.rg_album_peak,
       t.date_added, t.last_played, t.play_count, t.rating, t.favorite, t.artwork_id, t.missing
FROM tracks t
LEFT JOIN albums al ON al.id = t.album_id
LEFT JOIN genres g  ON g.id = t.genre_id
";

/// Borrows a list of values as bound parameters.
fn bind(values: &[Value]) -> Vec<&dyn ToSql> {
    values.iter().map(|value| value as &dyn ToSql).collect()
}

fn read_track(row: &Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        title: row.get(2)?,
        artist: row.get(3)?,
        album_artist: row.get(4)?,
        album: row.get(5)?,
        album_id: row.get(6)?,
        artist_id: row.get(7)?,
        genre: row.get(8)?,
        year: row.get(9)?,
        track_number: row.get(10)?,
        disc_number: row.get(11)?,
        composer: row.get(12)?,
        comment: row.get(13)?,
        bpm: row.get(14)?,
        duration: row.get(15)?,
        bitrate: row.get(16)?,
        sample_rate: row.get(17)?,
        channels: row.get(18)?,
        codec: row.get(19)?,
        file_size: row.get(20)?,
        replay_gain: ReplayGainTags {
            track_gain: row.get(21)?,
            track_peak: row.get(22)?,
            album_gain: row.get(23)?,
            album_peak: row.get(24)?,
        },
        date_added: row.get(25)?,
        last_played: row.get(26)?,
        play_count: row.get(27)?,
        rating: row.get(28)?,
        favorite: row.get::<_, i64>(29)? != 0,
        artwork_id: row.get(30)?,
        missing: row.get::<_, i64>(31)? != 0,
    })
}

/// The `WHERE` fragment and its bound values for one library view. The fragment
/// is built from constants only; every value the user supplied is a bound
/// parameter, so nothing here can be injected into.
///
/// It takes a connection because one view cannot be answered without the
/// database: a playlist may be a hand-made list or a set of rules, and which
/// one it is lives in its row. That is also why it is fallible.
fn filter(conn: &Connection, view: &LibraryView) -> Result<(String, Vec<Value>)> {
    if let LibraryView::Playlist(id) = view {
        return playlist_filter(conn, *id);
    }
    Ok(plain_filter(view))
}

/// The filter for every view that does not have to ask the database anything.
fn plain_filter(view: &LibraryView) -> (String, Vec<Value>) {
    match view {
        LibraryView::AllSongs
        | LibraryView::Albums
        | LibraryView::Artists
        | LibraryView::Genres
        | LibraryView::Years
        | LibraryView::Folders => ("t.missing = 0".to_owned(), vec![]),
        LibraryView::RecentlyAdded => ("t.missing = 0".to_owned(), vec![]),
        LibraryView::RecentlyPlayed => (
            "t.missing = 0 AND t.last_played IS NOT NULL".to_owned(),
            vec![],
        ),
        LibraryView::MostPlayed => ("t.missing = 0 AND t.play_count > 0".to_owned(), vec![]),
        LibraryView::Favorites => ("t.missing = 0 AND t.favorite = 1".to_owned(), vec![]),
        LibraryView::Album(id) => ("t.album_id = ?".to_owned(), vec![Value::Integer(*id)]),
        LibraryView::Artist(id) => (
            "t.missing = 0 AND (t.artist_id = ? OR al.artist_id = ?)".to_owned(),
            vec![Value::Integer(*id), Value::Integer(*id)],
        ),
        LibraryView::Genre(name) => (
            "t.missing = 0 AND g.name = ? COLLATE NOCASE".to_owned(),
            vec![Value::Text(name.clone())],
        ),
        LibraryView::Year(year) => (
            "t.missing = 0 AND t.year = ?".to_owned(),
            vec![Value::Integer(*year as i64)],
        ),
        LibraryView::Folder(id) => (
            "t.missing = 0 AND t.folder_id = ?".to_owned(),
            vec![Value::Integer(*id)],
        ),
        // Answered by `playlist_filter`, which has the connection it needs to
        // tell a hand-made list from a set of rules.
        LibraryView::Playlist(id) => (
            "t.id IN (SELECT track_id FROM playlist_tracks WHERE playlist_id = ?)".to_owned(),
            vec![Value::Integer(*id)],
        ),
    }
}

/// A playlist's filter: the stored track list, or the compiled rules.
///
/// One branch point, keyed off the row's `kind`. There is no new `LibraryView`
/// variant and no new navigation path — everything that already knows how to
/// open a playlist keeps working, and a custom playlist turned smart changes
/// what it shows without anything upstream noticing.
fn playlist_filter(conn: &Connection, id: i64) -> Result<(String, Vec<Value>)> {
    use rusqlite::OptionalExtension as _;

    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT kind, rules FROM playlists WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("cannot read a playlist")?;

    let Some((kind, rules)) = row else {
        // A playlist that is no longer there shows nothing rather than
        // everything, which is what an empty clause would have meant.
        return Ok(("0".to_owned(), vec![]));
    };
    if PlaylistKind::parse(&kind) != PlaylistKind::Smart {
        return Ok(plain_filter(&LibraryView::Playlist(id)));
    }

    let rules = crate::smart::SmartRules::parse(rules.as_deref());
    let (clause, mut values) = crate::smart::compile(&rules);

    let Some(limit) = rules.limit else {
        return Ok((format!("t.missing = 0 AND {clause}"), values));
    };

    // A capped rule set is the same question asked inside an `IN`, so the
    // `LIMIT` has somewhere to go. The inner aliases shadow the outer ones, so
    // the subquery is not correlated and the compiled clause needs no rewriting
    // — but the values are bound twice, once for each copy.
    let inner = values.clone();
    values.extend(inner);
    values.push(Value::Integer(i64::from(limit)));
    Ok((
        format!(
            "t.missing = 0 AND {clause} AND t.id IN (
                 SELECT t.id FROM tracks t
                 LEFT JOIN albums al ON al.id = t.album_id
                 LEFT JOIN genres g  ON g.id = t.genre_id
                 WHERE t.missing = 0 AND {clause}
                 ORDER BY t.date_added DESC, t.id DESC
                 LIMIT ?)"
        ),
        values,
    ))
}

/// Views with a natural order of their own ignore the column sort.
///
/// `smart` says whether a playlist view is a smart one, which changes the
/// answer: a hand-made list has an order the user arranged, and a smart one has
/// no stored positions to read.
fn implied_sort(view: &LibraryView, smart: bool) -> Option<&'static str> {
    match view {
        LibraryView::RecentlyAdded => Some("t.date_added DESC"),
        LibraryView::RecentlyPlayed => Some("t.last_played DESC"),
        LibraryView::MostPlayed => Some("t.play_count DESC, t.last_played DESC"),
        LibraryView::Album(_) => Some("t.disc_number, t.track_number, t.title COLLATE NOCASE"),
        // A smart playlist has no `playlist_tracks` rows, so the position
        // subquery would sort every track by NULL. It falls through to the
        // column sort instead, like any other list the user did not arrange.
        LibraryView::Playlist(_) if smart => None,
        LibraryView::Playlist(_) => Some(
            "(SELECT position FROM playlist_tracks pt \
              WHERE pt.track_id = t.id AND pt.playlist_id = ?)",
        ),
        _ => None,
    }
}

/// True when this view is a smart playlist. Cheap: a primary-key lookup, and
/// only for a playlist view at all.
fn is_smart(conn: &Connection, view: &LibraryView) -> Result<bool> {
    use rusqlite::OptionalExtension as _;
    let LibraryView::Playlist(id) = view else {
        return Ok(false);
    };
    let kind: Option<String> = conn
        .query_row("SELECT kind FROM playlists WHERE id = ?1", [*id], |row| {
            row.get(0)
        })
        .optional()
        .context("cannot read a playlist")?;
    Ok(kind.is_some_and(|kind| PlaylistKind::parse(&kind) == PlaylistKind::Smart))
}

pub fn count_tracks(conn: &Connection, view: &LibraryView) -> Result<usize> {
    let (where_clause, values) = filter(conn, view)?;
    let sql = format!(
        "SELECT COUNT(*) FROM tracks t
         LEFT JOIN albums al ON al.id = t.album_id
         LEFT JOIN genres g  ON g.id = t.genre_id
         WHERE {where_clause}"
    );
    let params: Vec<&dyn ToSql> = values.iter().map(|v| v as &dyn ToSql).collect();
    let count: i64 = conn
        .query_row(&sql, params.as_slice(), |row| row.get(0))
        .context("cannot count tracks")?;
    Ok(count as usize)
}

/// One window of a track list. `offset`/`limit` come straight from the virtual
/// list's visible range.
pub fn tracks_page(
    conn: &Connection,
    view: &LibraryView,
    sort: Sort,
    offset: usize,
    limit: usize,
) -> Result<Vec<Track>> {
    let (where_clause, mut values) = filter(conn, view)?;
    let order = match implied_sort(view, is_smart(conn, view)?) {
        Some(order) => {
            // The playlist ordering repeats the playlist id inside ORDER BY.
            if let LibraryView::Playlist(id) = view {
                values.push(Value::Integer(*id));
            }
            order.to_owned()
        }
        None => format!(
            "{} {}",
            sort.key.column(),
            if sort.descending { "DESC" } else { "ASC" }
        ),
    };

    let sql = format!("{TRACK_SELECT} WHERE {where_clause} ORDER BY {order} LIMIT ? OFFSET ?");
    values.push(Value::Integer(limit as i64));
    values.push(Value::Integer(offset as i64));

    let params: Vec<&dyn ToSql> = values.iter().map(|v| v as &dyn ToSql).collect();
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement
        .query_map(params.as_slice(), read_track)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read a page of tracks")?;
    Ok(rows)
}

/// Every track id in a view, in display order. Used when the user hits play on a
/// view: the queue holds ids, not rows, so even a huge selection stays cheap.
pub fn track_ids(conn: &Connection, view: &LibraryView, sort: Sort) -> Result<Vec<TrackId>> {
    let (where_clause, mut values) = filter(conn, view)?;
    let order = match implied_sort(view, is_smart(conn, view)?) {
        Some(order) => {
            if let LibraryView::Playlist(id) = view {
                values.push(Value::Integer(*id));
            }
            order.to_owned()
        }
        None => format!(
            "{} {}",
            sort.key.column(),
            if sort.descending { "DESC" } else { "ASC" }
        ),
    };
    let sql = format!(
        "SELECT t.id FROM tracks t
         LEFT JOIN albums al ON al.id = t.album_id
         LEFT JOIN genres g  ON g.id = t.genre_id
         WHERE {where_clause} ORDER BY {order}"
    );
    let params: Vec<&dyn ToSql> = values.iter().map(|v| v as &dyn ToSql).collect();
    let mut statement = conn.prepare(&sql)?;
    let ids = statement
        .query_map(params.as_slice(), |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read track ids")?;
    Ok(ids)
}

/// The row for a path, if the library knows this file. Used when something
/// outside the library hands us a file — a command-line argument, a drop on the
/// window — so an already-indexed track plays as itself, with its history and
/// rating, rather than as an anonymous file.
pub fn track_by_path(conn: &Connection, path: &std::path::Path) -> Result<Option<Track>> {
    let text = crate::db::path_text(path)?;
    let sql = format!("{TRACK_SELECT} WHERE t.path = ?1");
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query_map([text], read_track)?;
    Ok(rows.next().transpose()?)
}

pub fn track(conn: &Connection, id: TrackId) -> Result<Option<Track>> {
    let sql = format!("{TRACK_SELECT} WHERE t.id = ?1");
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query_map([id], read_track)?;
    Ok(rows.next().transpose()?)
}

/// Reads many tracks by id, preserving the caller's order. The queue uses this to
/// turn its id list into rows for display.
pub fn tracks_by_id(conn: &Connection, ids: &[TrackId]) -> Result<Vec<Track>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("{TRACK_SELECT} WHERE t.id IN ({placeholders})");
    let params: Vec<&dyn ToSql> = ids.iter().map(|id| id as &dyn ToSql).collect();
    let mut statement = conn.prepare(&sql)?;
    let found = statement
        .query_map(params.as_slice(), read_track)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut by_id: std::collections::HashMap<TrackId, Track> =
        found.into_iter().map(|track| (track.id, track)).collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// How much each signal is worth when looking for tracks like another one.
///
/// One table, interpolated into the SQL below as constants — there is no second
/// copy of these numbers anywhere, so the ranking cannot drift out of step with
/// what this says. The order is the claim being made: who made it beats what the
/// user filed it beside, which beats what a tagger called it.
mod weight {
    /// The same artist, by id or by album-artist name. By far the strongest.
    pub const ARTIST: i64 = 100;
    /// Sitting in a playlist with the seed: the user's own curation, and a
    /// better signal than a genre tag somebody else wrote.
    pub const PLAYLIST: i64 = 40;
    pub const GENRE: i64 = 25;
    pub const ALBUM: i64 = 20;
    /// Released within `YEARS` of the seed.
    pub const YEAR: i64 = 10;
    pub const YEARS: i64 = 3;
    /// Within `BPM_TOLERANCE` of the seed's tempo, when both are tagged.
    pub const BPM: i64 = 8;
    pub const BPM_TOLERANCE: f64 = 0.10;
}

/// Tracks that resemble `seed`, best first.
///
/// Everything it scores on is a column that already exists and is already
/// indexed. Candidates must match at least one of the strong signals, so this
/// is never a scan of the whole library dressed up as a ranking.
///
/// The caller asks for more than it wants on purpose: the queue lives in memory
/// rather than in the database, so "not already queued" cannot be a `WHERE`
/// clause without binding the whole queue as parameters — fifty thousand of them
/// after a Shuffle All. Over-fetching and filtering in Rust is the cheaper half
/// of that trade.
pub fn similar_to(conn: &Connection, seed: TrackId, limit: usize) -> Result<Vec<TrackId>> {
    use weight::*;

    // Every value is bound; the only things formatted into the statement are
    // the integer constants above, which is the discipline the rest of this
    // module follows.
    let sql = format!(
        "WITH seed AS (SELECT t.id, t.artist_id, t.album_id, t.genre_id, t.year, t.bpm,
                              COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist) AS who
                         FROM tracks t WHERE t.id = ?1)
         SELECT t.id
           FROM tracks t, seed s
          WHERE t.id <> s.id
            AND t.missing = 0
            AND (
                 (t.artist_id IS NOT NULL AND t.artist_id = s.artist_id)
              OR COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist) = s.who COLLATE NOCASE
              OR (t.genre_id IS NOT NULL AND t.genre_id = s.genre_id)
              OR (t.album_id IS NOT NULL AND t.album_id = s.album_id)
              OR EXISTS (SELECT 1 FROM playlist_tracks a
                         JOIN playlist_tracks b ON b.playlist_id = a.playlist_id
                          WHERE a.track_id = s.id AND b.track_id = t.id)
            )
          ORDER BY
            {ARTIST} * (((t.artist_id IS NOT NULL AND t.artist_id = s.artist_id)
                         OR COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist)
                            = s.who COLLATE NOCASE) IS 1)
          + {PLAYLIST} * (EXISTS (SELECT 1 FROM playlist_tracks a
                                  JOIN playlist_tracks b ON b.playlist_id = a.playlist_id
                                   WHERE a.track_id = s.id AND b.track_id = t.id) IS 1)
          + {GENRE} * ((t.genre_id IS NOT NULL AND t.genre_id = s.genre_id) IS 1)
          + {ALBUM} * ((t.album_id IS NOT NULL AND t.album_id = s.album_id) IS 1)
          + {YEAR} * ((t.year IS NOT NULL AND s.year IS NOT NULL
                       AND ABS(t.year - s.year) <= {YEARS}) IS 1)
          + {BPM} * ((t.bpm IS NOT NULL AND s.bpm IS NOT NULL AND s.bpm > 0
                      AND ABS(t.bpm - s.bpm) <= s.bpm * {BPM_TOLERANCE}) IS 1)
            DESC,
            t.play_count DESC,
            RANDOM()
          LIMIT ?2"
    );

    let mut statement = conn.prepare_cached(&sql)?;
    let ids = statement
        .query_map([seed, limit as i64], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot find similar tracks")?;
    Ok(ids)
}

// -- groupings ------------------------------------------------------------

pub fn albums(conn: &Connection, offset: usize, limit: usize) -> Result<Vec<Album>> {
    let mut statement = conn.prepare_cached(
        "SELECT al.id, al.name, al.album_artist, al.artist_id, al.year, al.artwork_id,
                COUNT(t.id)
         FROM albums al
         LEFT JOIN tracks t ON t.album_id = al.id AND t.missing = 0
         GROUP BY al.id
         HAVING COUNT(t.id) > 0
         ORDER BY al.name COLLATE NOCASE
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement
        .query_map([limit as i64, offset as i64], |row| {
            Ok(Album {
                id: row.get(0)?,
                name: row.get(1)?,
                album_artist: row.get(2)?,
                artist_id: row.get(3)?,
                year: row.get(4)?,
                artwork_id: row.get(5)?,
                track_count: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read albums")?;
    Ok(rows)
}

pub fn count_albums(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM albums al
         WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = al.id AND t.missing = 0)",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub fn album(conn: &Connection, id: AlbumId) -> Result<Option<Album>> {
    let mut statement = conn.prepare_cached(
        "SELECT al.id, al.name, al.album_artist, al.artist_id, al.year, al.artwork_id,
                (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id AND t.missing = 0)
         FROM albums al WHERE al.id = ?1",
    )?;
    let mut rows = statement.query_map([id], |row| {
        Ok(Album {
            id: row.get(0)?,
            name: row.get(1)?,
            album_artist: row.get(2)?,
            artist_id: row.get(3)?,
            year: row.get(4)?,
            artwork_id: row.get(5)?,
            track_count: row.get(6)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// The artwork an album carries. Used as the fallback for a track that has no
/// picture of its own, so one untagged file on an album is not the only row in
/// the list with a blank square.
pub fn album_artwork(conn: &Connection, album: AlbumId) -> Result<Option<i64>> {
    let mut statement = conn.prepare_cached("SELECT artwork_id FROM albums WHERE id = ?1")?;
    let mut rows = statement.query_map([album], |row| row.get::<_, Option<i64>>(0))?;
    Ok(rows.next().transpose()?.flatten())
}

/// Splits a `group_concat` result back into ids, dropping anything unparseable
/// and keeping first-seen order.
///
/// Order matters: the SQL sorts the covers by how many tracks carry them, and
/// that is the order the mosaic lays them out in. Duplicates are dropped here
/// rather than in SQL because a genre's covers are grouped by artist, and two
/// artists can legitimately land on the same picture.
fn parse_ids(text: Option<String>) -> Vec<i64> {
    let mut ids: Vec<i64> = Vec::new();
    for part in text.unwrap_or_default().split(',') {
        if let Ok(id) = part.trim().parse::<i64>()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

/// The most covers a mosaic tile will draw.
pub const MOSAIC: usize = 4;

/// Up to four covers for a playlist's mosaic, most common first. Goes through
/// `playlist_filter`, so a smart playlist shows whatever its rules match now.
pub fn playlist_artwork(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let (clause, mut values) = playlist_filter(conn, id)?;
    let clause = if clause.contains("t.missing") {
        clause
    } else {
        format!("t.missing = 0 AND {clause}")
    };
    values.push(Value::Integer(MOSAIC as i64));
    let sql = format!(
        "SELECT group_concat(art) FROM (
            SELECT COALESCE(t.artwork_id, al.artwork_id) AS art, COUNT(*) AS n
              FROM tracks t
              LEFT JOIN albums al ON al.id = t.album_id
              LEFT JOIN genres g  ON g.id = t.genre_id
             WHERE {clause} AND COALESCE(t.artwork_id, al.artwork_id) IS NOT NULL
             GROUP BY art ORDER BY n DESC, art LIMIT ?)"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let text: Option<String> = statement
        .query_row(bind(&values).as_slice(), |row| row.get(0))
        .context("cannot read playlist covers")?;
    Ok(parse_ids(text))
}

pub fn artists(conn: &Connection, offset: usize, limit: usize) -> Result<Vec<Artist>> {
    // Falling back to the album's cover is what makes four covers reachable at
    // all: plenty of libraries carry art on the album row and nothing on the
    // track rows, and without the COALESCE those artists would show one square
    // or none.
    let mut statement = conn.prepare_cached(&format!(
        "SELECT a.id, a.name,
                (SELECT COUNT(DISTINCT t.album_id) FROM tracks t
                  WHERE (t.artist_id = a.id) AND t.missing = 0 AND t.album_id IS NOT NULL),
                (SELECT COUNT(*) FROM tracks t WHERE t.artist_id = a.id AND t.missing = 0),
                (SELECT group_concat(art) FROM (
                    SELECT COALESCE(t.artwork_id, al.artwork_id) AS art, COUNT(*) AS n
                      FROM tracks t LEFT JOIN albums al ON al.id = t.album_id
                     WHERE t.artist_id = a.id AND t.missing = 0
                       AND COALESCE(t.artwork_id, al.artwork_id) IS NOT NULL
                     GROUP BY art ORDER BY n DESC, art LIMIT {MOSAIC}))
         FROM artists a
         WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.artist_id = a.id AND t.missing = 0)
         ORDER BY a.name COLLATE NOCASE
         LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = statement
        .query_map([limit as i64, offset as i64], |row| {
            Ok(Artist {
                id: row.get(0)?,
                name: row.get(1)?,
                album_count: row.get(2)?,
                track_count: row.get(3)?,
                artwork_ids: parse_ids(row.get(4)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read artists")?;
    Ok(rows)
}

pub fn count_artists(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM artists a
         WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.artist_id = a.id AND t.missing = 0)",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// `(genre, track count, up to four covers)`, by name.
///
/// Grouped by artist rather than by artwork, which is the whole point: a genre
/// with one dominant artist would otherwise show that one artist four times.
/// Picking an arbitrary cover per artist is fine — any of their covers is a
/// correct answer to "what does this artist look like". Tracks with no artist
/// group together as one.
///
/// Two artists can still land on the same picture, from a compilation or a
/// split release, so the ids are deduplicated on the way out.
pub fn genres(conn: &Connection) -> Result<Vec<(String, u32, Vec<i64>)>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT g.name,
                (SELECT COUNT(*) FROM tracks t WHERE t.genre_id = g.id AND t.missing = 0),
                (SELECT group_concat(art) FROM (
                    SELECT COALESCE(t.artwork_id, al.artwork_id) AS art, COUNT(*) AS n
                      FROM tracks t LEFT JOIN albums al ON al.id = t.album_id
                     WHERE t.genre_id = g.id AND t.missing = 0
                       AND COALESCE(t.artwork_id, al.artwork_id) IS NOT NULL
                     GROUP BY t.artist_id ORDER BY n DESC LIMIT {MOSAIC}))
         FROM genres g
         WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.genre_id = g.id AND t.missing = 0)
         ORDER BY g.name COLLATE NOCASE"
    ))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, parse_ids(row.get(2)?)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read genres")?;
    Ok(rows)
}

/// `(year, track count)`, newest first.
pub fn years(conn: &Connection) -> Result<Vec<(i32, u32)>> {
    let mut statement = conn.prepare_cached(
        "SELECT year, COUNT(*) FROM tracks
         WHERE year IS NOT NULL AND missing = 0
         GROUP BY year ORDER BY year DESC",
    )?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read years")?;
    Ok(rows)
}

pub fn folders(conn: &Connection) -> Result<Vec<Folder>> {
    let mut statement =
        conn.prepare_cached("SELECT id, path, watch, added_at FROM folders ORDER BY path")?;
    let rows = statement
        .query_map([], |row| {
            Ok(Folder {
                id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                watch: row.get::<_, i64>(2)? != 0,
                added_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read folders")?;
    Ok(rows)
}

/// What a stored track claims to be: its path, its album, and its artist.
pub type Identity = (PathBuf, Option<String>, Option<String>);

/// Album and artist for files already in the library, by path.
///
/// The scanner uses this to judge whether a folder's art belongs to everything
/// in it: a re-scan re-reads only the files that changed, and a directory has to
/// be judged on all of its contents rather than on the one file someone touched.
pub fn identities(conn: &Connection, paths: &[&std::path::Path]) -> Result<Vec<Identity>> {
    let mut out = Vec::with_capacity(paths.len());
    // Chunked because SQLite caps how many parameters one statement may bind,
    // and a single directory can hold more files than that.
    for chunk in paths.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT t.path, al.name, COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist)
             FROM tracks t LEFT JOIN albums al ON al.id = t.album_id
             WHERE t.path IN ({placeholders})"
        );
        let text: Vec<String> = chunk
            .iter()
            .map(|path| crate::db::path_text(path))
            .collect::<Result<_>>()?;
        let params: Vec<&dyn ToSql> = text.iter().map(|path| path as &dyn ToSql).collect();

        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params.as_slice(), |row| {
            Ok((
                PathBuf::from(row.get::<_, String>(0)?),
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// The folder id whose path is the longest prefix of `path`, so a scanned file
/// lands under the folder the user actually added.
pub fn folder_for_path(conn: &Connection, path: &str) -> Result<Option<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT id FROM folders WHERE ?1 LIKE path || '%' ORDER BY LENGTH(path) DESC LIMIT 1",
    )?;
    let mut rows = statement.query_map([path], |row| row.get(0))?;
    Ok(rows.next().transpose()?)
}

// -- search ---------------------------------------------------------------

/// The words the user typed, with punctuation dropped. Everything else in the
/// search is built from these rather than from the raw string, so `"pink floyd"`
/// and `"pink, floyd"` are the same search and word order does not matter.
fn tokens(input: &str) -> Vec<String> {
    input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Turns what the user typed into an FTS5 prefix query. Every token is quoted, so
/// FTS operators the user typed (`"`, `*`, `NEAR`, `-`) are searched for, not run.
fn fts_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = tokens(input)
        .iter()
        .map(|token| format!("\"{token}\"*"))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" AND "))
}

/// A `WHERE` fragment that requires every token to appear somewhere in `haystack`,
/// and the values to bind to it.
///
/// This is what makes the search find a word that is not at the start of a title.
/// The index can only answer prefixes — "roun" finds "Round", "ound" finds
/// nothing — and a person typing three letters from the middle of a song name
/// expects to see it.
fn like_all(haystack: &str, tokens: &[String]) -> (String, Vec<Value>) {
    let clause = tokens
        .iter()
        .map(|_| format!("{haystack} LIKE ? ESCAPE '\\' COLLATE NOCASE"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let values = tokens
        .iter()
        .map(|token| Value::Text(format!("%{}%", escape_like(token))))
        .collect();
    (clause, values)
}

/// `%`, `_` and `\` are wildcards to `LIKE`; a user searching for them means the
/// characters themselves.
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

/// Title, artist, album and genre as one string, for substring matching.
const TRACK_HAYSTACK: &str =
    "(t.title || ' ' || t.artist || ' ' || COALESCE(al.name, '') || ' ' || COALESCE(g.name, ''))";

/// Tracks whose text contains every token, anywhere. The slower half of the
/// search: it cannot use the index, so it only runs to top up what the index
/// already found, and always with a limit.
fn search_tracks_by_substring(
    conn: &Connection,
    tokens: &[String],
    limit: usize,
) -> Result<Vec<Track>> {
    if tokens.is_empty() || limit == 0 {
        return Ok(vec![]);
    }
    let (clause, mut values) = like_all(TRACK_HAYSTACK, tokens);
    let sql = format!(
        "{TRACK_SELECT} WHERE t.missing = 0 AND {clause}
         ORDER BY t.title COLLATE NOCASE LIMIT ?"
    );
    values.push(Value::Integer(limit as i64));

    let params: Vec<&dyn ToSql> = values.iter().map(|value| value as &dyn ToSql).collect();
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement
        .query_map(params.as_slice(), read_track)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot search tracks")?;
    Ok(rows)
}

/// Global search across songs, albums, artists, genres and playlists.
/// `per_group` caps each section so the palette stays fast on a huge library.
pub fn search(conn: &Connection, input: &str, per_group: usize) -> Result<SearchResults> {
    let tokens = tokens(input);
    if tokens.is_empty() {
        return Ok(SearchResults::default());
    }
    let limit = per_group as i64;

    // The index first: it ranks, and it matches whole words and prefixes.
    let mut tracks = match fts_query(input) {
        Some(query) => {
            let sql = format!(
                "{TRACK_SELECT}
                 JOIN tracks_fts f ON f.rowid = t.id
                 WHERE tracks_fts MATCH ?1 AND t.missing = 0
                 ORDER BY bm25(tracks_fts, 4.0, 2.0, 1.0, 1.0)
                 LIMIT ?2"
            );
            let mut statement = conn.prepare_cached(&sql)?;
            statement
                .query_map(rusqlite::params![query, limit], read_track)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("cannot search tracks")?
        }
        None => vec![],
    };

    // Then a substring sweep for anything the index could not reach — a word
    // typed from the middle, or a row whose index entry is stale. Results the
    // index already returned keep their place at the top.
    if tracks.len() < per_group {
        let seen: std::collections::HashSet<TrackId> =
            tracks.iter().map(|track| track.id).collect();
        let extra = search_tracks_by_substring(conn, &tokens, per_group)?;
        tracks.extend(
            extra
                .into_iter()
                .filter(|track| !seen.contains(&track.id))
                .take(per_group - tracks.len()),
        );
    }

    let (clause, mut values) = like_all("al.name", &tokens);
    values.push(Value::Integer(limit));
    let sql = format!(
        "SELECT al.id, al.name, al.album_artist, al.artist_id, al.year, al.artwork_id,
                (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id AND t.missing = 0)
         FROM albums al WHERE {clause}
         ORDER BY al.name COLLATE NOCASE LIMIT ?"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let albums = statement
        .query_map(bind(&values).as_slice(), |row| {
            Ok(Album {
                id: row.get(0)?,
                name: row.get(1)?,
                album_artist: row.get(2)?,
                artist_id: row.get(3)?,
                year: row.get(4)?,
                artwork_id: row.get(5)?,
                track_count: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot search albums")?;

    let (clause, mut values) = like_all("a.name", &tokens);
    values.push(Value::Integer(limit));
    let sql = format!(
        "SELECT a.id, a.name, 0,
                (SELECT COUNT(*) FROM tracks t WHERE t.artist_id = a.id AND t.missing = 0),
                (SELECT t.artwork_id FROM tracks t
                  WHERE t.artist_id = a.id AND t.missing = 0 AND t.artwork_id IS NOT NULL
                  LIMIT 1)
         FROM artists a WHERE {clause}
         ORDER BY a.name COLLATE NOCASE LIMIT ?"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let artists = statement
        .query_map(bind(&values).as_slice(), |row| {
            Ok(Artist {
                id: row.get(0)?,
                name: row.get(1)?,
                album_count: row.get(2)?,
                track_count: row.get(3)?,
                // A search row draws one cover, so it keeps the cheap
                // single-cover subquery above rather than the grouped scan the
                // Artists page pays for.
                artwork_ids: row.get::<_, Option<i64>>(4)?.into_iter().collect(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot search artists")?;

    let (clause, mut values) = like_all("name", &tokens);
    values.push(Value::Integer(limit));
    let sql = format!("SELECT name FROM genres WHERE {clause} ORDER BY name LIMIT ?");
    let mut statement = conn.prepare_cached(&sql)?;
    let genres = statement
        .query_map(bind(&values).as_slice(), |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot search genres")?;

    let (clause, mut values) = like_all("p.name", &tokens);
    values.push(Value::Integer(limit));
    let sql = format!(
        "SELECT p.id, p.name, p.kind, p.rules, p.position, p.created_at, p.modified_at,
                (SELECT COUNT(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id)
         FROM playlists p
         WHERE p.kind != 'favorites' AND {clause}
         ORDER BY p.position LIMIT ?"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let playlists = statement
        .query_map(bind(&values).as_slice(), |row| {
            Ok(PlaylistRow {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: PlaylistKind::parse(&row.get::<_, String>(2)?),
                rules: row.get(3)?,
                position: row.get(4)?,
                created_at: row.get(5)?,
                modified_at: row.get(6)?,
                track_count: row.get(7)?,
                artwork_ids: vec![],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot search playlists")?;

    Ok(SearchResults {
        tracks,
        albums,
        artists,
        genres,
        playlists,
    })
}

// -- user data ------------------------------------------------------------

pub fn set_favorite(conn: &Connection, id: TrackId, favorite: bool) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET favorite = ?2 WHERE id = ?1",
        rusqlite::params![id, favorite as i64],
    )?;
    Ok(())
}

pub fn set_rating(conn: &Connection, id: TrackId, rating: u8) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET rating = ?2 WHERE id = ?1",
        rusqlite::params![id, rating.min(5) as i64],
    )?;
    Ok(())
}

/// Records one listen and bumps the track's counters. Called when playback of a
/// track ends, so a skipped track does not inflate the play count.
pub fn record_listen(conn: &Connection, listen: crate::models::Listen) -> Result<()> {
    conn.execute(
        "INSERT INTO play_history(track_id, played_at, listened_ms, completion)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            listen.track_id,
            listen.played_at,
            listen.listened_ms,
            listen.completion
        ],
    )?;
    // Half the track counts as a play; anything less was a skip.
    if listen.completion >= crate::models::PLAY_THRESHOLD {
        conn.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played = ?2 WHERE id = ?1",
            rusqlite::params![listen.track_id, listen.played_at],
        )?;
    }
    Ok(())
}

/// Stamps a track as played right now, without touching its play count.
///
/// Called when playback starts. "Recently played" means the songs you put on,
/// which is knowable the moment one starts — waiting for the listen to be
/// recorded meant a track only appeared once it had finished, and never at all
/// if it was skipped. The play count still waits for the listen, so Most Played
/// stays a count of real plays rather than of things you started.
pub fn mark_played(conn: &Connection, id: TrackId, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE tracks SET last_played = ?2 WHERE id = ?1",
        rusqlite::params![id, at],
    )?;
    Ok(())
}

// -- per-track audio ------------------------------------------------------

/// The volume and equalizer curve remembered for one song, if any.
pub fn track_audio(conn: &Connection, id: TrackId) -> Result<Option<crate::models::TrackAudio>> {
    let mut statement =
        conn.prepare_cached("SELECT volume, eq FROM track_audio WHERE track_id = ?1")?;
    let mut rows = statement.query_map([id], |row| {
        Ok(crate::models::TrackAudio {
            volume: row.get(0)?,
            eq: row.get(1)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// Remembers a song's volume and curve, replacing whatever was there.
///
/// A save with nothing in it forgets the row instead of writing an empty one —
/// otherwise the "does this song have settings?" check, and the menu entry that
/// depends on it, would be answered yes by a row that holds nothing.
pub fn save_track_audio(
    conn: &Connection,
    id: TrackId,
    audio: &crate::models::TrackAudio,
    at: i64,
) -> Result<()> {
    if audio.is_empty() {
        return forget_track_audio(conn, id);
    }
    conn.execute(
        "INSERT INTO track_audio (track_id, volume, eq, saved_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(track_id) DO UPDATE SET
             volume = excluded.volume,
             eq = excluded.eq,
             saved_at = excluded.saved_at",
        rusqlite::params![id, audio.volume, audio.eq, at],
    )?;
    Ok(())
}

/// Drops a song's remembered settings, so it plays at the global ones again.
pub fn forget_track_audio(conn: &Connection, id: TrackId) -> Result<()> {
    conn.execute("DELETE FROM track_audio WHERE track_id = ?1", [id])?;
    Ok(())
}

/// Removes a track row. The file on disk is not touched.
///
/// The path is remembered as excluded, because the file usually still sits in a
/// watched folder: without this the next scan finds it and puts the row
/// straight back, and "Remove from Library" appears to do nothing.
pub fn remove_track(conn: &Connection, id: TrackId) -> Result<()> {
    use rusqlite::OptionalExtension as _;
    let path: Option<String> = conn
        .query_row("SELECT path FROM tracks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    conn.execute("DELETE FROM tracks WHERE id = ?1", [id])?;
    if let Some(path) = path {
        conn.execute(
            "INSERT OR REPLACE INTO excluded(path, excluded_at) VALUES (?1, ?2)",
            rusqlite::params![path, crate::models::now_ms()],
        )?;
    }
    Ok(())
}

/// Every path the user has taken out of the library, for the scanner to skip.
pub fn excluded_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached("SELECT path FROM excluded")?;
    let rows = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()
        .context("cannot read the excluded paths")?;
    Ok(rows)
}

/// Lets a folder's contents back in. Adding a folder is the user saying they
/// want it indexed, which outranks anything they excluded from it before.
pub fn clear_exclusions_under(conn: &Connection, folder: &std::path::Path) -> Result<()> {
    let text = crate::db::path_text(folder)?;
    conn.execute(
        "DELETE FROM excluded WHERE path = ?1 OR path LIKE ?2 ESCAPE '!'",
        rusqlite::params![text, under(&text)],
    )?;
    Ok(())
}

/// The ids of present tracks at `path`, or anywhere beneath it when it is a
/// directory. What a watcher's "this went away" is turned into.
pub fn ids_under_path(conn: &Connection, path: &std::path::Path) -> Result<Vec<TrackId>> {
    let text = crate::db::path_text(path)?;
    let mut statement = conn.prepare_cached(
        "SELECT id FROM tracks WHERE missing = 0 AND (path = ?1 OR path LIKE ?2 ESCAPE '!')",
    )?;
    let rows = statement
        .query_map(rusqlite::params![text, under(&text)], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<TrackId>>>()
        .context("cannot find the tracks under a path")?;
    Ok(rows)
}

/// A `LIKE` pattern matching everything inside a directory.
///
/// The path is escaped first: `_` matches any single character in SQL and is a
/// perfectly ordinary character in a file name, so an unescaped folder called
/// `my_music` would take `my-music` down with it.
fn under(path: &str) -> String {
    let escaped = path
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_");
    format!("{escaped}{}%", std::path::MAIN_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, ScannedTrack};
    use crate::models::{AudioProperties, TrackTags};

    fn seed(db: &mut Db, path: &str, title: &str, artist: &str, album: &str) -> TrackId {
        let track = ScannedTrack {
            path: PathBuf::from(path),
            folder_id: None,
            tags: TrackTags {
                title: Some(title.to_owned()),
                artist: Some(artist.to_owned()),
                album: Some(album.to_owned()),
                genre: Some("Progressive Rock".to_owned()),
                year: Some(1973),
                ..Default::default()
            },
            properties: AudioProperties {
                duration: 400.0,
                ..Default::default()
            },
            codec: Some("flac".to_owned()),
            file_size: 1_000,
            mtime: 1_738_368_000_000,
            artwork_id: None,
        };
        db.transaction(|conn| Db::upsert_track(conn, &track))
            .unwrap()
            .id()
    }

    #[test]
    fn a_genre_wears_one_cover_from_each_of_its_biggest_artists() {
        // The fixture is Pink Floyd twice and Yes once, all in one genre. Three
        // more under a third artist make it the biggest.
        let mut db = library();
        let one = seed(&mut db, r"d:\m\a.flac", "A", "Third", "Y");
        let two = seed(&mut db, r"d:\m\b.flac", "B", "Third", "Y");
        let three = seed(&mut db, r"d:\m\c.flac", "C", "Third", "Y");
        db.transaction(|conn| {
            conn.execute(
                "INSERT INTO artwork (id, hash) VALUES (1, 'one'), (2, 'two'), (3, 'three')",
                [],
            )?;
            conn.execute(
                "UPDATE tracks SET artwork_id = 3 WHERE id IN (?1, ?2, ?3)",
                [one, two, three],
            )?;
            // Both Pink Floyd tracks carry the same cover: without the grouping
            // by artist it would win twice over and crowd Yes out.
            conn.execute(
                "UPDATE tracks SET artwork_id = 1
                  WHERE artist_id = (SELECT id FROM artists WHERE name = 'Pink Floyd')",
                [],
            )?;
            conn.execute(
                "UPDATE tracks SET artwork_id = 2
                  WHERE artist_id = (SELECT id FROM artists WHERE name = 'Yes')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let genres = genres(db.conn()).unwrap();
        assert_eq!(genres.len(), 1);
        assert_eq!(genres[0].1, 6);
        // One per artist, biggest artist first: three tracks, then two, then one.
        assert_eq!(genres[0].2, vec![3, 1, 2]);
    }

    #[test]
    fn a_smart_playlist_shows_what_its_rules_match() {
        use crate::db::playlists;
        use crate::smart::{Field, Op, Rule, SmartRules};

        let db = library();
        let rules = SmartRules {
            match_all: true,
            rules: vec![Rule {
                field: Field::Artist,
                op: Op::Is,
                value: "Pink Floyd".to_owned(),
            }],
            limit: None,
        };
        let id = playlists::create_smart(db.conn(), "Floyd", rules.to_json().as_deref()).unwrap();
        let view = LibraryView::Playlist(id);

        // It holds no `playlist_tracks` rows at all — the two tracks come from
        // the rules being compiled into the same query every other view uses.
        assert_eq!(count_tracks(db.conn(), &view).unwrap(), 2);
        let titles: Vec<String> = tracks_page(db.conn(), &view, Sort::default(), 0, 10)
            .unwrap()
            .into_iter()
            .map(|track| track.title)
            .collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"Time".to_owned()));
        assert!(titles.contains(&"Money".to_owned()));

        // A hostile value is matched as text rather than executed.
        playlists::set_rules(
            db.conn(),
            id,
            SmartRules {
                match_all: true,
                rules: vec![Rule {
                    field: Field::Artist,
                    op: Op::Is,
                    value: "'; DROP TABLE tracks; --".to_owned(),
                }],
                limit: None,
            }
            .to_json()
            .as_deref(),
        )
        .unwrap();
        assert_eq!(count_tracks(db.conn(), &view).unwrap(), 0);
        // Still there, which is the point.
        assert_eq!(count_tracks(db.conn(), &LibraryView::AllSongs).unwrap(), 3);

        // A limit caps the list without changing what matches.
        playlists::set_rules(
            db.conn(),
            id,
            SmartRules {
                match_all: false,
                rules: vec![Rule {
                    field: Field::Artist,
                    op: Op::Contains,
                    value: "o".to_owned(),
                }],
                limit: Some(1),
            }
            .to_json()
            .as_deref(),
        )
        .unwrap();
        assert_eq!(count_tracks(db.conn(), &view).unwrap(), 1);
    }

    #[test]
    fn a_custom_playlist_keeps_its_own_order_after_smart_playlists_arrive() {
        use crate::db::playlists;

        let db = library();
        let ids = track_ids(db.conn(), &LibraryView::AllSongs, Sort::default()).unwrap();
        let list = playlists::create(db.conn(), "Mine").unwrap();
        playlists::add_tracks(db.conn(), list, &[ids[2], ids[0]]).unwrap();

        let view = LibraryView::Playlist(list);
        assert_eq!(count_tracks(db.conn(), &view).unwrap(), 2);
        assert_eq!(
            track_ids(db.conn(), &view, Sort::default()).unwrap(),
            vec![ids[2], ids[0]]
        );
    }

    #[test]
    fn a_list_of_cover_ids_survives_an_empty_or_malformed_answer() {
        assert_eq!(parse_ids(Some("3,1,9".to_owned())), vec![3, 1, 9]);
        // No covers at all: `group_concat` over nothing is NULL.
        assert_eq!(parse_ids(None), Vec::<i64>::new());
        assert_eq!(parse_ids(Some(String::new())), Vec::<i64>::new());
        assert_eq!(parse_ids(Some("what,,".to_owned())), Vec::<i64>::new());
        // Two artists on one compilation cover: kept once, in first-seen order.
        assert_eq!(parse_ids(Some("5,2,5,7".to_owned())), vec![5, 2, 7]);
    }

    #[test]
    fn removing_a_track_keeps_it_out_of_later_scans() {
        // The file is still sitting in a watched folder, so without a record of
        // the removal the next scan finds it and puts the row straight back.
        let mut db = library();
        let path = r"d:\m\x\unwanted.flac";
        let id = seed(&mut db, path, "Unwanted", "X", "Y");

        assert!(excluded_paths(db.conn()).unwrap().is_empty());
        remove_track(db.conn(), id).unwrap();
        assert_eq!(excluded_paths(db.conn()).unwrap(), vec![path.to_owned()]);

        // Adding the folder again is the user asking for its contents, which
        // outranks the earlier removal — otherwise the file could never return.
        clear_exclusions_under(db.conn(), std::path::Path::new(r"d:\m\x")).unwrap();
        assert!(excluded_paths(db.conn()).unwrap().is_empty());
    }

    #[test]
    fn clearing_one_folder_leaves_another_folders_removals_alone() {
        let mut db = library();
        let kept = seed(&mut db, r"d:\m\keep\a.flac", "A", "X", "Y");
        let other = seed(&mut db, r"d:\m\other\b.flac", "B", "X", "Y");
        remove_track(db.conn(), kept).unwrap();
        remove_track(db.conn(), other).unwrap();

        clear_exclusions_under(db.conn(), std::path::Path::new(r"d:\m\other")).unwrap();
        assert_eq!(
            excluded_paths(db.conn()).unwrap(),
            vec![r"d:\m\keep\a.flac".to_owned()]
        );
    }

    #[test]
    fn starting_a_track_puts_it_in_recently_played_without_counting_a_play() {
        // The two lists answer different questions. Recently Played is what you
        // put on, known the moment it starts. Most Played is what you actually
        // heard, so it waits for the listen — starting a track and skipping it
        // must not inflate the count.
        let mut db = library();
        let id = seed(&mut db, r"d:\m\x\started.flac", "Started", "X", "Y");
        let (recent, most) = (LibraryView::RecentlyPlayed, LibraryView::MostPlayed);

        mark_played(db.conn(), id, 1_738_368_100_000).unwrap();
        assert!(
            track_ids(db.conn(), &recent, Sort::default())
                .unwrap()
                .contains(&id)
        );
        assert!(
            !track_ids(db.conn(), &most, Sort::default())
                .unwrap()
                .contains(&id),
            "starting a track is not a play"
        );

        // Hearing it through is.
        record_listen(
            db.conn(),
            crate::models::Listen {
                track_id: id,
                played_at: 1_738_368_200_000,
                listened_ms: 300_000,
                completion: 0.9,
            },
        )
        .unwrap();
        assert!(
            track_ids(db.conn(), &most, Sort::default())
                .unwrap()
                .contains(&id)
        );
    }

    #[test]
    fn search_never_offers_favorites_as_a_playlist() {
        // Favorites is the heart on a track, not a list you open. The schema
        // seeds a playlist row so the kind has somewhere to live, and it kept
        // surfacing as an ordinary playlist — in the sidebar, in the "add to
        // playlist" menus, and here.
        let db = library();
        let results = search(db.conn(), "favorites", 10).unwrap();
        assert!(results.playlists.is_empty());

        // A playlist the user made with a similar name is still found.
        crate::db::playlists::create(db.conn(), "Favorites Mix").unwrap();
        let results = search(db.conn(), "favorites", 10).unwrap();
        let names: Vec<_> = results.playlists.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Favorites Mix"]);
    }

    #[test]
    fn the_heart_is_what_the_favorites_view_reads() {
        // Favorites is the flag on the track, not the seeded playlist of the
        // same name. Hearting a song used to write one and read the other,
        // which showed an empty list however many songs were hearted.
        let mut db = Db::in_memory().unwrap();
        let id = seed(&mut db, r"d:\m\a.flac", "A", "Someone", "Album");
        let view = LibraryView::Favorites;
        let listed = |db: &Db| track_ids(db.conn(), &view, Sort::default()).unwrap();

        assert!(listed(&db).is_empty());

        set_favorite(db.conn(), id, true).unwrap();
        assert_eq!(listed(&db), vec![id]);

        set_favorite(db.conn(), id, false).unwrap();
        assert!(listed(&db).is_empty());
    }

    fn library() -> Db {
        let mut db = Db::in_memory().unwrap();
        seed(
            &mut db,
            r"d:\m\pf\dsotm\01.flac",
            "Time",
            "Pink Floyd",
            "The Dark Side of the Moon",
        );
        seed(
            &mut db,
            r"d:\m\pf\dsotm\02.flac",
            "Money",
            "Pink Floyd",
            "The Dark Side of the Moon",
        );
        seed(
            &mut db,
            r"d:\m\yes\fragile\01.flac",
            "Roundabout",
            "Yes",
            "Fragile",
        );
        db
    }

    #[test]
    fn a_song_remembers_its_own_volume_until_it_is_told_to_forget() {
        use crate::models::TrackAudio;

        let mut db = library();
        let id = seed(&mut db, r"d:\m\loud\a.flac", "Loud", "X", "Y");
        assert_eq!(track_audio(db.conn(), id).unwrap(), None);

        let mine = TrackAudio {
            volume: Some(0.3),
            eq: Some(r#"{"enabled":true}"#.to_owned()),
        };
        save_track_audio(db.conn(), id, &mine, 1_738_368_000_000).unwrap();
        assert_eq!(track_audio(db.conn(), id).unwrap(), Some(mine));

        // A second save replaces rather than failing on the primary key.
        let quieter = TrackAudio {
            volume: Some(0.1),
            eq: None,
        };
        save_track_audio(db.conn(), id, &quieter, 1_738_368_100_000).unwrap();
        assert_eq!(track_audio(db.conn(), id).unwrap(), Some(quieter));

        // Saving nothing drops the row instead of leaving an empty one behind,
        // which is what the "forget this song" menu entry keys off.
        save_track_audio(db.conn(), id, &TrackAudio::default(), 1_738_368_200_000).unwrap();
        assert_eq!(track_audio(db.conn(), id).unwrap(), None);

        save_track_audio(
            db.conn(),
            id,
            &TrackAudio {
                volume: Some(0.5),
                eq: None,
            },
            1_738_368_300_000,
        )
        .unwrap();
        forget_track_audio(db.conn(), id).unwrap();
        assert_eq!(track_audio(db.conn(), id).unwrap(), None);
    }

    #[test]
    fn forgetting_a_track_forgets_its_audio_settings_with_it() {
        use crate::models::TrackAudio;

        let mut db = library();
        let id = seed(&mut db, r"d:\m\gone\a.flac", "Gone", "X", "Y");
        save_track_audio(
            db.conn(),
            id,
            &TrackAudio {
                volume: Some(0.4),
                eq: None,
            },
            1_738_368_000_000,
        )
        .unwrap();

        remove_track(db.conn(), id).unwrap();
        // The cascade is the reason this is its own table rather than a pile of
        // columns kept in step by hand.
        let orphans: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM track_audio", [], |row| row.get(0))
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn similar_tracks_rank_the_same_artist_above_the_same_genre() {
        let mut db = library();
        // The fixture's three tracks all sit in one genre and one year. A fourth
        // that shares nothing is what proves the filter is doing work at all.
        let stranger = seed(&mut db, r"d:\m\misc\beep.flac", "Beep", "Nobody", "Noise");
        db.transaction(|conn| {
            conn.execute(
                "UPDATE tracks SET genre_id = NULL, year = 1999 WHERE id = ?1",
                [stranger],
            )?;
            Ok(())
        })
        .unwrap();

        let time = track_ids(db.conn(), &LibraryView::AllSongs, Sort::default())
            .unwrap()
            .into_iter()
            .find(|id| track(db.conn(), *id).unwrap().unwrap().title == "Time")
            .unwrap();

        let similar = similar_to(db.conn(), time, 10).unwrap();
        let titles: Vec<String> = similar
            .iter()
            .map(|id| track(db.conn(), *id).unwrap().unwrap().title)
            .collect();

        // Same artist and album first, then the genre-only match. The seed is
        // never suggested back to itself, and the stranger shares no signal at
        // all so it is not a candidate.
        assert_eq!(titles, vec!["Money".to_owned(), "Roundabout".to_owned()]);
    }

    #[test]
    fn pages_a_view() {
        let db = library();
        let view = LibraryView::AllSongs;
        assert_eq!(count_tracks(db.conn(), &view).unwrap(), 3);

        let page = tracks_page(db.conn(), &view, Sort::default(), 0, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].title, "Money");

        let rest = tracks_page(db.conn(), &view, Sort::default(), 2, 2).unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].title, "Time");
    }

    #[test]
    fn finds_by_prefix_across_groups() {
        let db = library();
        let hits = search(db.conn(), "pink", 10).unwrap();
        assert_eq!(hits.tracks.len(), 2);
        assert_eq!(hits.artists.len(), 1);
        assert_eq!(hits.artists[0].name, "Pink Floyd");

        // Partial words match, because the query is a prefix query.
        assert_eq!(search(db.conn(), "roun", 10).unwrap().tracks.len(), 1);
        // Nonsense finds nothing rather than erroring.
        assert!(search(db.conn(), "zzzzz", 10).unwrap().is_empty());
    }

    #[test]
    fn finds_a_word_from_the_middle_of_a_title() {
        let db = library();
        // The index can only answer prefixes, and a person typing three letters
        // out of the middle of a song name still expects to find it.
        assert_eq!(search(db.conn(), "about", 10).unwrap().tracks.len(), 1);
        assert_eq!(search(db.conn(), "oney", 10).unwrap().tracks.len(), 1);
        // Album and artist text count too, and words may be typed in any order.
        assert_eq!(search(db.conn(), "floyd pink", 10).unwrap().tracks.len(), 2);
        assert_eq!(
            search(db.conn(), "ide of the moon", 10)
                .unwrap()
                .tracks
                .len(),
            2
        );
    }

    #[test]
    fn a_track_is_never_listed_twice() {
        let db = library();
        // "Money" is both a prefix hit and a substring hit; it appears once.
        let hits = search(db.conn(), "money", 10).unwrap();
        assert_eq!(hits.tracks.len(), 1);
    }

    #[test]
    fn like_wildcards_are_searched_for_rather_than_run() {
        let db = library();
        // `%` matches everything to LIKE; typed by a user it is a character.
        assert!(search(db.conn(), "%", 10).unwrap().is_empty());
        assert_eq!(escape_like("100%_a\\b"), "100\\%\\_a\\\\b");
    }

    #[test]
    fn search_does_not_execute_fts_operators() {
        let db = library();
        // A bare `"` or `*` would be a syntax error if it reached FTS unquoted.
        assert!(search(db.conn(), "\" OR 1=1 --", 10).is_ok());
        assert!(search(db.conn(), "*", 10).unwrap().is_empty());
        assert!(search(db.conn(), "time NEAR money", 10).is_ok());
    }

    #[test]
    fn tracks_by_id_keeps_the_requested_order() {
        let db = library();
        let ids = track_ids(db.conn(), &LibraryView::AllSongs, Sort::default()).unwrap();
        let reversed: Vec<_> = ids.iter().rev().copied().collect();
        let rows = tracks_by_id(db.conn(), &reversed).unwrap();
        let got: Vec<_> = rows.iter().map(|t| t.id).collect();
        assert_eq!(got, reversed);
    }

    #[test]
    fn a_finished_listen_counts_but_a_skip_does_not() {
        let mut db = library();
        let id = seed(&mut db, r"d:\m\x\01.flac", "Counted", "X", "Y");

        record_listen(
            db.conn(),
            crate::models::Listen {
                track_id: id,
                played_at: 1_738_368_100_000,
                listened_ms: 10_000,
                completion: 0.02,
            },
        )
        .unwrap();
        let plays: i64 = db
            .conn()
            .query_row("SELECT play_count FROM tracks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(plays, 0);

        record_listen(
            db.conn(),
            crate::models::Listen {
                track_id: id,
                played_at: 1_738_368_200_000,
                listened_ms: 380_000,
                completion: 0.95,
            },
        )
        .unwrap();
        let plays: i64 = db
            .conn()
            .query_row("SELECT play_count FROM tracks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(plays, 1);

        // Both listens are in the history, even the skip.
        let listens: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM play_history WHERE track_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(listens, 2);
    }
}
