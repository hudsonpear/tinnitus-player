//! Library state: what the list is showing, and the background work that fills it.
//!
//! The whole library is never in memory. A view holds its row ids — cheap even
//! at a hundred thousand tracks — and rows themselves arrive a page at a time as
//! the user scrolls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui::{Context, EventEmitter, Task};
use library::db::playlists;
use library::models::{
    Album, Artist, Folder, LibraryView, PlaylistKind, PlaylistRow, ScanEvent, SearchResults, Sort,
    SortKey, Track, TrackId,
};
use library::{ArtworkCache, Cancel, Db, ThumbSize, db::queries};

/// Rows fetched per page. Two screenfuls at any sensible row height.
const PAGE: usize = 120;

/// How many tracks each of Home's recent lists holds.
const RECENT: usize = 8;

/// How often the watcher's mailbox is checked. The watcher debounces bursts for
/// two seconds of its own, so this only has to be the same order.
const WATCH_POLL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum LibraryEvent {
    /// The view changed and the list should scroll back to the top.
    ViewChanged,
    /// A scan finished; anything showing counts should refresh.
    ScanFinished,
    Trouble(String),
}

/// Progress of a running scan, as the UI needs it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScanProgress {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub current: Option<PathBuf>,
}

impl ScanProgress {
    pub fn fraction(&self) -> f32 {
        match self.total > 0 {
            true => (self.done as f32 / self.total as f32).clamp(0.0, 1.0),
            false => 0.0,
        }
    }
}

pub struct Library {
    db: Arc<Mutex<Db>>,
    artwork: Arc<ArtworkCache>,
    /// Cover files already found, by artwork id and size, and the artwork each
    /// album carries. Drawing asks for these constantly and they only change
    /// when the library is re-read.
    art_paths: RefCell<HashMap<(i64, ThumbSize), Option<PathBuf>>>,
    album_art: RefCell<HashMap<i64, Option<i64>>>,
    /// Which playlists hold a track, for the menus that tick them.
    track_lists: RefCell<HashMap<TrackId, Vec<i64>>>,

    view: LibraryView,
    sort: Sort,

    /// Row ids of the current view, in display order.
    ids: Vec<TrackId>,
    /// Rows we have actually read, by index into `ids`.
    rows: HashMap<usize, Track>,
    /// Page indices already requested, so scrolling does not ask twice.
    requested: std::collections::HashSet<usize>,
    /// Bumped by every `reload`. A page that was asked for under an older epoch
    /// belongs to a view the user has already left, and is thrown away.
    epoch: u64,

    albums: Vec<Album>,
    artists: Vec<Artist>,
    genres: Vec<(String, u32, Vec<i64>)>,
    years: Vec<(i32, u32)>,
    folders: Vec<Folder>,
    playlists: Vec<PlaylistRow>,

    /// The short lists Home shows. Held as rows rather than ids because they are
    /// a dozen tracks each and are drawn whole.
    recent_added: Vec<Track>,
    recent_played: Vec<Track>,
    most_played: Vec<Track>,

    search: SearchResults,
    query: String,

    scan: ScanProgress,
    cancel: Cancel,

    /// Watches the library folders for files appearing, changing or going away.
    /// `None` when the platform would not give us a watcher — the library still
    /// works, it just needs Rescan pressing.
    watcher: Option<library::Watcher>,
    /// Which folders the watcher has been pointed at, so the set is only
    /// rebuilt when it actually changes.
    watched: Vec<PathBuf>,

    tasks: Vec<Task<()>>,
}

impl EventEmitter<LibraryEvent> for Library {}

