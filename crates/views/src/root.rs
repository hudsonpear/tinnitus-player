//! The window: title bar, sidebar, content, player bar, and the overlays that
//! sit above them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Pixels, Point, ScrollHandle,
    UniformListScrollHandle, Window, div,
};
use library::models::{LibraryView, TrackId};
use state::{Library, Player, SettingsStore, Tinnitus};
use ui::{ActiveTheme as _, Field, FieldEvent, Menu, MenuEntry, Separator};

use crate::actions::*;
use crate::chrome;
use crate::dialogs::{self, Dialog};
use crate::screen::{History, Screen, SearchFilter};
use crate::screens;

pub struct Root {
    pub settings: Entity<SettingsStore>,
    pub library: Entity<Library>,
    pub player: Entity<Player>,

    pub history: History,
    pub search: Entity<Field>,
    /// The theme colour picker, when open: where it was opened from.
    pub accent_picker: Option<Point<Pixels>>,
    pub palette: Option<Entity<Field>>,
    /// An open context menu: where it is, and what it is about.
    pub menu: Option<(Point<Pixels>, MenuTarget)>,
    pub dialog: Option<Dialog>,
    /// Text fields belonging to the open dialog, by key.
    pub dialog_fields: Vec<(&'static str, Entity<Field>)>,
    /// Indices into the current track list.
    pub selection: HashSet<usize>,
    pub anchor: Option<usize>,
    /// A message across the bottom of the content area.
    pub notice: Option<String>,
    /// Which chip is lit on the Search page.
    pub search_filter: SearchFilter,

    /// The rules being edited in the smart playlist dialog. Held here rather
    /// than in the dialog because a dialog is redrawn from scratch every frame
    /// and the half-built rule set has to survive that.
    pub smart_draft: library::smart::SmartRules,
    /// Which rule row has its field or operator list open: `(index, field?)`.
    /// One at a time, so the dialog does not become a wall of buttons.
    pub smart_picker: Option<(usize, bool)>,

    /// The title last handed to the platform. Windows labels the taskbar button
    /// with it, so it names the song rather than the app — and keeping the last
    /// value means the platform is only told when the song actually changes,
    /// rather than five times a second while a track plays.
    title: String,

    /// The window, kept so the title can be set when the track changes rather
    /// than from `render`. A minimized or occluded window is never drawn, so
    /// anything done in the paint path does not happen until the user clicks
    /// the app — which is exactly how the title used to go stale.
    window: gpui::AnyWindowHandle,

    /// Scroll positions, kept here because a list rebuilt every frame would
    /// otherwise forget where the user had scrolled to, and because the
    /// scrollbar has to read the same handle the list writes.
    ///
    /// A `RefCell` because the screens render from `&Root` and a handle has to
    /// be created the first time a screen is drawn.
    scrolls: RefCell<Scrolls>,

    focus: FocusHandle,
}

/// The scroll handles in play, by the id of the list or region they belong to.
#[derive(Default)]
struct Scrolls {
    lists: HashMap<&'static str, UniformListScrollHandle>,
    areas: HashMap<&'static str, ScrollHandle>,
}

/// What a context menu was opened on.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuTarget {
    Track(TrackId),
    Playlist(i64),
    Folder(i64),
    /// The queue as a whole, from the panel's overflow button.
    Queue,
    /// Just the playlists, for the player bar's add button — the full track menu
    /// there would offer Play and Add to Queue for the track already playing.
    AddToPlaylist(TrackId),
    /// The app itself, from the mark in the corner of the title bar.
    App,
}

impl Root {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = Tinnitus::global(cx);
        let (settings, library, player) = (
            global.settings.clone(),
            global.library.clone(),
            global.player.clone(),
        );

        let search = cx.new(|cx| Field::new(cx).placeholder("Search").icon("search"));
        cx.subscribe(&search, |this, field, event: &FieldEvent, cx| {
            let text = field.read(cx).text().to_owned();
            match event {
                FieldEvent::Changed(_) => this.search_for(text, cx),
                FieldEvent::Cancel => {
                    field.update(cx, |field, cx| field.clear(cx));
                    this.search_for(String::new(), cx);
                }
                // Enter is what makes a search worth remembering. Every prefix
                // typed on the way to a word would otherwise be kept too.
                FieldEvent::Submit(query) => this.remember_search(query.clone(), cx),
            }
        })
        .detach();

