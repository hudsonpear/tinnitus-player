//! Playlist storage. Ordering lives in `playlist_tracks.position`; a reorder
//! rewrites the whole list inside one transaction rather than shuffling
//! positions one row at a time, which the primary key would reject halfway.

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::models::{PlaylistId, PlaylistKind, PlaylistRow, TrackId, now_ms};

pub fn all(conn: &Connection) -> Result<Vec<PlaylistRow>> {
    let mut statement = conn.prepare_cached(
        "SELECT p.id, p.name, p.kind, p.rules, p.position, p.created_at, p.modified_at,
                (SELECT COUNT(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id)
         FROM playlists p ORDER BY p.position, p.id",
    )?;
    let rows = statement
        .query_map([], |row| {
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
        .context("cannot read playlists")?;
    let mut rows = rows;
    for row in &mut rows {
        row.artwork_ids = crate::db::queries::playlist_artwork(conn, row.id).unwrap_or_default();
    }
    Ok(rows)
}

pub fn create(conn: &Connection, name: &str) -> Result<PlaylistId> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "a playlist needs a name");
    let now = now_ms();
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM playlists",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO playlists(name, kind, position, created_at, modified_at)
         VALUES (?1, 'custom', ?2, ?3, ?3)",
        params![name, next, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Creates a smart playlist: a name and a set of rules, and no stored tracks.
///
/// Same table, same ordering, same everything else — `kind` is the only thing
/// that tells it apart, which is why the schema needed no migration for this.
pub fn create_smart(conn: &Connection, name: &str, rules: Option<&str>) -> Result<PlaylistId> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "a playlist needs a name");
    let now = now_ms();
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM playlists",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO playlists(name, kind, rules, position, created_at, modified_at)
         VALUES (?1, 'smart', ?2, ?3, ?4, ?4)",
        params![name, rules, next, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Replaces a smart playlist's rules. Refuses anything that is not one, so a
/// hand-made list cannot quietly acquire rules nothing would read.
pub fn set_rules(conn: &Connection, id: PlaylistId, rules: Option<&str>) -> Result<()> {
    let changed = conn.execute(
        "UPDATE playlists SET rules = ?2, modified_at = ?3 WHERE id = ?1 AND kind = 'smart'",
        params![id, rules, now_ms()],
    )?;
    anyhow::ensure!(changed == 1, "playlist {id} is not a smart playlist");
    Ok(())
}

/// The `settings` key recording how many automatic playlists have been made.
///
/// A count rather than a flag, so a later release can add a fifth without
/// putting back the four the user has since deleted.
const AUTO_SEEDED: &str = "auto_playlists";

/// Rule sets that shipped, turned out to be wrong, and are replaced.
///
/// Matched byte for byte against what is stored, so a playlist the user has
/// since edited is left exactly as they left it — this repairs a mistake of
/// ours, it does not overwrite a decision of theirs.
const SUPERSEDED: &[(&str, &str)] = &[(
    // Asked for a play count of zero, which a skipped track also has: anything
    // started and abandoned before the halfway mark sat in "Never Played" for
    // ever. It asks whether the track was ever started now.
    "Never Played",
    r#"{"match_all":true,"rules":[{"field":"PlayCount","op":"Is","value":"0"}],"limit":null}"#,
)];

/// Replaces rule sets that shipped wrong, leaving edited ones alone.
fn repair_auto(conn: &Connection) -> Result<()> {
    let current = crate::smart::automatic();
    for (name, stale) in SUPERSEDED {
        let Some((id, stored)) = conn
            .query_row(
                "SELECT id, rules FROM playlists WHERE name = ?1 AND kind = 'smart'",
                [name],
                |row| {
                    Ok((
                        row.get::<_, PlaylistId>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?
        else {
            continue;
        };
        if stored.as_deref() != Some(*stale) {
            continue;
        }
        let Some((_, fixed)) = current.iter().find(|(known, _)| known == name) else {
            continue;
        };
        set_rules(conn, id, fixed.to_json().as_deref())?;
        log::info!("library: corrected the rules behind the {name} smart playlist");
    }
    Ok(())
}

/// Creates the app's own smart playlists, once per library.
///
/// "Once" is the whole design. These are generated rather than authored, but
/// they are not owned: deleting one has to stick, and it cannot stick if
/// startup puts it back. What is remembered is how many have ever been made,
/// so only genuinely new ones are added on a later run.
pub fn seed_auto(conn: &Connection) -> Result<()> {
    let seeded: usize = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [AUTO_SEEDED],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("cannot read how many automatic playlists were made")?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    // Before the early return: a library that already has all of them is
    // exactly the one that may be carrying a rule set we got wrong.
    repair_auto(conn)?;

    let wanted = crate::smart::automatic();
    if seeded >= wanted.len() {
        return Ok(());
    }

    for (name, rules) in wanted.iter().skip(seeded) {
        create_smart(conn, name, rules.to_json().as_deref())
            .with_context(|| format!("cannot create the {name} playlist"))?;
        log::info!("library: added the {name} smart playlist");
    }
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![AUTO_SEEDED, wanted.len().to_string()],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: PlaylistId, name: &str) -> Result<()> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "a playlist needs a name");
    let changed = conn.execute(
        "UPDATE playlists SET name = ?2, modified_at = ?3 WHERE id = ?1 AND kind != 'favorites'",
        params![id, name, now_ms()],
    )?;
    anyhow::ensure!(changed == 1, "playlist {id} cannot be renamed");
    Ok(())
}

pub fn delete(conn: &Connection, id: PlaylistId) -> Result<()> {
    // Favorites is built in and is not the user's to delete.
    let changed = conn.execute(
        "DELETE FROM playlists WHERE id = ?1 AND kind != 'favorites'",
        [id],
    )?;
    anyhow::ensure!(changed == 1, "playlist {id} cannot be deleted");
    Ok(())
}

/// Copies a playlist and its ordering under a new name.
pub fn duplicate(conn: &Connection, id: PlaylistId, name: &str) -> Result<PlaylistId> {
    let copy = create(conn, name)?;
    conn.execute(
        "INSERT INTO playlist_tracks(playlist_id, track_id, position, added_at)
         SELECT ?1, track_id, position, ?2 FROM playlist_tracks WHERE playlist_id = ?3",
        params![copy, now_ms(), id],
    )?;
    Ok(copy)
}

/// Appends tracks to the end, in the order given. Duplicates are allowed: a
/// playlist is a sequence, not a set.
pub fn add_tracks(conn: &Connection, id: PlaylistId, tracks: &[TrackId]) -> Result<()> {
    if tracks.is_empty() {
        return Ok(());
    }
    let start: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?1",
        [id],
        |row| row.get(0),
    )?;
    let now = now_ms();
    let mut statement = conn.prepare(
        "INSERT INTO playlist_tracks(playlist_id, track_id, position, added_at)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (offset, track) in tracks.iter().enumerate() {
        statement.execute(params![id, track, start + offset as i64, now])?;
    }
    drop(statement);
    touch(conn, id)?;
    Ok(())
}

/// Removes the entries at the given positions and closes the gaps.
pub fn remove_at(conn: &Connection, id: PlaylistId, positions: &[i64]) -> Result<()> {
    if positions.is_empty() {
        return Ok(());
    }
    let mut order = track_order(conn, id)?;
    let mut drop_set: Vec<usize> = positions.iter().map(|p| *p as usize).collect();
    drop_set.sort_unstable();
    drop_set.dedup();
    for index in drop_set.into_iter().rev() {
        if index < order.len() {
            order.remove(index);
        }
    }
    rewrite(conn, id, &order)
}

/// Replaces the playlist's order wholesale. Drag-and-drop reordering and sorting
/// both land here.
pub fn reorder(conn: &Connection, id: PlaylistId, tracks: &[TrackId]) -> Result<()> {
    rewrite(conn, id, tracks)
}

pub fn track_order(conn: &Connection, id: PlaylistId) -> Result<Vec<TrackId>> {
    let mut statement = conn.prepare_cached(
        "SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
    )?;
    let rows = statement
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read playlist order")?;
    Ok(rows)
}

/// The playlists a track is already in — what the "Add to playlist" menus tick,
/// so adding the same song twice is a decision rather than an accident.
pub fn holding(conn: &Connection, track: TrackId) -> Result<Vec<PlaylistId>> {
    let mut statement =
        conn.prepare_cached("SELECT playlist_id FROM playlist_tracks WHERE track_id = ?1")?;
    let rows = statement
        .query_map([track], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("cannot read which playlists hold a track")?;
    Ok(rows)
}

pub fn favorites_id(conn: &Connection) -> Result<Option<PlaylistId>> {
    conn.query_row(
        "SELECT id FROM playlists WHERE kind = 'favorites' LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
    .context("cannot find the favorites playlist")
}

fn rewrite(conn: &Connection, id: PlaylistId, tracks: &[TrackId]) -> Result<()> {
    conn.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", [id])?;
    let now = now_ms();
    let mut statement = conn.prepare(
        "INSERT INTO playlist_tracks(playlist_id, track_id, position, added_at)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (position, track) in tracks.iter().enumerate() {
        statement.execute(params![id, track, position as i64, now])?;
    }
    drop(statement);
    touch(conn, id)
}

fn touch(conn: &Connection, id: PlaylistId) -> Result<()> {
    conn.execute(
        "UPDATE playlists SET modified_at = ?2 WHERE id = ?1",
        params![id, now_ms()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, ScannedTrack};
    use crate::models::{AudioProperties, TrackTags};
    use std::path::PathBuf;

    fn track(db: &mut Db, name: &str) -> TrackId {
        let scanned = ScannedTrack {
            path: PathBuf::from(format!(r"d:\m\{name}.mp3")),
            folder_id: None,
            tags: TrackTags {
                title: Some(name.to_owned()),
                artist: Some("A".to_owned()),
                ..Default::default()
            },
            properties: AudioProperties::default(),
            codec: None,
            file_size: 1,
            mtime: 0,
            artwork_id: None,
        };
        db.transaction(|conn| Db::upsert_track(conn, &scanned))
            .unwrap()
            .id()
    }

    #[test]
    fn the_automatic_playlists_are_made_once_and_stay_deleted() {
        // `Db::in_memory` runs the seeding, so they are already there.
        let db = Db::in_memory().unwrap();
        let smart = |db: &Db| -> Vec<String> {
            all(db.conn())
                .unwrap()
                .into_iter()
                .filter(|list| list.kind == PlaylistKind::Smart)
                .map(|list| list.name)
                .collect()
        };
        let expected: Vec<String> = crate::smart::automatic()
            .into_iter()
            .map(|(name, _)| name.to_owned())
            .collect();
        assert_eq!(smart(&db), expected);

        // Running again adds nothing: the count is already recorded.
        seed_auto(db.conn()).unwrap();
        assert_eq!(smart(&db), expected);

        // And a deleted one is not put back, which is the whole point of
        // counting rather than checking for absence.
        let gone = all(db.conn())
            .unwrap()
            .into_iter()
            .find(|list| list.kind == PlaylistKind::Smart)
            .expect("a smart playlist");
        delete(db.conn(), gone.id).unwrap();
        seed_auto(db.conn()).unwrap();
        assert!(!smart(&db).contains(&gone.name));
    }

    #[test]
    fn on_repeat_follows_what_was_actually_played() {
        use crate::db::queries;
        use crate::models::{LibraryView, Listen};

        let mut db = Db::in_memory().unwrap();
        let (hot, cold) = (track(&mut db, "hot"), track(&mut db, "cold"));
        let list = all(db.conn())
            .unwrap()
            .into_iter()
            .find(|list| list.name == "On Repeat")
            .expect("the On Repeat playlist");
        let view = LibraryView::Playlist(list.id);

        // Nothing has been played, so it is empty rather than everything.
        assert_eq!(queries::count_tracks(db.conn(), &view).unwrap(), 0);

        // Three listens this month is "on repeat"; one is not.
        let now = crate::models::now_ms();
        for _ in 0..3 {
            queries::record_listen(
                db.conn(),
                Listen {
                    track_id: hot,
                    played_at: now,
                    listened_ms: 200_000,
                    completion: 1.0,
                },
            )
            .unwrap();
        }
        queries::record_listen(
            db.conn(),
            Listen {
                track_id: cold,
                played_at: now,
                listened_ms: 200_000,
                completion: 1.0,
            },
        )
        .unwrap();

        let ids = queries::track_ids(db.conn(), &view, crate::models::Sort::default()).unwrap();
        assert_eq!(ids, vec![hot]);
    }

    #[test]
    fn never_played_means_never_started_rather_than_never_finished() {
        use crate::db::queries;
        use crate::models::{LibraryView, Listen, Sort, now_ms};

        let mut db = Db::in_memory().unwrap();
        let (skipped, untouched) = (track(&mut db, "skipped"), track(&mut db, "untouched"));
        let list = all(db.conn())
            .unwrap()
            .into_iter()
            .find(|list| list.name == "Never Played")
            .expect("the Never Played playlist");
        let view = LibraryView::Playlist(list.id);

        // Put one on and abandon it after a few seconds. That is a play in the
        // sense that matters here — it was heard — but it never reaches the
        // halfway mark, so `play_count` stays at zero.
        queries::mark_played(db.conn(), skipped, now_ms()).unwrap();
        queries::record_listen(
            db.conn(),
            Listen {
                track_id: skipped,
                played_at: now_ms(),
                listened_ms: 4_000,
                completion: 0.02,
            },
        )
        .unwrap();
        let plays: i64 = db
            .conn()
            .query_row(
                "SELECT play_count FROM tracks WHERE id = ?1",
                [skipped],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(plays, 0, "a skip must not count as a play");

        // It is still gone from Never Played, which is the bug this pins.
        let shown = queries::track_ids(db.conn(), &view, Sort::default()).unwrap();
        assert_eq!(shown, vec![untouched]);
    }

    #[test]
    fn on_repeat_ignores_tracks_that_were_only_skipped() {
        use crate::db::queries;
        use crate::models::{LibraryView, Listen, Sort, now_ms};

        let mut db = Db::in_memory().unwrap();
        let (real, skipped) = (track(&mut db, "real"), track(&mut db, "skipped"));
        let list = all(db.conn())
            .unwrap()
            .into_iter()
            .find(|list| list.name == "On Repeat")
            .expect("the On Repeat playlist");

        // Three real listens against three skips of the same length. Counting
        // raw history rows would make these look identical.
        for _ in 0..3 {
            for (id, completion) in [(real, 0.95), (skipped, 0.05)] {
                queries::record_listen(
                    db.conn(),
                    Listen {
                        track_id: id,
                        played_at: now_ms(),
                        listened_ms: 4_000,
                        completion,
                    },
                )
                .unwrap();
            }
        }

        let shown = queries::track_ids(db.conn(), &LibraryView::Playlist(list.id), Sort::default())
            .unwrap();
        assert_eq!(shown, vec![real]);
    }

    #[test]
    fn the_play_threshold_in_the_rules_matches_the_one_that_counts_plays() {
        // `Field::PlaysThisMonth` has to spell 0.5 into its SQL, because that
        // has to be one `&'static str`. This is what stops the two drifting
        // apart silently: change the constant and come fix the SQL.
        assert_eq!(crate::models::PLAY_THRESHOLD, 0.5);
    }

    #[test]
    fn a_smart_playlist_keeps_no_rows_of_its_own() {
        // The guard that matters lives in `state::Library::add_to_playlist`,
        // which has no test harness. What is pinned here is the reason it has
        // to: rows written against a smart playlist are invisible, because its
        // contents come from its rules and nothing reads `playlist_tracks`.
        let mut db = Db::in_memory().unwrap();
        let a = track(&mut db, "a");
        let list = all(db.conn())
            .unwrap()
            .into_iter()
            .find(|list| list.name == "Never Played")
            .expect("the Never Played playlist");

        add_tracks(db.conn(), list.id, &[a]).unwrap();
        // The row went in...
        assert_eq!(track_order(db.conn(), list.id).unwrap(), vec![a]);
        // ...and the playlist still shows exactly what its rules match, which
        // is every unplayed track whether or not anyone "added" it.
        let shown = crate::db::queries::track_ids(
            db.conn(),
            &crate::models::LibraryView::Playlist(list.id),
            crate::models::Sort::default(),
        )
        .unwrap();
        assert_eq!(shown, vec![a]);

        // Played once, and it leaves — despite the row that was written.
        crate::db::queries::record_listen(
            db.conn(),
            crate::models::Listen {
                track_id: a,
                played_at: crate::models::now_ms(),
                listened_ms: 1_000,
                completion: 1.0,
            },
        )
        .unwrap();
        let shown = crate::db::queries::track_ids(
            db.conn(),
            &crate::models::LibraryView::Playlist(list.id),
            crate::models::Sort::default(),
        )
        .unwrap();
        assert!(shown.is_empty(), "the stored row outlived the rules");
    }

    #[test]
    fn favorites_exists_and_is_protected() {
        let db = Db::in_memory().unwrap();
        let id = favorites_id(db.conn()).unwrap().expect("favorites row");
        assert!(delete(db.conn(), id).is_err());
        assert!(rename(db.conn(), id, "Nope").is_err());
    }

    #[test]
    fn add_reorder_and_remove_keep_positions_dense() {
        let mut db = Db::in_memory().unwrap();
        let (a, b, c) = (
            track(&mut db, "a"),
            track(&mut db, "b"),
            track(&mut db, "c"),
        );
        let list = create(db.conn(), "Rock").unwrap();

        add_tracks(db.conn(), list, &[a, b, c]).unwrap();
        assert_eq!(track_order(db.conn(), list).unwrap(), vec![a, b, c]);

        reorder(db.conn(), list, &[c, a, b]).unwrap();
        assert_eq!(track_order(db.conn(), list).unwrap(), vec![c, a, b]);

        remove_at(db.conn(), list, &[0]).unwrap();
        assert_eq!(track_order(db.conn(), list).unwrap(), vec![a, b]);

        let positions: Vec<i64> = db
            .conn()
            .prepare(
                "SELECT position FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
            )
            .unwrap()
            .query_map([list], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(positions, vec![0, 1]);
    }

    #[test]
    fn the_same_track_can_appear_twice() {
        let mut db = Db::in_memory().unwrap();
        let a = track(&mut db, "a");
        let list = create(db.conn(), "Repeats").unwrap();
        add_tracks(db.conn(), list, &[a, a]).unwrap();
        assert_eq!(track_order(db.conn(), list).unwrap(), vec![a, a]);
    }

    #[test]
    fn duplicate_copies_the_order() {
        let mut db = Db::in_memory().unwrap();
        let (a, b) = (track(&mut db, "a"), track(&mut db, "b"));
        let list = create(db.conn(), "Chill").unwrap();
        add_tracks(db.conn(), list, &[b, a]).unwrap();

        let copy = duplicate(db.conn(), list, "Chill copy").unwrap();
        assert_eq!(track_order(db.conn(), copy).unwrap(), vec![b, a]);
    }

    #[test]
    fn deleting_a_track_removes_it_from_playlists() {
        let mut db = Db::in_memory().unwrap();
        let (a, b) = (track(&mut db, "a"), track(&mut db, "b"));
        let list = create(db.conn(), "Mix").unwrap();
        add_tracks(db.conn(), list, &[a, b]).unwrap();

        crate::db::queries::remove_track(db.conn(), a).unwrap();
        assert_eq!(track_order(db.conn(), list).unwrap(), vec![b]);
    }
}