impl Library {
    pub fn new(db: Arc<Mutex<Db>>, artwork: Arc<ArtworkCache>, cx: &mut Context<Self>) -> Self {
        let mut library = Self {
            db,
            artwork,
            art_paths: RefCell::default(),
            album_art: RefCell::default(),
            track_lists: RefCell::default(),
            view: LibraryView::AllSongs,
            sort: Sort::default(),
            ids: vec![],
            rows: HashMap::new(),
            requested: Default::default(),
            epoch: 0,
            albums: vec![],
            artists: vec![],
            genres: vec![],
            years: vec![],
            folders: vec![],
            playlists: vec![],
            recent_added: vec![],
            recent_played: vec![],
            most_played: vec![],
            search: SearchResults::default(),
            query: String::new(),
            scan: ScanProgress::default(),
            cancel: Cancel::new(),
            watcher: match library::Watcher::new() {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    log::warn!("library: no filesystem watching this session: {error:#}");
                    None
                }
            },
            watched: vec![],
            tasks: vec![],
        };
        library.reload(cx);
        library.reload_sidebar(cx);
        library.poll_changes(cx);
        library
    }

    /// Checks the watcher for filesystem changes, forever.
    ///
    /// `drain` is non-blocking, so this polls rather than parking a thread on
    /// the channel — the watcher's own debounce has already collapsed a burst of
    /// events into one batch, and a two-second tick on top of that costs
    /// nothing.
    fn poll_changes(&mut self, cx: &mut Context<Self>) {
        self.spawn(cx, async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_POLL).await;
                if this
                    .update(cx, |library, cx| library.absorb_changes(cx))
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    /// Takes in whatever moved under the watched folders.
    ///
    /// Only the reported paths are re-read. Walking the whole library to find
    /// the one file that changed is what would make copying an album in feel
    /// like the app was grinding: the files land one at a time, and each one
    /// would start another full sweep.
    fn absorb_changes(&mut self, cx: &mut Context<Self>) {
        let Some(watcher) = &self.watcher else { return };
        let changes = watcher.drain();
        if changes.is_empty() {
            return;
        }
        // A scan is already reading everything; its results supersede this batch.
        if self.scan.running {
            return;
        }
        log::info!(
            "library: {} changed and {} gone under the watched folders",
            changes.touched.len(),
            changes.gone.len()
        );

        let db = self.db.clone();
        let artwork = self.artwork.clone();
        let cancel = self.cancel.clone();
        self.spawn(cx, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    library::sync_paths(&db, &artwork, &changes.touched, &changes.gone, &cancel)
                })
                .await;

            this.update(cx, |library, cx| match outcome {
                Ok((added, updated, removed)) => {
                    // Nothing actually changed, so nothing needs redrawing — a
                    // save inside a watched folder that turns out byte-identical
                    // must not throw away the user's place in the list.
                    if added + updated + removed > 0 {
                        library.reload_sidebar(cx);
                        library.reload(cx);
                        cx.emit(LibraryEvent::ScanFinished);
                    }
                }
                Err(error) => log::warn!("library: cannot take in a change: {error:#}"),
            })
            .ok();
        });
    }

    /// Points the watcher at the current set of library folders.
    fn sync_watcher(&mut self) {
        let wanted: Vec<PathBuf> = self
            .folders
            .iter()
            .map(|folder| folder.path.clone())
            .collect();
        if wanted == self.watched {
            return;
        }
        let Some(watcher) = &mut self.watcher else {
            return;
        };

        for path in &self.watched {
            // A folder that has been removed from the library, or has gone from
            // the disk: either way there is nothing to stop watching cleanly.
            watcher.unwatch(path).ok();
        }
        for path in &wanted {
            if let Err(error) = watcher.watch(path) {
                log::warn!("library: {error:#}");
            }
        }
        self.watched = wanted;
    }

    pub fn db(&self) -> Arc<Mutex<Db>> {
        self.db.clone()
    }

    pub fn artwork(&self) -> Arc<ArtworkCache> {
        self.artwork.clone()
    }

    // -- what is being shown ----------------------------------------------

    pub fn view(&self) -> &LibraryView {
        &self.view
    }

    pub fn sort(&self) -> Sort {
        self.sort
    }

    pub fn set_view(&mut self, view: LibraryView, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.reload(cx);
        cx.emit(LibraryEvent::ViewChanged);
    }

    /// Clicking a column header: the same column flips direction, a new column
    /// starts ascending.
    pub fn sort_by(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(key) = sort_key(key) else { return };
        self.sort = match self.sort.key == key {
            true => Sort {
                key,
                descending: !self.sort.descending,
            },
            false => Sort {
                key,
                descending: false,
            },
        };
        self.reload(cx);
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn ids(&self) -> &[TrackId] {
        &self.ids
    }

    /// The row at `index`, if it has been read yet. `None` means "still loading",
    /// and the list draws a placeholder.
    pub fn row(&self, index: usize) -> Option<&Track> {
        self.rows.get(&index)
    }

    pub fn albums(&self) -> &[Album] {
        &self.albums
    }

    pub fn artists(&self) -> &[Artist] {
        &self.artists
    }

    /// `(name, track count, up to four covers from four different artists)`.
    pub fn genres(&self) -> &[(String, u32, Vec<i64>)] {
        &self.genres
    }

    pub fn years(&self) -> &[(i32, u32)] {
        &self.years
    }

    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    pub fn recent_added(&self) -> &[Track] {
        &self.recent_added
    }

    pub fn recent_played(&self) -> &[Track] {
        &self.recent_played
    }

    pub fn most_played(&self) -> &[Track] {
        &self.most_played
    }

    pub fn playlists(&self) -> &[PlaylistRow] {
        &self.playlists
    }

    pub fn scan_progress(&self) -> &ScanProgress {
        &self.scan
    }

    pub fn search_results(&self) -> &SearchResults {
        &self.search
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Where the cached artwork for a track lives, if it is on disk already.
    ///
    /// A track with no picture of its own borrows its album's, which is what
    /// keeps the player bar showing a cover for a file whose own tags carry none.
    pub fn artwork_path(&self, track: &Track, size: ThumbSize) -> Option<PathBuf> {
        let id = match track.artwork_id {
            Some(id) => id,
            None => self.album_artwork(track.album_id?)?,
        };
        self.artwork_for(Some(id), size)
    }

    /// The cached file for an artwork id — what the album grid and the artist
    /// list draw, since those rows carry an id rather than a track.
    ///
    /// Answered from memory after the first look. A grid asks for every one of
    /// its covers on every frame it draws, and each miss costs the database lock,
    /// a query and a `stat`: with a thousand albums on screen that turned
    /// scrolling into a filesystem workout.
    pub fn artwork_for(&self, id: Option<i64>, size: ThumbSize) -> Option<PathBuf> {
        let id = id?;
        if let Some(cached) = self.art_paths.borrow().get(&(id, size)) {
            return cached.clone();
        }
        let found = self
            .db
            .lock()
            .ok()
            .and_then(|db| self.artwork.path_for(db.conn(), id, size).ok().flatten());
        self.art_paths
            .borrow_mut()
            .insert((id, size), found.clone());
        found
    }

    /// Which artwork an album carries, remembered the same way and for the same
    /// reason: every track row without art of its own asks this.
    fn album_artwork(&self, album: i64) -> Option<i64> {
        if let Some(cached) = self.album_art.borrow().get(&album) {
            return *cached;
        }
        let found = self
            .db
            .lock()
            .ok()
            .and_then(|db| queries::album_artwork(db.conn(), album).ok().flatten());
        self.album_art.borrow_mut().insert(album, found);
        found
    }

    /// The playlists that already hold a track.
    ///
    /// Remembered like the artwork, and for the same reason: the menu that asks
    /// is rebuilt on every frame it stays open. Every playlist write reloads the
    /// library, which is where the memo is dropped.
    pub fn playlists_with(&self, track: TrackId) -> Vec<i64> {
        if let Some(cached) = self.track_lists.borrow().get(&track) {
            return cached.clone();
        }
        let found = self
            .db
            .lock()
            .ok()
            .and_then(|db| playlists::holding(db.conn(), track).ok())
            .unwrap_or_default();
        self.track_lists.borrow_mut().insert(track, found.clone());
        found
    }

    /// Drops what was remembered about covers and playlist membership. Called
    /// wherever the library is re-read, which is the only time either can have
    /// changed underneath us.
    fn forget_caches(&self) {
        self.art_paths.borrow_mut().clear();
        self.album_art.borrow_mut().clear();
        self.track_lists.borrow_mut().clear();
    }

    // -- loading ----------------------------------------------------------

    /// Reloads the id list for the current view, then the first page of rows.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        // The ids belong to the view we are leaving, and the list renders from
        // them the moment this returns — before the new ones have arrived. Left
        // in place they are drawn as if they were the new view's, and the pages
        // fetched for them fill `rows` and `requested` so the real ids, when
        // they land, find every page already "loaded" with the old view's
        // songs. Clearing here is what makes the list go empty and then right,
        // rather than wrong until the next sort.
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.ids.clear();
        self.rows.clear();
        self.requested.clear();

        let db = self.db.clone();
        let view = self.view.clone();
        let sort = self.sort;

        self.spawn(cx, async move |this, cx| {
            let ids = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    queries::track_ids(db.conn(), &view, sort)
                })
                .await;

            this.update(cx, |library, cx| {
                if library.epoch != epoch {
                    return;
                }
                match ids {
                    Ok(ids) => {
                        library.ids = ids;
                        library.ensure(0..PAGE.min(library.ids.len()), cx);
                    }
                    Err(error) => {
                        library.trouble(format!("cannot read the library: {error:#}"), cx)
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Called by the list with the range it is about to draw. Anything already
    /// loaded or already asked for is skipped, so scrolling does not queue a
    /// hundred redundant queries.
    pub fn ensure(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        if self.ids.is_empty() {
            return;
        }
        let first = range.start / PAGE;
        let last = range.end.saturating_sub(1).min(self.ids.len() - 1) / PAGE;

        for page in first..=last {
            if !self.requested.insert(page) {
                continue;
            }
            let start = page * PAGE;
            let end = (start + PAGE).min(self.ids.len());
            let wanted: Vec<TrackId> = self.ids[start..end].to_vec();
            let db = self.db.clone();
            let epoch = self.epoch;

            self.spawn(cx, async move |this, cx| {
                let rows = cx
                    .background_executor()
                    .spawn(async move {
                        let db = db.lock().expect("library mutex poisoned");
                        queries::tracks_by_id(db.conn(), &wanted)
                    })
                    .await;

                this.update(cx, |library, cx| {
                    // These are rows for ids the view no longer holds.
                    if library.epoch != epoch {
                        return;
                    }
                    match rows {
                        Ok(rows) => {
                            for (offset, track) in rows.into_iter().enumerate() {
                                library.rows.insert(start + offset, track);
                            }
                        }
                        Err(error) => {
                            // Let it be retried rather than leaving a hole.
                            library.requested.remove(&page);
                            log::warn!("library: cannot read a page: {error:#}");
                        }
                    }
                    cx.notify();
                })
                .ok();
            });
        }
    }

    /// The three short lists on Home. Kept apart from `reload_sidebar` because
    /// playing a track changes them and nothing else, and Home asks for them
    /// again every time it is opened.
    pub fn reload_recent(&mut self, cx: &mut Context<Self>) {
        let db = self.db.clone();
        self.spawn(cx, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    let conn = db.conn();
                    let sort = Sort::default();
                    Ok::<_, anyhow::Error>((
                        queries::tracks_page(conn, &LibraryView::RecentlyAdded, sort, 0, RECENT)?,
                        queries::tracks_page(conn, &LibraryView::RecentlyPlayed, sort, 0, RECENT)?,
                        queries::tracks_page(conn, &LibraryView::MostPlayed, sort, 0, RECENT)?,
                    ))
                })
                .await;

            this.update(cx, |library, cx| {
                match loaded {
                    Ok((added, played, most)) => {
                        library.recent_added = added;
                        library.recent_played = played;
                        library.most_played = most;
                    }
                    Err(error) => log::warn!("library: cannot read the recent lists: {error:#}"),
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Reloads albums, artists, genres, years, folders and playlists.
    pub fn reload_sidebar(&mut self, cx: &mut Context<Self>) {
        // A scan can have written new covers, a playlist write can have moved a
        // track; this is the one place that runs after every such change.
        self.forget_caches();
        // Search results are a snapshot of the rows as they were when the query
        // ran. Retagging a song changes those rows, and a Search page left
        // standing would go on showing the old title until the user typed
        // something else.
        if !self.query.trim().is_empty() {
            let query = self.query.clone();
            self.set_query(query, cx);
        }
        self.reload_recent(cx);
        let db = self.db.clone();
        self.spawn(cx, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    let conn = db.conn();
                    Ok::<_, anyhow::Error>((
                        queries::albums(conn, 0, 5000)?,
                        queries::artists(conn, 0, 5000)?,
                        queries::genres(conn)?,
                        queries::years(conn)?,
                        queries::folders(conn)?,
                        playlists::all(conn)?,
                    ))
                })
                .await;

            this.update(cx, |library, cx| {
                match loaded {
                    Ok((albums, artists, genres, years, folders, playlists)) => {
                        library.folders = folders;
                        // The folder list is the watcher's subscription list, so
                        // this is the one place that has to keep them in step.
                        library.sync_watcher();
                        library.albums = albums;
                        library.artists = artists;
                        library.genres = genres;
                        library.years = years;
                        library.playlists = playlists;
                    }
                    Err(error) => log::warn!("library: cannot read the sidebar: {error:#}"),
                }
                cx.notify();
            })
            .ok();
        });
    }

    // -- search -----------------------------------------------------------

    pub fn set_query(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        self.query = query.into();
        if self.query.trim().is_empty() {
            self.search = SearchResults::default();
            cx.notify();
            return;
        }

        let db = self.db.clone();
        let query = self.query.clone();
        self.spawn(cx, async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    queries::search(db.conn(), &query, 25)
                })
                .await;

            this.update(cx, |library, cx| {
                match results {
                    Ok(results) => library.search = results,
                    Err(error) => log::warn!("library: search failed: {error:#}"),
                }
                cx.notify();
            })
            .ok();
        });
    }

    // -- folders and scanning ---------------------------------------------

    pub fn add_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let db = self.db.clone();
        self.spawn(cx, async move |this, cx| {
            let added = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    let folder = db.add_folder(&path, true)?;
                    // Adding a folder is the user asking for its contents to be
                    // indexed, which outranks anything they removed from it
                    // before — otherwise those files could never come back.
                    queries::clear_exclusions_under(db.conn(), &path)?;
                    Ok::<_, anyhow::Error>(folder)
                })
                .await;

            this.update(cx, |library, cx| match added {
                Ok(_) => {
                    library.reload_sidebar(cx);
                    library.rescan(cx);
                }
                Err(error) => library.trouble(format!("cannot add that folder: {error:#}"), cx),
            })
            .ok();
        });
    }

    pub fn remove_folder(&mut self, id: i64, cx: &mut Context<Self>) {
        let db = self.db.clone();
        self.spawn(cx, async move |this, cx| {
            let removed = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    db.remove_folder(id)?;
                    db.prune_orphans()
                })
                .await;

            this.update(cx, |library, cx| {
                if let Err(error) = removed {
                    library.trouble(format!("cannot remove that folder: {error:#}"), cx);
                }
                library.reload_sidebar(cx);
                library.reload(cx);
            })
            .ok();
        });
    }

    /// Starts a background scan of every configured folder. Playback continues
    /// throughout, and the list stays usable.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scan.running {
            return;
        }
        self.cancel = Cancel::new();
        self.scan = ScanProgress {
            running: true,
            ..Default::default()
        };
        cx.notify();

        let db = self.db.clone();
        let artwork = self.artwork.clone();
        let cancel = self.cancel.clone();

        self.spawn(cx, async move |this, cx| {
            let (progress_tx, progress_rx) = std::sync::mpsc::channel::<ScanEvent>();

            let worker = cx.background_executor().spawn({
                let db = db.clone();
                async move {
                    let folders = {
                        let db = db.lock().expect("library mutex poisoned");
                        queries::folders(db.conn())?
                    };
                    library::scan(&db, &artwork, &folders, &cancel, &|event| {
                        progress_tx.send(event).ok();
                    })
                }
            });

            // Drain progress onto the UI thread while the scan runs.
            let mut finished = false;
            while !finished {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(120))
                    .await;

                let mut batch = Vec::new();
                while let Ok(event) = progress_rx.try_recv() {
                    finished |= matches!(event, ScanEvent::Finished { .. });
                    batch.push(event);
                }
                if batch.is_empty() {
                    continue;
                }
                let carried_on = this
                    .update(cx, |library, cx| {
                        for event in batch {
                            library.apply_scan(event, cx);
                        }
                    })
                    .is_ok();
                if !carried_on {
                    return;
                }
            }

            let outcome = worker.await;
            this.update(cx, |library, cx| {
                library.scan.running = false;
                if let Err(error) = outcome {
                    library.trouble(format!("the scan stopped: {error:#}"), cx);
                }
                library.reload_sidebar(cx);
                library.reload(cx);
                cx.emit(LibraryEvent::ScanFinished);
                cx.notify();
            })
            .ok();
        });
    }

    pub fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        cx.notify();
    }

    fn apply_scan(&mut self, event: ScanEvent, cx: &mut Context<Self>) {
        match event {
            ScanEvent::Started { total } => {
                self.scan = ScanProgress {
                    running: true,
                    done: 0,
                    total,
                    current: None,
                };
            }
            ScanEvent::Progress {
                done,
                total,
                current,
            } => {
                self.scan.done = done;
                self.scan.total = total;
                self.scan.current = Some(current);
            }
            ScanEvent::Finished { .. } => self.scan.running = false,
            ScanEvent::Failed(error) => {
                self.scan.running = false;
                self.trouble(error, cx);
            }
        }
        cx.notify();
    }

    // -- user data --------------------------------------------------------

    pub fn toggle_favorite(&mut self, id: TrackId, cx: &mut Context<Self>) {
        let favorite = !self.is_favorite(id).unwrap_or(false);
        self.set_favorite(id, favorite, cx);
    }

    /// Sets the flag outright rather than flipping it.
    ///
    /// The hearts on a row, in the queue and on the player bar all have to agree,
    /// and the queue's rows live in a cache of the player's rather than here — so
    /// the caller decides the new value once and tells both.
    pub fn set_favorite(&mut self, id: TrackId, favorite: bool, cx: &mut Context<Self>) {
        // Un-hearting a track while looking at Favorites has to take it off the
        // list, and the list is an id query rather than a filter over the rows
        // we hold — so that one view is reloaded once the write lands. Every
        // other view only needs the row updated in place, below.
        match self.view == LibraryView::Favorites {
            true => {
                self.write_then_reload(cx, move |db| queries::set_favorite(db.conn(), id, favorite))
            }
            false => self.write(cx, move |db| queries::set_favorite(db.conn(), id, favorite)),
        }
        // Reflect it immediately rather than waiting for the round trip.
        for track in self.rows.values_mut().filter(|track| track.id == id) {
            track.favorite = favorite;
        }
        for track in self
            .recent_added
            .iter_mut()
            .chain(self.recent_played.iter_mut())
            .chain(self.most_played.iter_mut())
            .filter(|track| track.id == id)
        {
            track.favorite = favorite;
        }
        cx.notify();
    }

    /// The row for a track id, wherever we happen to be holding it.
    ///
    /// The current view's fetched pages are the usual place, but Home's lists
    /// carry rows for tracks that are in no view at all — and a menu opened on
    /// one of those still has to be able to reveal, retag or delete it.
    /// A row we already hold: the page of the current view that has been read,
    /// one of Home's short lists, or a search hit.
    pub fn track_by_id(&self, id: TrackId) -> Option<&Track> {
        self.rows
            .values()
            .chain(self.recent_added.iter())
            .chain(self.recent_played.iter())
            .chain(self.most_played.iter())
            .chain(self.search.tracks.iter())
            .find(|track| track.id == id)
    }

    /// The row for a track, read from the database when it is not already held.
    ///
    /// What anything acting on one particular track should ask. Not every track
    /// on screen is a row we hold — a search result, a queue row, a card on a
    /// page since left. Looking only at the loaded rows is why the metadata
    /// editor could open blank: the id was known, the row was not, and the
    /// fields were left empty rather than filled from what is on disk.
    pub fn track(&self, id: TrackId) -> Option<Track> {
        if let Some(track) = self.track_by_id(id) {
            return Some(track.clone());
        }
        let db = self.db.lock().ok()?;
        queries::track(db.conn(), id).ok().flatten()
    }

    /// Whether a track is a favourite, if we hold a row for it at all.
    pub fn is_favorite(&self, id: TrackId) -> Option<bool> {
        self.track_by_id(id).map(|track| track.favorite)
    }

    /// Removes a row. The file on disk is untouched.
    pub fn forget_track(&mut self, id: TrackId, cx: &mut Context<Self>) {
        self.forget_tracks(vec![id], cx);
    }

    /// Drops rows from the library. The files themselves are somebody else's
    /// business — this is only the record of them.
    ///
    /// One write and one reload however many tracks there are: deleting a
    /// selection used to reload the whole library once per track.
    pub fn forget_tracks(&mut self, ids: Vec<TrackId>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }

        // Dropped from everything held here first, so the rows go as the click
        // lands rather than after the round trip.
        self.ids.retain(|existing| !ids.contains(existing));
        self.rows.clear();
        self.requested.clear();
        for list in [
            &mut self.recent_added,
            &mut self.recent_played,
            &mut self.most_played,
        ] {
            list.retain(|track| !ids.contains(&track.id));
        }

        // `write_then_reload` rather than `write`: the reload has to come after
        // the delete commits, or it races it and reads the row back. It also
        // refreshes Home's lists, which a plain view reload leaves alone — that
        // is what kept a deleted track sitting on Home.
        self.write_then_reload(cx, move |db| {
            for id in ids {
                queries::remove_track(db.conn(), id)?;
            }
            Ok(())
        });
    }

    // -- playlists --------------------------------------------------------

    pub fn create_playlist(&mut self, name: String, cx: &mut Context<Self>) {
        self.write_then_reload(cx, move |db| {
            playlists::create(db.conn(), &name).map(|_| ())
        });
    }

    /// Creates a playlist that already has tracks in it, in one write, so the
    /// sidebar never shows an empty list that fills in a moment later.
    pub fn create_playlist_with(
        &mut self,
        name: String,
        tracks: Vec<TrackId>,
        cx: &mut Context<Self>,
    ) {
        self.write_then_reload(cx, move |db| {
            let id = playlists::create(db.conn(), &name)?;
            playlists::add_tracks(db.conn(), id, &tracks)
        });
    }

    /// Creates a smart playlist from a name and a set of rules.
    pub fn create_smart_playlist(
        &mut self,
        name: String,
        rules: library::smart::SmartRules,
        cx: &mut Context<Self>,
    ) {
        self.write_then_reload(cx, move |db| {
            playlists::create_smart(db.conn(), &name, rules.to_json().as_deref()).map(|_| ())
        });
    }

    /// Replaces a smart playlist's rules, which is what changes what it holds —
    /// there are no stored tracks to rewrite.
    pub fn set_playlist_rules(
        &mut self,
        id: i64,
        rules: library::smart::SmartRules,
        cx: &mut Context<Self>,
    ) {
        self.write_then_reload(cx, move |db| {
            playlists::set_rules(db.conn(), id, rules.to_json().as_deref())
        });
    }

    /// The rules behind a smart playlist, for the editor to open on.
    pub fn playlist_rules(&self, id: i64) -> library::smart::SmartRules {
        let stored = self
            .playlists
            .iter()
            .find(|list| list.id == id)
            .and_then(|list| list.rules.as_deref());
        library::smart::SmartRules::parse(stored)
    }

    pub fn rename_playlist(&mut self, id: i64, name: String, cx: &mut Context<Self>) {
        self.write_then_reload(cx, move |db| playlists::rename(db.conn(), id, &name));
    }

    pub fn delete_playlist(&mut self, id: i64, cx: &mut Context<Self>) {
        self.write_then_reload(cx, move |db| playlists::delete(db.conn(), id));
    }

    pub fn duplicate_playlist(&mut self, id: i64, name: String, cx: &mut Context<Self>) {
        self.write_then_reload(cx, move |db| {
            playlists::duplicate(db.conn(), id, &name).map(|_| ())
        });
    }

    pub fn add_to_playlist(&mut self, id: i64, tracks: Vec<TrackId>, cx: &mut Context<Self>) {
        // Favorites is the `favorite` flag, not a table of rows: the seeded
        // playlist exists so the kind has somewhere to live. Writing entries
        // into it would put tracks somewhere nothing ever reads — which is
        // exactly how "I hearted songs but Favorites is empty" happened.
        if self.is_favorites_playlist(id) {
            for track in tracks {
                self.set_favorite(track, true, cx);
            }
            return;
        }
        // A smart playlist is a question, not a list. Its contents come from
        // running its rules, so a `playlist_tracks` row written here would sit
        // in the database forever and never appear anywhere — the same trap
        // Favorites fell into above. Refused at this level rather than only in
        // the menus, so no caller can reintroduce it.
        if self.is_smart_playlist(id) {
            self.trouble(
                "That playlist fills itself from its rules, so songs cannot be added to it."
                    .to_owned(),
                cx,
            );
            return;
        }
        self.write_then_reload(cx, move |db| playlists::add_tracks(db.conn(), id, &tracks));
    }

    /// Whether a playlist id is the built-in Favorites, which is backed by the
    /// flag on each track rather than by `playlist_tracks`.
    pub fn is_favorites_playlist(&self, id: i64) -> bool {
        self.playlists
            .iter()
            .any(|list| list.id == id && list.kind == PlaylistKind::Favorites)
    }

    /// Whether a playlist id is a smart one, whose contents are its rules
    /// rather than stored rows.
    pub fn is_smart_playlist(&self, id: i64) -> bool {
        self.playlists
            .iter()
            .any(|list| list.id == id && list.kind == PlaylistKind::Smart)
    }

    pub fn reorder_playlist(&mut self, id: i64, tracks: Vec<TrackId>, cx: &mut Context<Self>) {
        // Nothing to reorder: a smart playlist has no stored positions, and
        // writing some would be inventing an order the rules do not have.
        if self.is_smart_playlist(id) {
            return;
        }
        self.write_then_reload(cx, move |db| playlists::reorder(db.conn(), id, &tracks));
    }

    /// Imports an M3U/M3U8/PLS/XSPF file into a new playlist. Entries whose file
    /// is not in the library are dropped, with a count in the log.
    pub fn import_playlist(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let db = self.db.clone();
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Imported".to_owned());

        self.spawn(cx, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let entries = playlist::formats::read(&path)?;
                    let db = db.lock().expect("library mutex poisoned");
                    let conn = db.conn();

                    let mut ids = Vec::new();
                    let mut missing = 0;
                    for entry in &entries {
                        match id_for_path(conn, &entry.path)? {
                            Some(id) => ids.push(id),
                            None => missing += 1,
                        }
                    }
                    let list = playlists::create(conn, &name)?;
                    playlists::add_tracks(conn, list, &ids)?;
                    Ok::<_, anyhow::Error>((ids.len(), missing))
                })
                .await;

            this.update(cx, |library, cx| match outcome {
                Ok((added, missing)) => {
                    if missing > 0 {
                        log::info!(
                            "library: imported {added} tracks, skipped {missing} not in the library"
                        );
                    }
                    library.reload_sidebar(cx);
                }
                Err(error) => {
                    library.trouble(format!("cannot import that playlist: {error:#}"), cx)
                }
            })
            .ok();
        });
    }

    /// Exports a playlist. The format follows the extension of `path`.
    pub fn export_playlist(&mut self, id: i64, path: PathBuf, cx: &mut Context<Self>) {
        let db = self.db.clone();
        // A smart playlist has no stored order to read: `track_order` would
        // come back empty and the export would be a file with nothing in it.
        // What it means to export one is whatever it matches right now.
        let smart = self.is_smart_playlist(id);
        self.spawn(cx, async move |this, cx| {
            let ids = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    match smart {
                        true => queries::track_ids(
                            db.conn(),
                            &LibraryView::Playlist(id),
                            Sort::default(),
                        ),
                        false => playlists::track_order(db.conn(), id),
                    }
                })
                .await;

            this.update(cx, |library, cx| match ids {
                Ok(ids) => library.export_tracks(ids, path, cx),
                Err(error) => {
                    library.trouble(format!("cannot export that playlist: {error:#}"), cx)
                }
            })
            .ok();
        });
    }

    /// Writes an explicit list of tracks out as a playlist file, in the order
    /// given. What "Export to m3u8" on the queue uses — the queue is an order,
    /// not a saved playlist, so there is no id to export from.
    pub fn export_tracks(&mut self, ids: Vec<TrackId>, path: PathBuf, cx: &mut Context<Self>) {
        let db = self.db.clone();
        self.spawn(cx, async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    let tracks = queries::tracks_by_id(db.conn(), &ids)?;
                    // `tracks_by_id` answers in whatever order the rows come
                    // back, and a playlist is nothing if not an order.
                    let by_id: std::collections::HashMap<TrackId, Track> =
                        tracks.into_iter().map(|track| (track.id, track)).collect();
                    let entries: Vec<playlist::Entry> = ids
                        .iter()
                        .filter_map(|id| by_id.get(id))
                        .map(|track| playlist::Entry {
                            path: track.path.clone(),
                            title: Some(format!("{} - {}", track.artist, track.title)),
                            duration: Some(track.duration),
                        })
                        .collect();
                    playlist::formats::write(&path, &entries, path.parent())
                })
                .await;

            this.update(cx, |library, cx| {
                if let Err(error) = written {
                    library.trouble(format!("cannot export that playlist: {error:#}"), cx);
                }
            })
            .ok();
        });
    }

    // -- plumbing ---------------------------------------------------------

    fn trouble(&mut self, message: String, cx: &mut Context<Self>) {
        log::warn!("library: {message}");
        cx.emit(LibraryEvent::Trouble(message));
    }

    /// Runs a write on the background pool, logging anything that goes wrong.
    fn write(
        &mut self,
        cx: &mut Context<Self>,
        body: impl FnOnce(&Db) -> anyhow::Result<()> + Send + 'static,
    ) {
        let db = self.db.clone();
        self.spawn(cx, async move |_, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    body(&db)
                })
                .await;
            if let Err(error) = outcome {
                log::warn!("library: write failed: {error:#}");
            }
        });
    }

    fn write_then_reload(
        &mut self,
        cx: &mut Context<Self>,
        body: impl FnOnce(&Db) -> anyhow::Result<()> + Send + 'static,
    ) {
        let db = self.db.clone();
        self.spawn(cx, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let db = db.lock().expect("library mutex poisoned");
                    body(&db)
                })
                .await;

            this.update(cx, |library, cx| {
                if let Err(error) = outcome {
                    library.trouble(format!("{error:#}"), cx);
                }
                library.reload_sidebar(cx);
                library.reload(cx);
            })
            .ok();
        });
    }

    /// Keeps a task alive without leaking one per keystroke.
    fn spawn<F>(&mut self, cx: &mut Context<Self>, body: F)
    where
        F: AsyncFnOnce(gpui::WeakEntity<Self>, &mut gpui::AsyncApp) + 'static,
    {
        self.tasks.retain(|task| !task.is_ready());
        let task = cx.spawn(async move |this, cx| body(this, cx).await);
        self.tasks.push(task);
    }
}