        // Anything the player or the library wants the user to see becomes a
        // notice rather than a silent log line.
        cx.subscribe(&player, |this, _, event: &state::PlayerEvent, cx| {
            match event {
                state::PlayerEvent::Trouble(message) => this.notice = Some(message.clone()),
                // Home's lists are a snapshot taken when it was opened, so a
                // track starting while the user is standing on it would not
                // appear until they navigated away and back.
                state::PlayerEvent::TrackChanged(_) => {
                    if this.history.current() == &Screen::Home {
                        this.library
                            .update(cx, |library, cx| library.reload_recent(cx));
                    }
                    this.retitle(cx);
                }
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(&library, |this, _, event: &state::LibraryEvent, cx| {
            if let state::LibraryEvent::Trouble(message) = event {
                this.notice = Some(message.clone());
            }
            cx.notify();
        })
        .detach();

        // Repaint while audio is playing so the clock and the seek bar move.
        cx.observe(&player, |_, _, cx| cx.notify()).detach();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        Self {
            window: window.window_handle(),
            settings,
            library,
            player,
            history: History::new(Screen::Home),
            search,
            accent_picker: None,
            palette: None,
            menu: None,
            dialog: None,
            dialog_fields: vec![],
            selection: HashSet::new(),
            anchor: None,
            notice: None,
            search_filter: SearchFilter::default(),
            smart_draft: library::smart::SmartRules::default(),
            smart_picker: None,
            title: String::new(),
            scrolls: RefCell::default(),
            focus: cx.focus_handle(),
        }
    }

    // -- scrolling --------------------------------------------------------

    /// The scroll handle for a virtualized list, created on first use.
    pub fn list_scroll(&self, id: &'static str) -> UniformListScrollHandle {
        self.scrolls
            .borrow_mut()
            .lists
            .entry(id)
            .or_default()
            .clone()
    }

    /// The scroll handle for a plain scrolling region, created on first use.
    pub fn area_scroll(&self, id: &'static str) -> ScrollHandle {
        self.scrolls
            .borrow_mut()
            .areas
            .entry(id)
            .or_default()
            .clone()
    }

    /// Shows or hides the queue panel, keeping the place in the list.
    ///
    /// The panel and the collapsed strip draw the same queue at different row
    /// heights, so the raw pixel offset means nothing to the other one. What is
    /// carried across is the position measured in rows — fraction included, so a
    /// list stopped halfway down a row stays halfway down it instead of snapping
    /// to the nearest edge.
    pub fn toggle_queue_panel(&mut self, cx: &mut Context<Self>) {
        let showing = self.settings.read(cx).get().queue_panel;
        let theme = cx.theme();
        let (panel, strip) = (
            screens::queue_row_height(theme),
            screens::strip_row_height(theme),
        );
        let (from, to, from_row, to_row) = match showing {
            true => ("queue-panel", "queue-strip", panel, strip),
            false => ("queue-strip", "queue-panel", strip, panel),
        };

        carry_scroll(
            &self.list_scroll(from),
            &self.list_scroll(to),
            from_row,
            to_row,
        );

        self.settings.update(cx, |store, cx| {
            store.update(|s| s.queue_panel = !showing, cx)
        });
        cx.notify();
    }

    // -- navigation -------------------------------------------------------

    /// Commits whatever is in the search box when the user leaves the page.
    ///
    /// The field filters as you type, so almost nobody presses Enter — waiting
    /// for a submit meant the history stayed empty however much was searched.
    /// Leaving the page is the real signal that a search was a search.
    fn leave_search(&mut self, next: &Screen, cx: &mut Context<Self>) {
        if self.history.current() != &Screen::Search || next == &Screen::Search {
            return;
        }
        let query = self.library.read(cx).query().to_owned();
        self.remember_search(query, cx);
    }

    pub fn go(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.leave_search(&screen, cx);
        self.accent_picker = None;
        if let Some(view) = screen.library_view() {
            self.library
                .update(cx, |library, cx| library.set_view(view, cx));
        }
        // Home's recent lists are the only thing that goes stale from playing a
        // track, so they are refreshed on the way in rather than on a timer.
        if screen == Screen::Home {
            self.library
                .update(cx, |library, cx| library.reload_recent(cx));
        }
        self.selection.clear();
        self.anchor = None;
        self.history.go(screen);
        cx.notify();
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.history.peek_back().cloned() {
            self.leave_search(&next, cx);
        }
        if let Some(screen) = self.history.back().cloned() {
            if let Some(view) = screen.library_view() {
                self.library
                    .update(cx, |library, cx| library.set_view(view, cx));
            }
            self.selection.clear();
            cx.notify();
        }
    }

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.history.peek_forward().cloned() {
            self.leave_search(&next, cx);
        }
        if let Some(screen) = self.history.forward().cloned() {
            if let Some(view) = screen.library_view() {
                self.library
                    .update(cx, |library, cx| library.set_view(view, cx));
            }
            self.selection.clear();
            cx.notify();
        }
    }

    /// Filters as the user types. The Search page is somewhere they went on
    /// purpose, so unlike the old title-bar box this no longer navigates on
    /// their behalf.
    fn search_for(&mut self, query: String, cx: &mut Context<Self>) {
        self.library
            .update(cx, |library, cx| library.set_query(query, cx));
        cx.notify();
    }

    /// Puts the text back in the field and searches for it: what clicking one of
    /// the remembered searches does.
    pub fn run_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |field, cx| field.set_text(query.clone(), cx));
        let handle = self.search.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.search_for(query, cx);
    }

    /// Remembers a search the user committed with Enter. Typing is not
    /// committing: every prefix on the way to a word would otherwise pile up in
    /// the list.
    pub fn remember_search(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.settings.update(cx, |store, cx| {
            store.update(
                |settings| {
                    // Searching the same thing again moves it to the front
                    // rather than leaving a second copy behind.
                    settings
                        .search_history
                        .retain(|existing| !existing.eq_ignore_ascii_case(&query));
                    settings.search_history.insert(0, query.clone());
                    settings.search_history.truncate(state::SEARCH_HISTORY);
                },
                cx,
            )
        });
    }

    pub fn forget_searches(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |store, cx| {
            store.update(|settings| settings.search_history.clear(), cx)
        });
    }

    // -- selection --------------------------------------------------------

    /// Click behaviour people expect from a file list: plain click replaces,
    /// ctrl toggles, shift extends from the anchor.
    pub fn select(&mut self, index: usize, extend: bool, toggle: bool, cx: &mut Context<Self>) {
        let (selection, anchor) = select_indices(
            std::mem::take(&mut self.selection),
            self.anchor,
            index,
            extend,
            toggle,
        );
        self.selection = selection;
        self.anchor = anchor;
        cx.notify();
    }

    /// The tracks a command should act on: the whole selection when the clicked
    /// row is part of it, otherwise just that row.
    pub fn acting_on(&self, index: usize, cx: &App) -> Vec<TrackId> {
        let library = self.library.read(cx);
        let indices: Vec<usize> = match self.selection.contains(&index) {
            true => {
                let mut all: Vec<usize> = self.selection.iter().copied().collect();
                all.sort_unstable();
                all
            }
            false => vec![index],
        };
        indices
            .into_iter()
            .filter_map(|index| library.ids().get(index).copied())
            .collect()
    }

    // -- commands ---------------------------------------------------------

    pub fn play_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let ids = self.library.read(cx).ids().to_vec();
        if ids.is_empty() {
            return;
        }
        self.player
            .update(cx, |player, cx| player.play_all(ids, index, cx));
    }

    /// Shuffles the whole library and plays the track the shuffle put first.
    pub fn shuffle_all(&mut self, cx: &mut Context<Self>) {
        let ids = self.library.read(cx).ids().to_vec();
        if ids.is_empty() {
            return;
        }
        self.player
            .update(cx, |player, cx| player.shuffle_all(ids, cx));
    }

    /// Whether a track is a favourite, asking whoever holds a row for it.
    ///
    /// The library's rows and the player's queue cache are separate, and a track
    /// can be in either, both, or — for the playing track of a queue scrolled out
    /// of view — only the player's.
    pub fn is_favorite(&self, id: TrackId, cx: &App) -> bool {
        self.library
            .read(cx)
            .is_favorite(id)
            .or_else(|| self.player.read(cx).is_favorite(id))
            .unwrap_or(false)
    }

    /// The one way a heart is toggled, wherever it was clicked.
    ///
    /// The new value is decided once here and pushed to both caches, so the row,
    /// the queue and the player bar cannot end up disagreeing about the same
    /// track.
    pub fn toggle_favorite(&mut self, id: TrackId, cx: &mut Context<Self>) {
        let favorite = !self.is_favorite(id, cx);
        self.library
            .update(cx, |library, cx| library.set_favorite(id, favorite, cx));
        self.player
            .update(cx, |player, cx| player.mark_favorite(id, favorite, cx));
        cx.notify();
    }

    pub fn open_menu(&mut self, at: Point<Pixels>, target: MenuTarget, cx: &mut Context<Self>) {
        self.menu = Some((at, target));
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Opens a dialog, creating the text fields it asks for. The fields live on
    /// `Root` rather than inside the `Dialog` value so they survive re-renders.
    pub fn show(&mut self, dialog: Dialog, cx: &mut Context<Self>) {
        self.dialog_fields = dialog
            .fields()
            .iter()
            .map(|(key, label)| {
                let label = *label;
                (*key, cx.new(|cx| Field::new(cx).placeholder(label)))
            })
            .collect();

        // Renaming starts from the current name, so the user edits rather than
        // retypes.
        if let Dialog::RenamePlaylist(id) = dialog
            && let Some(name) = self
                .library
                .read(cx)
                .playlists()
                .iter()
                .find(|list| list.id == id)
                .map(|list| list.name.clone())
            && let Some(field) = self.dialog_field_entity("name")
        {
            field.update(cx, |field, cx| field.set_text(name, cx));
        }
        // Saving the whole queue starts from a name the user can accept as it
        // stands, because most of the time they only want it kept, not named.
        // One track is a playlist being started rather than a queue being kept,
        // and there is no sensible name to guess for that.
        if let Dialog::NewPlaylistWith(tracks) = &dialog
            && tracks.len() > 1
            && let Some(field) = self.dialog_field("name")
        {
            field.update(cx, |field, cx| field.set_text("Queue".to_owned(), cx));
        }
        // A single track's existing tags are shown so the editor is an edit, not
        // a blank form. A batch stays blank: a shared value would overwrite the
        // fields that differ.
        if let Dialog::EditMetadata(ids) = &dialog
            && let [only] = ids.as_slice()
        {
            self.prefill_metadata(*only, cx);
        }
        // A new smart playlist opens on one empty rule rather than on nothing,
        // so the first thing the user sees is the shape of the answer.
        if let Dialog::SmartPlaylist(id) = &dialog {
            self.smart_picker = None;
            self.smart_draft = match id {
                Some(id) => self.library.read(cx).playlist_rules(*id),
                None => library::smart::SmartRules::default(),
            };
            if self.smart_draft.rules.is_empty() {
                self.smart_draft.rules.push(library::smart::Rule::default());
            }
            self.smart_draft.rules.truncate(crate::dialogs::MAX_RULES);

            if let Some(id) = id
                && let Some(name) = self
                    .library
                    .read(cx)
                    .playlists()
                    .iter()
                    .find(|list| list.id == *id)
                    .map(|list| list.name.clone())
                && let Some(field) = self.dialog_field("name")
            {
                field.update(cx, |field, cx| field.set_text(name, cx));
            }
            for (index, rule) in self.smart_draft.rules.clone().into_iter().enumerate() {
                if let Some(field) = self.dialog_field(&format!("rule-{index}")) {
                    field.update(cx, |field, cx| field.set_text(rule.value.clone(), cx));
                }
            }
        }

        self.dialog = Some(dialog);
        cx.notify();
    }

    pub fn dismiss_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        self.dialog_fields.clear();
        cx.notify();
    }

    /// The field for a key, ready to be rendered.
    pub fn dialog_field(&self, key: &str) -> Option<Entity<Field>> {
        self.dialog_fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, field)| field.clone())
    }

    fn dialog_field_entity(&self, key: &str) -> Option<Entity<Field>> {
        self.dialog_field(key)
    }

    fn field_text(&self, key: &str, cx: &App) -> String {
        self.dialog_field(key)
            .map(|field| field.read(cx).text().to_owned())
            .unwrap_or_default()
    }

    fn prefill_metadata(&mut self, id: TrackId, cx: &mut Context<Self>) {
        let Some(track) = self.library.read(cx).track(id) else {
            return;
        };

        let values: Vec<(&'static str, String)> = vec![
            ("title", track.title.clone()),
            ("artist", track.artist.clone()),
            ("album", track.album.clone().unwrap_or_default()),
            (
                "album_artist",
                track.album_artist.clone().unwrap_or_default(),
            ),
            ("genre", track.genre.clone().unwrap_or_default()),
            (
                "year",
                track.year.map(|y| y.to_string()).unwrap_or_default(),
            ),
            (
                "track",
                track
                    .track_number
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            ),
            (
                "disc",
                track.disc_number.map(|n| n.to_string()).unwrap_or_default(),
            ),
            ("composer", track.composer.clone().unwrap_or_default()),
            ("comment", track.comment.clone().unwrap_or_default()),
        ];

        for (key, value) in values {
            if value.is_empty() {
                continue;
            }
            if let Some(field) = self.dialog_field(key) {
                field.update(cx, |field, cx| field.set_text(value, cx));
            }
        }
    }

    /// Opens a library folder in the platform's file manager.
    ///
    /// `open_with_system` rather than `reveal_path`: revealing a directory
    /// selects it in its parent, which is not what "Open Folder" says.
    fn open_folder_path(&self, id: i64, cx: &mut Context<Self>) {
        let Some(folder) = self
            .library
            .read(cx)
            .folders()
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.path.clone())
        else {
            return;
        };
        cx.open_with_system(&folder);
    }

    /// Where a folder is on disk, for the removal confirmation to name it.
    pub fn folder_path(&self, id: i64, cx: &App) -> Option<String> {
        self.library
            .read(cx)
            .folders()
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.path.display().to_string())
    }

    /// The file names behind a set of track ids, for the delete confirmation.
    pub fn track_names(&self, ids: &[TrackId], cx: &App) -> Vec<String> {
        let library = self.library.read(cx);
        ids.iter()
            .filter_map(|id| library.track(*id))
            .map(|track| {
                track
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| track.title.clone())
            })
            .collect()
    }

    // -- smart playlist rules ---------------------------------------------

    /// Adds an empty rule row, up to the cap.
    pub fn add_rule(&mut self, cx: &mut Context<Self>) {
        if self.smart_draft.rules.len() >= crate::dialogs::MAX_RULES {
            return;
        }
        self.read_rule_values(cx);
        self.smart_draft.rules.push(library::smart::Rule::default());
        self.write_rule_values(cx);
        cx.notify();
    }

    /// Removes one rule row, never the last — a rule set with no rows at all
    /// matches the whole library, which is never what anyone meant to build.
    pub fn remove_rule(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.smart_draft.rules.len() <= 1 || index >= self.smart_draft.rules.len() {
            return;
        }
        self.read_rule_values(cx);
        self.smart_draft.rules.remove(index);
        self.smart_picker = None;
        self.write_rule_values(cx);
        cx.notify();
    }

    pub fn set_rule_field(
        &mut self,
        index: usize,
        field: library::smart::Field,
        cx: &mut Context<Self>,
    ) {
        let Some(rule) = self.smart_draft.rules.get_mut(index) else {
            return;
        };
        rule.field = field;
        // The operators on offer change with the field, so an operator that no
        // longer applies is replaced rather than left showing something the
        // compiler would quietly reinterpret.
        let allowed = library::smart::Op::for_field(field);
        if !allowed.contains(&rule.op) {
            rule.op = allowed[0];
        }
        self.smart_picker = None;
        cx.notify();
    }

    pub fn set_rule_op(&mut self, index: usize, op: library::smart::Op, cx: &mut Context<Self>) {
        if let Some(rule) = self.smart_draft.rules.get_mut(index) {
            rule.op = op;
        }
        self.smart_picker = None;
        cx.notify();
    }

    /// Opens or closes the field or operator list for one row.
    pub fn toggle_rule_picker(&mut self, index: usize, field: bool, cx: &mut Context<Self>) {
        self.smart_picker = match self.smart_picker {
            Some((open, was_field)) if open == index && was_field == field => None,
            _ => Some((index, field)),
        };
        cx.notify();
    }

    pub fn set_rule_match_all(&mut self, all: bool, cx: &mut Context<Self>) {
        self.smart_draft.match_all = all;
        cx.notify();
    }

    pub fn set_rule_limit(&mut self, limit: Option<u32>, cx: &mut Context<Self>) {
        self.smart_draft.limit = limit;
        cx.notify();
    }

    /// Copies what is typed in the value fields into the draft, so adding or
    /// removing a row does not lose the rows beside it.
    fn read_rule_values(&mut self, cx: &App) {
        for index in 0..self.smart_draft.rules.len() {
            let text = self.field_text(&format!("rule-{index}"), cx);
            if let Some(rule) = self.smart_draft.rules.get_mut(index) {
                rule.value = text;
            }
        }
    }

    /// And back the other way, after the rows have moved.
    fn write_rule_values(&mut self, cx: &mut Context<Self>) {
        for index in 0..crate::dialogs::MAX_RULES {
            let value = self
                .smart_draft
                .rules
                .get(index)
                .map(|rule| rule.value.clone())
                .unwrap_or_default();
            if let Some(field) = self.dialog_field(&format!("rule-{index}")) {
                field.update(cx, |field, cx| field.set_text(value, cx));
            }
        }
    }

    /// The OK button of whatever dialog is open.
    pub fn commit_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };

        match dialog {
            // Nothing to commit: these have a Close button and only read back.
            Dialog::About | Dialog::MediaInfo(_) => {}
            Dialog::NewPlaylist => {
                let name = self.field_text("name", cx);
                if !name.trim().is_empty() {
                    self.library
                        .update(cx, |library, cx| library.create_playlist(name, cx));
                }
            }
            Dialog::RenamePlaylist(id) => {
                let name = self.field_text("name", cx);
                if !name.trim().is_empty() {
                    self.library
                        .update(cx, |library, cx| library.rename_playlist(id, name, cx));
                }
            }
            Dialog::NewPlaylistWith(tracks) => {
                let name = self.field_text("name", cx);
                if !name.trim().is_empty() && !tracks.is_empty() {
                    self.library.update(cx, |library, cx| {
                        library.create_playlist_with(name, tracks, cx)
                    });
                }
            }
            Dialog::SmartPlaylist(id) => {
                let name = self.field_text("name", cx);
                // The value fields are the truth for what was typed; the draft
                // carries the field and operator each row was set to.
                let mut rules = self.smart_draft.clone();
                for (index, rule) in rules.rules.iter_mut().enumerate() {
                    rule.value = self.field_text(&format!("rule-{index}"), cx);
                }
                if !name.trim().is_empty() {
                    self.library.update(cx, |library, cx| match id {
                        Some(id) => {
                            // The dialog edits both, so both are written.
                            library.rename_playlist(id, name, cx);
                            library.set_playlist_rules(id, rules, cx);
                        }
                        None => library.create_smart_playlist(name, rules, cx),
                    });
                }
            }
            Dialog::ConfirmDelete(ids) => self.delete_files(ids, cx),
            Dialog::ConfirmRemoveFolder(id) => {
                self.library
                    .update(cx, |library, cx| library.remove_folder(id, cx));
            }
            Dialog::EditMetadata(ids) => {
                let values: Vec<(&str, String)> = self
                    .dialog_fields
                    .iter()
                    .map(|(key, field)| (*key, field.read(cx).text().to_owned()))
                    .collect();
                // One track's form was filled in from the file, so a field the
                // user emptied is a field they want gone. A batch starts blank
                // and cannot say that.
                let edit = crate::dialogs::tag_edit_from(&values, ids.len() == 1);
                self.write_tags(ids, edit, cx);
            }
        }
        self.dismiss_dialog(cx);
    }

    /// Writes tags to files, then re-reads them so the list matches the disk.
    fn write_tags(&mut self, ids: Vec<TrackId>, edit: library::TagEdit, cx: &mut Context<Self>) {
        if edit.is_empty() {
            return;
        }
        let paths: Vec<std::path::PathBuf> = {
            let library = self.library.read(cx);
            ids.iter()
                .filter_map(|id| library.track(*id))
                .map(|track| track.path.clone())
                .collect()
        };

        let library = self.library.clone();
        cx.spawn(async move |this, cx| {
            let failures = cx
                .background_executor()
                .spawn(async move {
                    let mut failures = Vec::new();
                    for path in paths {
                        if let Err(error) = library::metadata::write_tags(&path, &edit) {
                            failures.push(format!("{}: {error:#}", path.display()));
                        }
                    }
                    failures
                })
                .await;

            this.update(cx, |this, cx| {
                if let Some(first) = failures.first() {
                    this.notice = Some(format!(
                        "{} of the files could not be written. {first}",
                        failures.len()
                    ));
                }
                // The files changed, so their rows are stale.
                library.update(cx, |library, cx| library.rescan(cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Deletes files from disk, after the confirmation dialog said so.
    fn delete_files(&mut self, ids: Vec<TrackId>, cx: &mut Context<Self>) {
        let paths: Vec<std::path::PathBuf> = {
            let library = self.library.read(cx);
            ids.iter()
                .filter_map(|id| library.track(*id))
                .map(|track| track.path.clone())
                .collect()
        };

        let library = self.library.clone();
        cx.spawn(async move |this, cx| {
            let failures = cx
                .background_executor()
                .spawn(async move {
                    let mut failures = Vec::new();
                    for path in paths {
                        // To the Recycle Bin, so a mis-click is recoverable from
                        // Explorer rather than gone.
                        if let Err(error) = library::delete_to_trash(&path) {
                            failures.push(format!("{}: {error:#}", path.display()));
                        }
                    }
                    failures
                })
                .await;

            this.update(cx, |this, cx| {
                if let Some(first) = failures.first() {
                    this.notice = Some(format!("Could not move {first} to the Recycle Bin"));
                }
                library.update(cx, |library, cx| library.forget_tracks(ids, cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the theme from the current settings. Called whenever anything in
    /// the appearance or performance sections changes.
    pub fn apply_theme(&mut self, cx: &mut Context<Self>) {
        let store = self.settings.read(cx);
        let settings = store.get();
        let (look, overrides, effects) =
            (settings.look, settings.theme_overrides(), store.effects());
        let dark = cx.theme().dark;
        ui::Theme::set(look, dark, &overrides, effects, cx);
        cx.notify();
    }

    /// Pushes playback settings into the engine after the user changes them.
    pub fn apply_playback(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.read(cx).get().clone();
        // The equalizer is deliberately not pushed here. With per-song audio on
        // it belongs to the song rather than to the settings file, and changing
        // an unrelated playback setting must not stamp the global curve over
        // it. `apply_equalizer` is the one path that changes the curve.
        self.player.update(cx, |player, cx| {
            player.set_replay_gain(settings.replay_gain, cx);
            player.set_crossfade(settings.crossfade, cx);
            player.set_max_volume(settings.max_volume, cx);
            player.set_remember_audio(settings.remember_per_track_audio, cx);
            player.set_waveform_wanted(settings.timeline == state::Timeline::Waveform, cx);
        });
        cx.notify();
    }

    /// Changes the equalizer curve, and decides who it belongs to.
    ///
    /// With per-song audio on and a library track playing, the edit is that
    /// song's and the settings file is left alone. Otherwise it is the global
    /// curve, as it always was. One place makes that call, so the equalizer
    /// screen does not have to make it five times.
    pub fn apply_equalizer(
        &mut self,
        change: impl FnOnce(&mut audio::EqSettings),
        cx: &mut Context<Self>,
    ) {
        let per_track = self.player.read(cx).per_track_target().is_some();
        let mut eq = self.player.read(cx).equalizer().clone();
        change(&mut eq);

        if !per_track {
            let stored = eq.clone();
            self.settings
                .update(cx, |store, cx| store.update(|s| s.equalizer = stored, cx));
        }
        self.player
            .update(cx, |player, cx| player.set_equalizer(eq, cx));
        cx.notify();
    }

    // -- actions ----------------------------------------------------------

    fn play_pause(&mut self, _: &PlayPause, _window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.toggle(cx));
    }

    fn next_track(&mut self, _: &NextTrack, _window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.next(cx));
    }

    fn previous_track(&mut self, _: &PreviousTrack, _window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.previous(cx));
    }

    fn stop(&mut self, _: &Stop, _window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.stop(cx));
    }

    fn volume_up(&mut self, _: &VolumeUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.nudge_volume(0.05, cx);
    }

    fn volume_down(&mut self, _: &VolumeDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.nudge_volume(-0.05, cx);
    }

    fn nudge_volume(&mut self, delta: f32, cx: &mut Context<Self>) {
        let volume = self.player.read(cx).volume() + delta;
        self.player
            .update(cx, |player, cx| player.set_volume(volume, cx));
        self.settings
            .update(cx, |store, cx| store.update(|s| s.volume = volume, cx));
    }

    fn toggle_mute(&mut self, _: &ToggleMute, _window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.toggle_mute(cx));
    }

    fn toggle_shuffle(&mut self, _: &ToggleShuffle, _window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.toggle_shuffle(cx));
        let shuffle = self.player.read(cx).shuffle();
        self.settings
            .update(cx, |store, cx| store.update(|s| s.shuffle = shuffle, cx));
    }

    fn cycle_repeat(&mut self, _: &CycleRepeat, _window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.cycle_repeat(cx));
        let repeat = self.player.read(cx).repeat();
        self.settings
            .update(cx, |store, cx| store.update(|s| s.repeat = repeat, cx));
    }

    /// The search shortcut has to open the page now that the field lives on it —
    /// focusing a box that is not on screen would look like nothing happened.
    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.current() != &Screen::Search {
            self.go(Screen::Search, cx);
        }
        let handle = self.search.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn command_palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| Field::new(cx).placeholder("Type a command or a song"));
        let handle = field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.subscribe(&field, |this, field, event: &FieldEvent, cx| match event {
            FieldEvent::Changed(text) => {
                this.library
                    .update(cx, |library, cx| library.set_query(text.clone(), cx));
            }
            FieldEvent::Cancel => {
                this.palette = None;
                let _ = field;
                cx.notify();
            }
            // Enter plays the top song, the row a click on it would have hit.
            FieldEvent::Submit(_) => {
                let top = this.library.read(cx).search_results().tracks.first().map(|t| t.id);
                if let Some(id) = top {
                    this.player.update(cx, |player, cx| player.play_now(id, cx));
                    this.palette = None;
                    cx.notify();
                }
            }
        })
        .detach();
        self.palette = Some(field);
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, _window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some()
            || self.accent_picker.take().is_some()
            || self.menu.take().is_some()
            || self.dialog.take().is_some()
        {
            cx.notify();
            return;
        }
        if self.notice.take().is_some() {
            cx.notify();
        }
    }

    fn show_queue(&mut self, _: &ShowQueue, _window: &mut Window, cx: &mut Context<Self>) {
        self.go(Screen::Queue, cx);
    }

    fn show_settings(&mut self, _: &ShowSettings, _window: &mut Window, cx: &mut Context<Self>) {
        self.go(Screen::Settings, cx);
    }

    fn show_equalizer(&mut self, _: &ShowEqualizer, _window: &mut Window, cx: &mut Context<Self>) {
        self.go(Screen::Equalizer, cx);
    }

    fn back(&mut self, _: &GoBack, _window: &mut Window, cx: &mut Context<Self>) {
        self.go_back(cx);
    }

    fn forward(&mut self, _: &GoForward, _window: &mut Window, cx: &mut Context<Self>) {
        self.go_forward(cx);
    }

    fn rescan(&mut self, _: &Rescan, _window: &mut Window, cx: &mut Context<Self>) {
        self.library.update(cx, |library, cx| library.rescan(cx));
    }

    fn quit(&mut self, _: &Quit, _window: &mut Window, cx: &mut Context<Self>) {
        // Closing the window is leaving the page too, and the save below is the
        // last chance to keep what was typed.
        self.leave_search(&Screen::Home, cx);
        self.settings.read(cx).save_now();
        self.player.read(cx).shutdown();
        cx.quit();
    }

    fn open_files(&mut self, _: &OpenFiles, _window: &mut Window, cx: &mut Context<Self>) {
        self.pick(false, cx);
    }

    fn open_folder(&mut self, _: &OpenFolder, _window: &mut Window, cx: &mut Context<Self>) {
        self.pick(true, cx);
    }

    fn seek_back(&mut self, _: &SeekBack, _window: &mut Window, cx: &mut Context<Self>) {
        self.seek_by(-5.0, cx);
    }

    fn seek_forward(&mut self, _: &SeekForward, _window: &mut Window, cx: &mut Context<Self>) {
        self.seek_by(5.0, cx);
    }

    /// `seek` clamps to the track, so running off either end is harmless.
    fn seek_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| {
            let at = player.position() + delta;
            player.seek(at);
            cx.notify();
        });
    }

    fn favorite_current(&mut self, _: &ToggleFavorite, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.player.read(cx).current().map(|track| track.id) else {
            return;
        };
        self.toggle_favorite(id, cx);
    }

    /// The list shortcuts act on a track list and nothing else: not behind a
    /// dialog or the palette, and not on Home, where there is no selection.
    fn on_track_list(&self) -> bool {
        matches!(self.history.current(), Screen::Tracks(_))
            && self.dialog.is_none()
            && self.palette.is_none()
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.on_track_list() {
            return;
        }
        let count = self.library.read(cx).ids().len();
        self.selection = (0..count).collect();
        self.anchor = (count > 0).then_some(0);
        cx.notify();
    }

    fn play_selected(&mut self, _: &PlaySelected, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.on_track_list() {
            return;
        }
        if let Some(first) = self.selection.iter().min().copied() {
            self.play_index(first, cx);
        }
    }

    /// Goes through the same confirmation as the menu's Delete, which sends the
    /// files to the Recycle Bin.
    fn delete_selected(&mut self, _: &DeleteSelected, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.on_track_list() {
            return;
        }
        let ids: Vec<TrackId> = {
            let library = self.library.read(cx);
            let mut indices: Vec<usize> = self.selection.iter().copied().collect();
            indices.sort_unstable();
            indices
                .into_iter()
                .filter_map(|index| library.ids().get(index).copied())
                .collect()
        };
        if !ids.is_empty() {
            self.show(Dialog::ConfirmDelete(ids), cx);
        }
    }

    /// Selects the playing song in the list and scrolls it into view, opening
    /// All Songs first when no list is showing.
    fn show_current(&mut self, _: &ShowCurrent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.player.read(cx).current().map(|track| track.id) else {
            return;
        };
        if !matches!(self.history.current(), Screen::Tracks(_)) {
            self.go(Screen::Tracks(LibraryView::AllSongs), cx);
        }
        let Some(index) = self.library.read(cx).ids().iter().position(|row| *row == id) else {
            return;
        };
        self.selection = HashSet::from([index]);
        self.anchor = Some(index);
        self.list_scroll("tracks")
            .scroll_to_item(index, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    /// Opens the platform file picker. Folders are added to the library and
    /// scanned; files are played straight away.
    fn pick(&mut self, directories: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: !directories,
            directories,
            multiple: true,
            prompt: None,
        });
        let library = self.library.clone();

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update(cx, |this, cx| {
                for path in paths {
                    match path.is_dir() {
                        true => library.update(cx, |library, cx| library.add_folder(path, cx)),
                        false => this.play_file(path, cx),
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Names the window after whatever is playing.
    ///
    /// The title bar is ours and draws none of this, but the platform still uses
    /// the window title for the taskbar button and its preview — so this is what
    /// makes the current song readable with the window buried. Which is also why
    /// it cannot live in `render`: a buried window is not drawn.
    fn retitle(&mut self, cx: &mut Context<Self>) {
        let player = self.player.read(cx);
        let next = title_for(player.now_playing(), player.now_playing_detail());
        if self.title == next {
            return;
        }
        self.title = next.clone();
        self.window
            .update(cx, |_, window, _| window.set_window_title(&next))
            .ok();
    }

    /// Plays a file the user dropped or opened, whether or not it is in the
    /// library. A player that refuses to open a file it has not indexed is a
    /// player that gets closed.
    pub fn play_file(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        if path.is_dir() {
            self.library
                .update(cx, |library, cx| library.add_folder(path, cx));
            return;
        }
        // A playlist file is a list of songs, not a song: the whole list goes
        // into the queue and the first entry starts. Checked before the audio
        // test, which an .m3u8 would fail.
        if state::is_playlist_file(&path) {
            self.player
                .update(cx, |player, cx| player.play_playlist_file(path, cx));
            return;
        }
        if !library::is_audio_file(&path) {
            self.notice = Some(format!("{} is not an audio file", path.display()));
            cx.notify();
            return;
        }
        // ponytail: files outside the library are played by handing the engine
        // the path directly. They get no row, so no history or rating — adding
        // an "outside the library" row type is not worth it for a drag-and-drop.
        self.player.update(cx, |player, cx| {
            player.play_loose_file(path, cx);
        });
    }
}

impl Focusable for Root {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let background = theme.background;
        let foreground = theme.foreground;
        let font_size = theme.font_size;

        div()
            .id("root")
            .track_focus(&self.focus)
            .key_context("Root")
            .on_action(cx.listener(Self::play_pause))
            .on_action(cx.listener(Self::next_track))
            .on_action(cx.listener(Self::previous_track))
            .on_action(cx.listener(Self::stop))
            .on_action(cx.listener(Self::volume_up))
            .on_action(cx.listener(Self::volume_down))
            .on_action(cx.listener(Self::toggle_mute))
            .on_action(cx.listener(Self::toggle_shuffle))
            .on_action(cx.listener(Self::cycle_repeat))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::command_palette))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::show_queue))
            .on_action(cx.listener(Self::show_settings))
            .on_action(cx.listener(Self::show_equalizer))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::forward))
            .on_action(cx.listener(Self::rescan))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::open_files))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::seek_back))
            .on_action(cx.listener(Self::seek_forward))
            .on_action(cx.listener(Self::favorite_current))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::play_selected))
            .on_action(cx.listener(Self::delete_selected))
            .on_action(cx.listener(Self::show_current))
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, _window, cx| {
                    for path in paths.paths() {
                        this.play_file(path.clone(), cx);
                    }
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .text_size(font_size)
            .font_family("Segoe UI")
            .child(chrome::title_bar(self, window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(chrome::sidebar(self, cx))
                    .child(Separator::vertical())
                    .child(screens::content(self, window, cx))
                    .child(Separator::vertical())
                    // Turning the panel off does not hide the queue, it shrinks
                    // it to a column of album art: what is coming next stays
                    // visible without costing the content area its full width.
                    // An empty queue keeps its place too — a panel that comes
                    // and goes on its own moves the whole window out from under
                    // the pointer.
                    .child(match self.settings.read(cx).get().queue_panel {
                        true => chrome::queue_panel(self, cx).into_any_element(),
                        false => screens::queue_strip(self, cx),
                    }),
            )
            .child(Separator::horizontal())
            .child(chrome::player_bar(self, window, cx))
            .when_some(self.notice.clone(), |this, message| {
                this.child(chrome::notice(message, cx))
            })
            .when_some(self.menu.clone(), |this, (at, target)| {
                this.child(self.context_menu(at, target, cx))
            })
            .when(self.palette.is_some(), |this| {
                this.child(dialogs::palette(self, cx))
            })
            .when_some(self.accent_picker, |this, at| {
                this.child(dialogs::accent_popover(self, at, window.viewport_size(), cx))
            })
            .when_some(self.dialog.clone(), |this, dialog| {
                this.child(dialogs::modal(self, dialog, cx))
            })
    }
}

impl Root {
    fn context_menu(
        &self,
        at: Point<Pixels>,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let menu = Menu::new("context-menu", at).on_dismiss(on_root(cx, |this, _window, cx| {
            this.close_menu(cx);
        }));

        match target {
            MenuTarget::Track(id) => menu.entries(self.track_menu(id, cx)),
            MenuTarget::Playlist(id) => menu.entries(self.playlist_menu(id, cx)),
            MenuTarget::Folder(id) => menu.entries(self.folder_menu(id, cx)),
            MenuTarget::Queue => menu.entries(self.queue_menu(cx)),
            MenuTarget::AddToPlaylist(id) => menu.entries(self.add_to_playlist_menu(id, cx)),
            MenuTarget::App => menu.entries(self.app_menu(cx)),
        }
    }

    /// The application menu, behind the mark in the corner: the handful of
    /// things that act on the app rather than on whatever is selected.
    fn app_menu(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let player = self.player.read(cx);
        let (speed, muted) = (player.speed(), player.muted());
        let queue_panel = self.settings.read(cx).get().queue_panel;

        vec![
            MenuEntry::item(
                "app-add-folder",
                "Add Folder…",
                on_root(cx, |this, window, cx| this.dispatch_open_folder(window, cx)),
            )
            .icon("folder"),
            MenuEntry::item(
                "app-new-playlist",
                "Create Playlist…",
                on_root(cx, |this, _window, cx| this.show(Dialog::NewPlaylist, cx)),
            )
            .icon("playlist"),
            MenuEntry::Divider,
            // The speed is in the label rather than behind a submenu: one click
            // steps to the next, the same as the button in the player bar.
            MenuEntry::item(
                "app-speed",
                format!("Playback Speed ({speed:.2}x)"),
                on_root(cx, |this, _window, cx| this.cycle_speed(cx)),
            )
            .icon("clock"),
            MenuEntry::item(
                "app-mute",
                match muted {
                    true => "Unmute",
                    false => "Mute",
                },
                on_root(cx, |this, _window, cx| {
                    this.player.update(cx, |player, cx| player.toggle_mute(cx));
                }),
            )
            .icon(match muted {
                true => "volume",
                false => "volume-mute",
            }),
            MenuEntry::item(
                "app-queue",
                match queue_panel {
                    true => "Hide Queue",
                    false => "Show Queue",
                },
                on_root(cx, |this, _window, cx| this.toggle_queue_panel(cx)),
            )
            .icon("queue")
            .checked(queue_panel),
            MenuEntry::Divider,
            MenuEntry::item(
                "app-equalizer",
                "Equalizer",
                on_root(cx, |this, _window, cx| this.go(Screen::Equalizer, cx)),
            )
            .icon("equalizer"),
            MenuEntry::item(
                "app-settings",
                "Settings",
                on_root(cx, |this, _window, cx| this.go(Screen::Settings, cx)),
            )
            .icon("settings"),
            MenuEntry::item(
                "app-about",
                "About",
                on_root(cx, |this, _window, cx| this.show(Dialog::About, cx)),
            )
            .icon("info"),
        ]
    }