/// The library row for a path, if there is one.
fn id_for_path(conn: &rusqlite::Connection, path: &Path) -> anyhow::Result<Option<TrackId>> {
    use rusqlite::OptionalExtension as _;
    let text = library::db::path_text(path)?;
    Ok(conn
        .query_row("SELECT id FROM tracks WHERE path = ?1", [text], |row| {
            row.get(0)
        })
        .optional()?)
}

/// Maps a column key from the table header to a sort key. Unknown keys are
/// ignored rather than guessed at.
fn sort_key(key: &str) -> Option<SortKey> {
    Some(match key {
        "title" => SortKey::Title,
        "artist" => SortKey::Artist,
        "album" => SortKey::Album,
        "duration" => SortKey::Duration,
        "track" => SortKey::TrackNumber,
        "year" => SortKey::Year,
        "added" => SortKey::DateAdded,
        "played" => SortKey::LastPlayed,
        "plays" => SortKey::PlayCount,
        "rating" => SortKey::Rating,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_keys_map_to_sort_keys() {
        assert_eq!(sort_key("title"), Some(SortKey::Title));
        assert_eq!(sort_key("duration"), Some(SortKey::Duration));
        // A decorative column has no sort, and must not silently become one.
        assert_eq!(sort_key("art"), None);
        assert_eq!(sort_key(""), None);
    }

    #[test]
    fn scan_progress_reports_a_sane_fraction() {
        let mut progress = ScanProgress::default();
        assert_eq!(progress.fraction(), 0.0);

        progress.total = 200;
        progress.done = 50;
        assert_eq!(progress.fraction(), 0.25);

        // More done than found should not overflow the bar.
        progress.done = 500;
        assert_eq!(progress.fraction(), 1.0);
    }

    #[test]
    fn a_path_that_is_not_in_the_library_is_none_not_an_error() {
        let db = Db::in_memory().unwrap();
        let found = id_for_path(db.conn(), Path::new(r"d:\nowhere\a.flac")).unwrap();
        assert_eq!(found, None);
    }
}