    /// The playlists a track can be added to, and a way to make one if there are
    /// none yet — an empty menu would look broken.
    fn add_to_playlist_menu(&self, track: TrackId, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        // Ticked where the song is already in the list, so adding it twice is a
        // decision rather than a surprise.
        let holding = self.library.read(cx).playlists_with(track);
        // The one thing here that is not about playlists. It belongs on the
        // player bar because that is where the user is when a song they like
        // is the song that is playing.
        let mut entries = vec![
            MenuEntry::item(
                "bar-play-similar",
                "Play Similar Tracks",
                on_root(cx, move |this, _window, cx| {
                    this.player
                        .update(cx, |player, cx| player.play_similar(track, cx));
                }),
            )
            .icon("shuffle"),
            MenuEntry::Divider,
        ];
        entries.extend(self.addable_playlists(cx).into_iter().map(|(id, name)| {
            MenuEntry::item(
                gpui::ElementId::Name(format!("bar-add-to-{id}").into()),
                name,
                on_root(cx, move |this, _window, cx| {
                    this.library.update(cx, |library, cx| {
                        library.add_to_playlist(id, vec![track], cx)
                    });
                }),
            )
            .icon("playlist")
            .checked(holding.contains(&id))
        }));

        entries.push(MenuEntry::Divider);
        entries.push(
            MenuEntry::item(
                "bar-new-playlist",
                "New Playlist…",
                on_root(cx, move |this, _window, cx| {
                    // The dialog carries the track, so committing it creates the
                    // playlist with the song already in it.
                    this.show(Dialog::NewPlaylistWith(vec![track]), cx);
                }),
            )
            .icon("plus"),
        );
        entries
    }

    /// Everything that can be done to the queue as a whole. Lives behind the
    /// panel's overflow button, so the header carries one control rather than
    /// four.
    fn queue_menu(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let tracks = self.player.read(cx).queue().in_play_order();
        let empty = tracks.is_empty();

        let mut entries = vec![
            MenuEntry::item(
                "queue-clear",
                "Clear Queue",
                on_root(cx, |this, _window, cx| {
                    this.player.update(cx, |player, cx| player.clear_queue(cx));
                }),
            )
            .icon("trash")
            .danger()
            .disabled(empty),
            MenuEntry::Divider,
            MenuEntry::item("queue-save", "Save as Playlist", {
                let tracks = tracks.clone();
                on_root(cx, move |this, _window, cx| {
                    this.show(Dialog::NewPlaylistWith(tracks.clone()), cx);
                })
            })
            .icon("playlist")
            .disabled(empty),
            MenuEntry::item("queue-export", "Export to m3u8 file", {
                let tracks = tracks.clone();
                on_root(cx, move |this, _window, cx| {
                    this.export_tracks(tracks.clone(), cx);
                })
            })
            .icon("external")
            .disabled(empty),
        ];

        // The same shape as the track menu: a labelled group rather than a
        // submenu, so this stays one popup.
        let playlists = self.addable_playlists(cx);
        if !playlists.is_empty() {
            entries.push(MenuEntry::Divider);
            entries.push(MenuEntry::Group("Add to a playlist".into()));
        }
        for (id, name) in playlists {
            let tracks = tracks.clone();
            entries.push(
                MenuEntry::item(
                    gpui::ElementId::Name(format!("queue-add-to-{id}").into()),
                    name,
                    on_root(cx, move |this, _window, cx| {
                        let tracks = tracks.clone();
                        this.library
                            .update(cx, |library, cx| library.add_to_playlist(id, tracks, cx));
                    }),
                )
                .icon("playlist")
                .disabled(empty),
            );
        }

        entries
    }

    /// Asks for a file and writes those tracks out as a playlist. The queue is
    /// an order rather than a saved list, so there is no playlist id to export
    /// from — the tracks go straight to the writer.
    fn export_tracks(&self, tracks: Vec<TrackId>, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        let path = cx.prompt_for_new_path(&library::data_dir(), None);
        let library = self.library.clone();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            // m3u8 is what the menu offers, so it is what an unsuffixed name
            // gets.
            let path = match path.extension().is_some() {
                true => path,
                false => path.with_extension("m3u8"),
            };
            cx.update(|cx| {
                library.update(cx, |library, cx| library.export_tracks(tracks, path, cx));
            });
        })
        .detach();
    }

    fn track_menu(&self, id: TrackId, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let mut entries = vec![
            MenuEntry::item(
                "play",
                "Play",
                on_root(cx, move |this, _window, cx| {
                    this.player.update(cx, |player, cx| player.play_now(id, cx));
                }),
            )
            .icon("play"),
            MenuEntry::item(
                "play-next",
                "Play Next",
                on_root(cx, move |this, _window, cx| {
                    this.player
                        .update(cx, |player, cx| player.play_next(&[id], cx));
                }),
            )
            .icon("queue"),
            MenuEntry::item(
                "enqueue",
                "Add to Queue",
                on_root(cx, move |this, _window, cx| {
                    this.player
                        .update(cx, |player, cx| player.enqueue(&[id], cx));
                }),
            )
            .icon("queue"),
            MenuEntry::item(
                "play-similar",
                "Play Similar Tracks",
                on_root(cx, move |this, _window, cx| {
                    this.player
                        .update(cx, |player, cx| player.play_similar(id, cx));
                }),
            )
            .icon("shuffle"),
        ];

        let playlists = self.addable_playlists(cx);
        // Only when there is something to list: with no playlists made yet, the
        // heading would stand over nothing.
        if !playlists.is_empty() {
            entries.push(MenuEntry::Divider);
            entries.push(MenuEntry::Group("Add to playlist".into()));
        }

        // Ticked where the song is already in the list, so adding it twice is a
        // decision rather than a surprise.
        let holding = self.library.read(cx).playlists_with(id);
        for (list_id, name) in playlists {
            entries.push(
                MenuEntry::item(
                    gpui::ElementId::Name(format!("add-to-{list_id}").into()),
                    name,
                    on_root(cx, move |this, _window, cx| {
                        this.library.update(cx, |library, cx| {
                            library.add_to_playlist(list_id, vec![id], cx)
                        });
                    }),
                )
                .icon("playlist")
                .checked(holding.contains(&list_id)),
            );
        }

        let favorite = self.is_favorite(id, cx);

        // Only where there is something to forget, so the menu does not offer
        // to undo a setting this song has never had.
        if self.player.read(cx).remembers_audio_for(id) {
            entries.extend([
                MenuEntry::Divider,
                MenuEntry::item(
                    "forget-audio",
                    "Forget this song's volume and EQ",
                    on_root(cx, move |this, _window, cx| {
                        this.player
                            .update(cx, |player, cx| player.forget_track_audio(id, cx));
                    }),
                )
                .icon("volume"),
            ]);
        }

        entries.extend([
            MenuEntry::Divider,
            MenuEntry::item(
                "reveal",
                "Open File Location",
                on_root(cx, move |this, _window, cx| this.reveal(id, cx)),
            )
            .icon("folder"),
            MenuEntry::item(
                "edit",
                "Edit Metadata",
                on_root(cx, move |this, _window, cx| {
                    let tracks = this.selection_or(id, cx);
                    this.show(Dialog::EditMetadata(tracks), cx);
                }),
            )
            .icon("edit"),
            MenuEntry::item(
                "media-info",
                "Media Info",
                on_root(cx, move |this, _window, cx| {
                    this.show(Dialog::MediaInfo(id), cx);
                }),
            )
            .icon("info"),
            MenuEntry::Divider,
            MenuEntry::item(
                "favorite",
                match favorite {
                    true => "Remove from Favorites",
                    false => "Favorite",
                },
                on_root(cx, move |this, _window, cx| this.toggle_favorite(id, cx)),
            )
            .icon(match favorite {
                true => "heart-filled",
                false => "heart",
            }),
        ]);

        entries.extend([
            MenuEntry::Divider,
            MenuEntry::item(
                "forget",
                "Remove from Library",
                on_root(cx, move |this, _window, cx| {
                    this.library
                        .update(cx, |library, cx| library.forget_track(id, cx));
                }),
            )
            // A minus rather than a bin: this drops the row and leaves the file
            // alone, which is the whole of what separates it from the entry
            // below.
            .icon("minus"),
            MenuEntry::item(
                "delete",
                "Delete File",
                on_root(cx, move |this, _window, cx| {
                    // Never without an explicit confirmation.
                    this.show(Dialog::ConfirmDelete(vec![id]), cx);
                }),
            )
            .icon("trash")
            .danger(),
        ]);

        entries
    }

    /// The playlists an "Add to playlist" group should offer.
    ///
    /// Favorites is left out: it is the heart on the row, not a list you put
    /// things into, and the seeded playlist row behind it holds no tracks. It
    /// showed up here as an ordinary playlist, which is exactly the confusion
    /// the sidebar already stopped presenting.
    ///
    /// Smart playlists are left out for the same reason and a stronger one:
    /// nothing reads the rows an "add" would write, so the song would appear
    /// to be added and then simply not be there.
    fn addable_playlists(&self, cx: &App) -> Vec<(i64, String)> {
        self.library
            .read(cx)
            .playlists()
            .iter()
            .filter(|list| {
                !matches!(
                    list.kind,
                    library::models::PlaylistKind::Favorites | library::models::PlaylistKind::Smart
                )
            })
            .map(|list| (list.id, list.name.clone()))
            .collect()
    }

    fn playlist_menu(&self, id: i64, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let kind = self
            .library
            .read(cx)
            .playlists()
            .iter()
            .find(|list| list.id == id)
            .map(|list| list.kind);
        let protected = kind == Some(library::models::PlaylistKind::Favorites);
        let smart = kind == Some(library::models::PlaylistKind::Smart);

        let mut entries = vec![
            MenuEntry::item(
                "open",
                "Open",
                on_root(cx, move |this, _window, cx| {
                    this.go(Screen::Tracks(LibraryView::Playlist(id)), cx);
                }),
            )
            .icon("playlist"),
            MenuEntry::item(
                "rename",
                "Rename",
                on_root(cx, move |this, _window, cx| {
                    this.show(Dialog::RenamePlaylist(id), cx)
                }),
            )
            .icon("edit")
            .disabled(protected),
            MenuEntry::item(
                "duplicate",
                "Duplicate",
                on_root(cx, move |this, _window, cx| {
                    let name = this
                        .library
                        .read(cx)
                        .playlists()
                        .iter()
                        .find(|list| list.id == id)
                        .map(|list| format!("{} copy", list.name))
                        .unwrap_or_else(|| "Playlist copy".to_owned());
                    this.library
                        .update(cx, |library, cx| library.duplicate_playlist(id, name, cx));
                }),
            )
            .icon("copy")
            // Duplicating copies stored rows, of which a smart playlist has
            // none: the copy would be an empty ordinary playlist wearing the
            // same name. Copying the rules instead is a different feature, and
            // nobody has asked for it.
            .disabled(smart),
            MenuEntry::item(
                "export",
                "Export…",
                on_root(cx, move |this, _window, cx| this.export_playlist(id, cx)),
            )
            .icon("external"),
            MenuEntry::Divider,
            MenuEntry::item(
                "delete-playlist",
                "Delete Playlist",
                on_root(cx, move |this, _window, cx| {
                    this.library
                        .update(cx, |library, cx| library.delete_playlist(id, cx));
                }),
            )
            .icon("trash")
            .danger()
            .disabled(protected),
        ];

        // A smart playlist's contents are its rules, so "edit" means the rules
        // rather than the track list — and it is the only way back to them.
        if smart {
            entries.insert(
                2,
                MenuEntry::item(
                    "edit-rules",
                    "Edit Rules…",
                    on_root(cx, move |this, _window, cx| {
                        this.show(Dialog::SmartPlaylist(Some(id)), cx)
                    }),
                )
                .icon("equalizer"),
            );
        }
        entries
    }

    fn folder_menu(&self, id: i64, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        vec![
            MenuEntry::item(
                "open-folder",
                "Show Tracks",
                on_root(cx, move |this, _window, cx| {
                    this.go(Screen::Tracks(LibraryView::Folder(id)), cx);
                }),
            )
            .icon("folder"),
            MenuEntry::item(
                "open-folder-in-explorer",
                "Open Folder",
                on_root(cx, move |this, _window, cx| this.open_folder_path(id, cx)),
            )
            .icon("external"),
            MenuEntry::item(
                "rescan-folder",
                "Rescan",
                on_root(cx, |this, _window, cx| {
                    this.library.update(cx, |library, cx| library.rescan(cx));
                }),
            )
            .icon("refresh"),
            MenuEntry::Divider,
            MenuEntry::item(
                "remove-folder",
                "Remove from Library",
                on_root(cx, move |this, _window, cx| {
                    this.library
                        .update(cx, |library, cx| library.remove_folder(id, cx));
                }),
            )
            .icon("trash")
            .danger(),
        ]
    }

    // Buttons in the chrome run the same code paths as the keyboard shortcuts,
    // so the two can never drift apart.

    pub fn dispatch_shuffle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_shuffle(&ToggleShuffle, window, cx);
    }

    pub fn dispatch_repeat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_repeat(&CycleRepeat, window, cx);
    }

    pub fn dispatch_open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_folder(&OpenFolder, window, cx);
    }

    /// Steps through the offered speeds and wraps, so one button covers them all.
    pub fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let current = self.player.read(cx).speed();
        let next = audio::SPEEDS
            .iter()
            .copied()
            .find(|speed| *speed > current + 0.01)
            .unwrap_or(audio::SPEEDS[0]);
        self.player
            .update(cx, |player, cx| player.set_speed(next, cx));
        self.settings
            .update(cx, |store, cx| store.update(|s| s.speed = next, cx));
    }

    /// The selection if `id` is part of it, otherwise just `id`.
    pub fn selection_or(&self, id: TrackId, cx: &App) -> Vec<TrackId> {
        let library = self.library.read(cx);
        let selected: Vec<TrackId> = self
            .selection
            .iter()
            .filter_map(|index| library.ids().get(*index).copied())
            .collect();
        match selected.contains(&id) {
            true => selected,
            false => vec![id],
        }
    }

    fn reveal(&self, id: TrackId, cx: &mut Context<Self>) {
        let Some(track) = self.library.read(cx).track(id) else {
            return;
        };
        if let Some(folder) = track.path.parent() {
            cx.reveal_path(&track.path.clone());
            let _ = folder;
        }
    }

    fn export_playlist(&self, id: i64, cx: &mut Context<Self>) {
        let path = cx.prompt_for_new_path(&library::data_dir(), None);
        let library = self.library.clone();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            // Default to the format everything reads.
            let path = match path.extension().is_some() {
                true => path,
                false => path.with_extension("m3u8"),
            };
            cx.update(|cx| {
                library.update(cx, |library, cx| library.export_playlist(id, path, cx));
            });
        })
        .detach();
    }
}

/// Moves a scroll position between two lists of the same thing drawn at
/// different row heights.
///
/// The offset is converted through "rows from the top" rather than copied:
/// `-offset.y / row` is where the list is in rows, and multiplying by the other
/// list's row height puts it in the same place there. The offset is set on the
/// handle directly rather than through `scroll_to_item`, which rounds to a row
/// boundary — the point here is that nothing visibly moves.
///
/// A list that has never been drawn has no offset to read, which reads as zero:
/// the top, which is where it would have started anyway.
fn carry_scroll(
    from: &UniformListScrollHandle,
    to: &UniformListScrollHandle,
    from_row: Pixels,
    to_row: Pixels,
) {
    if from_row <= gpui::px(0.) || to_row <= gpui::px(0.) {
        return;
    }

    let offset = from.0.borrow().base_handle.offset();
    let target = to.0.borrow().base_handle.clone();
    target.set_offset(gpui::point(
        offset.x,
        converted_offset(offset.y, from_row, to_row),
    ));
    // Any scroll asked for earlier and not yet drawn would overwrite this on the
    // next frame.
    to.0.borrow_mut().deferred_scroll_to_item = None;
}

/// Where a scroll offset lands in a list whose rows are a different height.
///
/// Offsets are negative going down, which is why the sign is flipped twice: once
/// to get a distance in rows, once to put it back.
fn converted_offset(offset_y: Pixels, from_row: Pixels, to_row: Pixels) -> Pixels {
    -(to_row * (-offset_y / from_row))
}

/// The window title for what is playing. Pure, so it can be tested without a
/// window and called from wherever the fact changes.
pub fn title_for(song: Option<String>, artist: Option<String>) -> String {
    match (song, artist) {
        (Some(song), Some(artist)) => format!("{song} — {artist} — Tinnitus"),
        (Some(song), None) => format!("{song} — Tinnitus"),
        (None, _) => "Tinnitus".to_owned(),
    }
}

/// Bridges a `Root` method onto a plain `Fn(&mut Window, &mut App)` callback.
///
/// Menu entries have no event to hand back, so `cx.listener` — which always
/// passes one — does not fit. This captures the entity instead.
pub fn on_root(
    cx: &mut Context<Root>,
    body: impl Fn(&mut Root, &mut Window, &mut Context<Root>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let root = cx.entity();
    move |window: &mut Window, cx: &mut App| {
        root.update(cx, |root, cx| body(root, window, cx));
    }
}

/// What clicking row `index` does to the selection.
///
/// Plain click replaces, ctrl-click toggles, shift-click extends from the
/// anchor. Split out from the view so the rules can be tested without a window.
pub fn select_indices(
    current: HashSet<usize>,
    anchor: Option<usize>,
    index: usize,
    extend: bool,
    toggle: bool,
) -> (HashSet<usize>, Option<usize>) {
    match (extend, toggle, anchor) {
        // Shift-click: everything between the anchor and here, in either
        // direction. The anchor stays put so a second shift-click re-extends.
        (true, _, Some(anchor)) => {
            let (low, high) = match anchor <= index {
                true => (anchor, index),
                false => (index, anchor),
            };
            ((low..=high).collect(), Some(anchor))
        }
        (_, true, _) => {
            let mut selection = current;
            if !selection.remove(&index) {
                selection.insert(index);
            }
            (selection, Some(index))
        }
        _ => (HashSet::from([index]), Some(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_title_names_the_song() {
        assert_eq!(
            title_for(Some("Teardrop".into()), Some("Massive Attack".into())),
            "Teardrop — Massive Attack — Tinnitus"
        );
        // A loose file has no row, so no artist.
        assert_eq!(
            title_for(Some("Teardrop".into()), None),
            "Teardrop — Tinnitus"
        );
        // Stopped: the app's own name, and no stray dash.
        assert_eq!(title_for(None, None), "Tinnitus");
        assert_eq!(title_for(None, Some("Massive Attack".into())), "Tinnitus");
    }

    #[test]
    fn collapsing_the_queue_keeps_the_place_in_the_list() {
        use gpui::px;

        // Thirty rows down a 44px panel, in a strip whose rows are 60px: the
        // same thirty rows down, not the top.
        assert_eq!(
            converted_offset(px(-30. * 44.), px(44.), px(60.)),
            px(-30. * 60.)
        );
        // And back again, landing exactly where it started.
        assert_eq!(
            converted_offset(px(-30. * 60.), px(60.), px(44.)),
            px(-30. * 44.)
        );
    }

    #[test]
    fn a_part_scrolled_row_stays_part_scrolled() {
        use gpui::px;

        // Half a row down is half a row down in the other list too, so nothing
        // snaps to a row edge on the way across.
        assert_eq!(
            converted_offset(px(-1.5 * 44.), px(44.), px(60.)),
            px(-1.5 * 60.)
        );
        // The top is the top whatever the row heights are.
        assert_eq!(converted_offset(px(0.), px(44.), px(60.)), px(0.));
    }

    #[test]
    fn a_plain_click_replaces_the_selection() {
        let (selection, anchor) =
            select_indices(HashSet::from([1, 2, 3]), Some(1), 7, false, false);
        assert_eq!(selection, HashSet::from([7]));
        assert_eq!(anchor, Some(7));
    }

    #[test]
    fn ctrl_click_toggles_one_row_at_a_time() {
        let (selection, _) = select_indices(HashSet::from([1, 2]), Some(1), 5, false, true);
        assert_eq!(selection, HashSet::from([1, 2, 5]));

        let (selection, _) = select_indices(selection, Some(5), 2, false, true);
        assert_eq!(selection, HashSet::from([1, 5]));
    }

    #[test]
    fn shift_click_extends_in_both_directions() {
        let (down, _) = select_indices(HashSet::from([3]), Some(3), 6, true, false);
        assert_eq!(down, HashSet::from([3, 4, 5, 6]));

        let (up, anchor) = select_indices(HashSet::from([3]), Some(3), 0, true, false);
        assert_eq!(up, HashSet::from([0, 1, 2, 3]));
        // The anchor does not move, so extending again works from the same spot.
        assert_eq!(anchor, Some(3));
    }

    #[test]
    fn shift_click_without_an_anchor_behaves_like_a_plain_click() {
        let (selection, anchor) = select_indices(HashSet::from([1, 2]), None, 9, true, false);
        assert_eq!(selection, HashSet::from([9]));
        assert_eq!(anchor, Some(9));
    }
}
