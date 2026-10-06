//! The main content area: track lists, the album grid, the queue, search
//! results, settings and the equalizer.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, MouseButton, Pixels, SharedString, Window, div, img, px, relative,
    svg, uniform_list,
};
use library::ThumbSize;
use library::models::{LibraryView, Track};
use ui::table::{Column, Width, sized};
use ui::{
    ActiveTheme as _, Button, Scrollbar, Slider, Table, Vacancy, clock, eyebrow, faint, heading,
    scrolled,
};

use crate::dialogs::Dialog;
use crate::root::{MenuTarget, Root};
use crate::screen::{Screen, SearchFilter};

pub fn content(root: &Root, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let background = theme.background;

    let body = match root.history.current().clone() {
        Screen::Home => home(root, window, cx),
        Screen::Tracks(view) => tracks(root, &view, cx),
        Screen::Albums => albums(root, window, cx),
        Screen::Artists => artists(root, window, cx),
        Screen::Genres => genres(root, cx),
        Screen::Years => years(root, cx),
        Screen::Folders => folders(root, cx),
        Screen::Playlists => playlists(root, cx),
        Screen::Queue => queue(root, cx),
        Screen::Search => search(root, cx),
        Screen::Settings => crate::dialogs::settings_screen(root, window, cx),
        Screen::Equalizer => crate::dialogs::equalizer_screen(root, cx),
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .bg(background)
        .child(body)
        .into_any_element()
}

/// The header over a list: what it is, and what to do with all of it.
fn header(
    title: String,
    subtitle: String,
    playable: bool,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    header_with(title, subtitle, playable, None, cx)
}

/// The same header, plus the one button that belongs to this particular list —
/// "Add Folder" over the folders, and so on. It sits top right, where a list's
/// own action is looked for.
fn header_with(
    title: String,
    subtitle: String,
    playable: bool,
    action: Option<AnyElement>,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let theme = cx.theme().clone();

    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(theme.metrics.gap)
        .px(theme.metrics.inset)
        .py(theme.metrics.pad)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(heading(title, cx).truncate())
                .child(faint(subtitle, cx)),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .when(playable, |this| {
                    this.child(
                        Button::new("play-all")
                            .primary()
                            .icon("play")
                            .label("Play")
                            .on_click(cx.listener(|this, _, _window, cx| this.play_index(0, cx))),
                    )
                    .child(
                        Button::new("queue-all")
                            .icon("queue")
                            .label("Queue")
                            .on_click(cx.listener(|this, _, _window, cx| {
                                let ids = this.library.read(cx).ids().to_vec();
                                this.player
                                    .update(cx, |player, cx| player.enqueue(&ids, cx));
                            })),
                    )
                })
                .when_some(action, |this, action| this.child(action)),
        )
}

/// The columns the user has switched on, in a fixed order.
fn columns(root: &Root, cx: &App) -> Vec<Column> {
    let wanted = &root.settings.read(cx).get().columns;
    let all = [
        ("art", Column::new("art", "", Width::Fixed(px(44.))).fixed()),
        // The flexible shares add up to 1.0, so they divide the free space
        // exactly; anything less leaves a dead gap before the fixed columns.
        ("title", Column::new("title", "Title", Width::Flex(0.52))),
        ("artist", Column::new("artist", "Artist", Width::Flex(0.24))),
        ("album", Column::new("album", "Album", Width::Flex(0.24))),
        (
            "year",
            Column::new("year", "Year", Width::Fixed(px(64.))).numeric(),
        ),
        (
            "plays",
            Column::new("plays", "Plays", Width::Fixed(px(64.))).numeric(),
        ),
        (
            "duration",
            Column::new("duration", "Duration", Width::Fixed(px(80.))).numeric(),
        ),
        // A real column rather than something tacked on the end of the row, so
        // the header reserves the same width the rows spend and the two stay
        // lined up.
        (
            "favorite",
            Column::new("favorite", "", Width::Fixed(px(36.))).fixed(),
        ),
    ];

    all.into_iter()
        .filter(|(key, _)| wanted.iter().any(|chosen| chosen == key))
        .map(|(_, column)| column)
        .collect()
}

fn tracks(root: &Root, view: &LibraryView, cx: &mut Context<Root>) -> AnyElement {
    let library = root.library.read(cx);
    let total = library.len();
    let scanning = library.scan_progress().running;
    let sort = library.sort();

    if total == 0 {
        // Only All Songs being empty means there is no music. Every other view
        // is a slice of the library, and offering to add a folder because a
        // slice came back empty answers a question the user did not ask.
        return match view {
            LibraryView::AllSongs => empty_library(root, scanning, cx).into_any_element(),
            _ => empty_view(view, scanning).into_any_element(),
        };
    }

    let title = tracks_title(root, view, cx);
    let subtitle = match total {
        1 => "1 track".to_owned(),
        _ => format!("{total} tracks"),
    };
    let cols = columns(root, cx);
    let sort_key = sort_column_key(sort.key);

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(header(title, subtitle, true, cx))
        .child(
            Table::new("tracks", total, {
                let root = cx.entity();
                let cols = cols.clone();
                move |index, _window, cx| {
                    root.update(cx, |root, cx| track_row(root, index, &cols, cx))
                }
            })
            .columns(cols)
            .track_scroll(&root.list_scroll("tracks"))
            .sorted(sort_key, sort.descending)
            .on_sort(cx.listener(|this, key: &gpui::SharedString, _window, cx| {
                let key = key.to_owned();
                this.library
                    .update(cx, |library, cx| library.sort_by(&key, cx));
            })),
        )
        .into_any_element()
}

/// One row of the track list. Asks the library for the row, and asks it to fetch
/// the surrounding page if it is not there yet.
fn track_row(root: &mut Root, index: usize, cols: &[Column], cx: &mut Context<Root>) -> AnyElement {
    // Tell the library what is on screen so it can page ahead.
    root.library.update(cx, |library, cx| {
        library.ensure(index.saturating_sub(20)..index + 40, cx)
    });

    let theme = cx.theme().clone();
    let row_height = theme.metrics.list_row;
    let inset = theme.metrics.inset;
    let gap = theme.metrics.gap;
    let selected_bg = theme.selected;
    let hover = theme.hover;
    let accent = theme.accent;
    // The accent, thinned to a wash: strong enough to pick the row out, quiet
    // enough to read the words on top of it.
    let playing_bg = theme.accent.opacity(0.18);
    let faint_color = theme.faint_foreground;
    let radius = theme.radius;
    let thumb = theme.metrics.thumb;

    let selected = root.selection.contains(&index);
    let library = root.library.read(cx);
    let Some(track) = library.row(index).cloned() else {
        // A placeholder of the right height keeps the scrollbar honest while the
        // page loads.
        return div()
            .h(row_height)
            .px(inset)
            .flex()
            .items_center()
            .child(faint("…", cx))
            .into_any_element();
    };

    let playing = root
        .player
        .read(cx)
        .current()
        .map(|current| current.id == track.id)
        .unwrap_or(false);
    let artwork = library.artwork_path(&track, ThumbSize::Small);
    let id = track.id;

    div()
        .id(SharedString::from(format!("track-{index}")))
        .flex()
        .items_center()
        .gap(gap)
        .h(row_height)
        // A list item sizes to its content unless told otherwise, and with no
        // width there is no free space for the flexible columns to divide —
        // they collapse and only the fixed-width cells appear.
        .w_full()
        .px(inset)
        .when(selected, |this| this.bg(selected_bg))
        // The song that is playing gets the whole row, not just a coloured
        // title: in a list of a thousand rows that has to be findable at a
        // glance. Drawn under the selection, since the two can be the same row.
        .when(playing, |this| this.bg(playing_bg))
        .hover(move |style| style.bg(hover))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                this.select(
                    index,
                    event.modifiers.shift,
                    event.modifiers.control || event.modifiers.platform,
                    cx,
                );
                if event.click_count >= 2 {
                    this.play_index(index, cx);
                }
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                if !this.selection.contains(&index) {
                    this.select(index, false, false, cx);
                }
                this.open_menu(event.position, MenuTarget::Track(id), cx);
            }),
        )
        .children(cols.iter().map(|column| {
            let cell = div().flex().items_center().min_w_0().h_full();
            let cell = match column.key.as_ref() {
                "art" => cell.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(thumb)
                        .rounded(radius)
                        .bg(theme.raised)
                        .overflow_hidden()
                        .map(|this| match artwork.clone() {
                            Some(path) => this.child(img(path).size(thumb).rounded(radius)),
                            None => this.child(
                                svg()
                                    .path(icons::path("music"))
                                    .size(px(14.))
                                    .text_color(faint_color),
                            ),
                        }),
                ),
                // Text cells take the width their column was given. Without
                // `flex_1` + `min_w_0` the label shrinks to nothing inside a
                // zero-basis cell and the row renders blank.
                "title" => cell.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(playing, |this| this.text_color(accent))
                        .child(SharedString::from(track.title.clone())),
                ),
                "artist" => cell.child(faint(dash_if_empty(&track.artist), cx).flex_1().min_w_0()),
                "album" => cell.child(
                    faint(track.album.clone().unwrap_or_else(|| "—".to_owned()), cx)
                        .flex_1()
                        .min_w_0(),
                ),
                "year" => cell.child(
                    faint(
                        track
                            .year
                            .map(|year| year.to_string())
                            .unwrap_or_else(|| "—".to_owned()),
                        cx,
                    )
                    .flex_1()
                    .min_w_0(),
                ),
                "plays" => cell.child(faint(track.play_count.to_string(), cx).flex_1().min_w_0()),
                "duration" => cell.child(faint(clock(track.duration), cx).flex_1().min_w_0()),
                "favorite" => cell.justify_center().child(favorite_button(
                    ("row-favorite", index as u64),
                    id,
                    track.favorite,
                    cx,
                )),
                _ => cell,
            };
            sized(cell, column)
        }))
        .into_any_element()
}

/// The heart that rides on a track row, in every list that shows one.
///
/// `Button` stops the press reaching what is underneath, so clicking the heart
/// on a row neither selects nor plays it.
fn favorite_button(
    id: impl Into<gpui::ElementId>,
    track: library::models::TrackId,
    favorite: bool,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let theme = cx.theme().clone();

    Button::new(id)
        .ghost()
        .small()
        .icon(match favorite {
            true => "heart-filled",
            false => "heart",
        })
        .tint(match favorite {
            true => theme.accent,
            // Unset hearts stay quiet: one per row, and they are not what the
            // eye is here to read.
            false => theme.faint_foreground,
        })
        .on_click(cx.listener(move |this, _, _window, cx| this.toggle_favorite(track, cx)))
}

/// The page title for a track list.
///
/// A folder or playlist page names what it is showing: every one of them is
/// otherwise headed by the same word, which tells the user nothing about where
/// they landed. The rest of the views already name themselves.
fn tracks_title(root: &Root, view: &LibraryView, cx: &Context<Root>) -> String {
    let base = crate::screen::Screen::Tracks(view.clone()).title();
    let library = root.library.read(cx);
    let name = match view {
        LibraryView::Folder(id) => library
            .folders()
            .iter()
            .find(|folder| folder.id == *id)
            .map(|folder| folder_name(&folder.path)),
        LibraryView::Playlist(id) => library
            .playlists()
            .iter()
            .find(|list| list.id == *id)
            .map(|list| list.name.clone()),
        _ => None,
    };
    match name {
        Some(name) => format!("{base} - {name}"),
        None => base,
    }
}

/// What a watched folder is called: its own name, falling back to the whole
/// path for a drive root, which has no file name of its own.
pub fn folder_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// An em-dash for a field the file did not fill in, so a column of untagged
/// tracks reads as "nothing here" rather than as a rendering fault.
fn dash_if_empty(value: &str) -> String {
    match value.trim().is_empty() {
        true => "—".to_owned(),
        false => value.to_owned(),
    }
}

/// A view of the library that happens to hold nothing.
///
/// It says what would put something here, which for most of these is simply
/// using the app. Notably it offers no "Add Music Folder": the library is not
/// empty, this slice of it is, and a folder button would be answering the wrong
/// question — an empty Favorites means nothing has been hearted yet.
fn empty_view(view: &LibraryView, scanning: bool) -> impl IntoElement {
    if scanning {
        return Vacancy::new("music", "Scanning your music…")
            .detail("Tracks appear as they are found. You can keep using the app.");
    }

    let (icon, title, detail) = match view {
        LibraryView::Favorites => (
            "heart",
            "No favourites yet",
            "Click the heart beside a song to keep it here.",
        ),
        LibraryView::MostPlayed => (
            "trending",
            "Nothing played yet",
            "Songs arrive here once you have played them a few times.",
        ),
        LibraryView::RecentlyPlayed => (
            "clock",
            "Nothing played yet",
            "The songs you play most recently show up here.",
        ),
        LibraryView::RecentlyAdded => (
            "clock",
            "Nothing added yet",
            "Newly scanned songs show up here.",
        ),
        LibraryView::Playlist(_) => (
            "playlist",
            "This playlist is empty",
            "Right-click a song and use Add to playlist.",
        ),
        LibraryView::Folder(_) => (
            "folder",
            "Nothing in this folder",
            "No playable audio was found here.",
        ),
        _ => ("music", "Nothing here", "This list has no songs in it."),
    };

    Vacancy::new(icon, title).detail(detail)
}

fn empty_library(root: &Root, scanning: bool, cx: &mut Context<Root>) -> impl IntoElement {
    let has_folders = !root.library.read(cx).folders().is_empty();

    let vacancy = Vacancy::new(
        "music",
        match scanning {
            true => "Scanning your music…",
            false => "No music yet",
        },
    )
    .detail(match (scanning, has_folders) {
        (true, _) => "Tracks appear as they are found. You can keep using the app.",
        (false, true) => "Nothing was found in the folders you added.",
        (false, false) => "Point Tinnitus at a folder and it will do the rest.",
    });

    match scanning {
        true => vacancy,
        false => vacancy.action(
            Button::new("choose-folder")
                .primary()
                .icon("folder")
                .label("Add Music Folder")
                .on_click(cx.listener(|this, _, window, cx| this.dispatch_open_folder(window, cx))),
        ),
    }
}

// -- home -----------------------------------------------------------------

/// The landing page: what is playing, how big the library is, and the handful of
/// places most sessions start from.
fn home(root: &Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let card = home_card_width(root, window, cx);
    let library = root.library.read(cx);
    let (albums, artists, genres, folders) = (
        library.albums().len(),
        library.artists().len(),
        library.genres().len(),
        library.folders().len(),
    );
    // Only the ones the user actually made. Counting Favorites and the
    // automatic smart playlists told a library with no playlists in it that it
    // had five.
    let playlists = library
        .playlists()
        .iter()
        .filter(|list| list.kind == library::models::PlaylistKind::Custom)
        .count();
    let scanning = library.scan_progress().running;
    let playing = root.player.read(cx).now_playing();
    let detail = root.player.read(cx).now_playing_detail();
    let scroll = root.area_scroll("home");

    let stats: Vec<(&'static str, String, Screen)> = vec![
        ("album", format!("{albums} albums"), Screen::Albums),
        ("artist", format!("{artists} artists"), Screen::Artists),
        ("genre", format!("{genres} genres"), Screen::Genres),
        (
            "playlist",
            format!("{playlists} playlists"),
            // The Playlists page, now that there is one. This used to open
            // Favorites, which is not what the tile says.
            Screen::Playlists,
        ),
        ("folder", format!("{folders} folders"), Screen::Folders),
    ];

    let pane =
        div()
            .id("home")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(inset)
            .p(inset)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            // No heading: the app names itself in the corner of the title bar,
            // and Home opens with what is actually happening instead.
            .child(faint(
                match (&playing, scanning) {
                    (_, true) => "Scanning your music. Everything else keeps working.".to_owned(),
                    (Some(title), false) => match &detail {
                        Some(artist) => format!("Playing {title} — {artist}"),
                        None => format!("Playing {title}"),
                    },
                    (None, false) => "Nothing playing yet.".to_owned(),
                },
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(
                        Button::new("home-play")
                            .primary()
                            .icon("play")
                            .label("Play All Songs")
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.go(Screen::Tracks(LibraryView::AllSongs), cx);
                                this.play_index(0, cx);
                            })),
                    )
                    .child(
                        Button::new("home-shuffle")
                            .icon("shuffle")
                            .label("Shuffle All")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.go(Screen::Tracks(LibraryView::AllSongs), cx);
                                if !this.player.read(cx).shuffle() {
                                    this.dispatch_shuffle(window, cx);
                                }
                                this.shuffle_all(cx);
                            })),
                    )
                    .child(
                        Button::new("home-add-folder")
                            .icon("plus")
                            .label("Add Folder")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dispatch_open_folder(window, cx)
                            })),
                    ),
            )
            .child(eyebrow("Your library", cx))
            .child(
                div()
                    .flex()
                    .gap(theme.metrics.gap)
                    .children(stats.into_iter().map(|(icon, label, screen)| {
                        let raised = theme.raised;
                        let hover = theme.hover;
                        div()
                            .id(SharedString::from(format!("home-{icon}")))
                            .flex()
                            // One row of five that shares the width, rather than
                            // fixed-width cards that wrap the last one down.
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(theme.metrics.gap)
                            .h(px(64.))
                            .px(theme.metrics.inset)
                            .rounded(theme.radius)
                            .bg(raised)
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(
                                svg()
                                    .path(icons::path(icon))
                                    .size(px(18.))
                                    .flex_none()
                                    .text_color(theme.accent),
                            )
                            .child(div().min_w_0().truncate().child(SharedString::from(label)))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.go(screen.clone(), cx);
                            }))
                    })),
            )
            // Most Played sits above the two recent lists: it used to be a sidebar
            // entry, and it is the one of the three that is worth a glance first.
            .children(recent_section(
                root,
                card,
                "Most Played",
                root.library.read(cx).most_played().to_vec(),
                LibraryView::MostPlayed,
                cx,
            ))
            .children(recent_section(
                root,
                card,
                "Recently Played",
                root.library.read(cx).recent_played().to_vec(),
                LibraryView::RecentlyPlayed,
                cx,
            ))
            .children(recent_section(
                root,
                card,
                "Recently Added",
                root.library.read(cx).recent_added().to_vec(),
                LibraryView::RecentlyAdded,
                cx,
            ));

    scrolled(pane, Scrollbar::area("home-scrollbar", &scroll)).into_any_element()
}

/// One of Home's short track lists. Empty until there is something to show: a
/// heading over nothing reads as a fault.
fn recent_section(
    root: &Root,
    card: gpui::Pixels,
    title: &'static str,
    tracks: Vec<Track>,
    view: LibraryView,
    cx: &mut Context<Root>,
) -> Vec<AnyElement> {
    if tracks.is_empty() {
        return vec![];
    }

    let gap = cx.theme().metrics.gap;
    let ids: Vec<_> = tracks.iter().map(|track| track.id).collect();
    // Collected rather than left lazy: the closures borrow `cx`, and the
    // heading below needs it back.
    let cards: Vec<AnyElement> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let ids = ids.clone();
            // Playing from here queues the whole list, so the rest of it
            // follows rather than playback stopping after one track.
            home_card(root, card, title, index, track, cx)
                .on_click(cx.listener(move |this, _, _window, cx| {
                    let ids = ids.clone();
                    this.player
                        .update(cx, |player, cx| player.play_all(ids, index, cx));
                }))
                .into_any_element()
        })
        .collect();

    vec![
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(eyebrow(title, cx))
            .child(
                Button::new(SharedString::from(format!("home-more-{title}")))
                    .ghost()
                    .label("See all")
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.go(Screen::Tracks(view.clone()), cx);
                    })),
            )
            .into_any_element(),
        // Three to a row on a normal window, fewer as it narrows: the cards
        // share out the width rather than being pinned to a column count that
        // only looks right at one size.
        div()
            .flex()
            .flex_wrap()
            .gap(gap)
            .children(cards)
            .into_any_element(),
    ]
}

/// The narrowest a Home card is allowed to get. Below this the title and artist
/// stop being readable beside the artwork, so a column is dropped instead.
const HOME_CARD_MIN: gpui::Pixels = px(260.);

/// Most columns of Home cards, however wide the window gets.
const HOME_COLUMNS: f32 = 3.;

/// How much width the content pane actually has: the window, less the sidebar
/// and queue beside it and the padding around it. Anything that lays cards out
/// in rows has to know this — a row that guesses wraps in the wrong place.
fn pane_width(root: &Root, window: &Window, cx: &App) -> gpui::Pixels {
    let theme = cx.theme();
    let settings = root.settings.read(cx).get();

    // The sidebar and the queue each take their column, plus the hairline
    // separator beside it.
    let mut available = window.viewport_size().width;
    available -= px(settings.sidebar_width) + px(1.);
    available -= px(1.)
        + match settings.queue_panel {
            true => px(272.),
            false => strip_art_size(theme) + theme.metrics.pad * 2.,
        };
    // The pane's own padding, and room for the scrollbar down the edge.
    available - (theme.metrics.inset * 2. + px(12.))
}

/// How wide one Home card should be.
///
/// Measured from the window rather than guessed: a fixed width has to assume a
/// screen size, and assuming wrong is what left one card per row with the rest
/// of the space empty. Every card in a section gets the same width whatever row
/// it lands in, which is what stops a short last row drawing outsized cards.
fn home_card_width(root: &Root, window: &Window, cx: &App) -> gpui::Pixels {
    let theme = cx.theme();
    let gap = theme.metrics.gap;
    let available = pane_width(root, window, cx);

    let columns = ((available + gap) / (HOME_CARD_MIN + gap))
        .floor()
        .clamp(1., HOME_COLUMNS);
    ((available - gap * (columns - 1.)) * (1. / columns)).max(HOME_CARD_MIN)
}

/// One card of a Home list: artwork, the title over the artist, and the heart.
///
/// The id is the section plus the position, never the song: a track that is both
/// most played and recently played appears in two of these lists, and two
/// elements sharing an id is one element as far as GPUI is concerned — the cards
/// stop responding to clicks. That is what made Recently Played look dead.
fn home_card(
    root: &Root,
    width: gpui::Pixels,
    section: &'static str,
    index: usize,
    track: &Track,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme().clone();
    let (raised, hover, faint_color, radius) = (
        theme.raised,
        theme.hover,
        theme.faint_foreground,
        theme.radius,
    );
    // Bigger than a list thumbnail: on a card the art is the thing being
    // recognised, not a marker beside the title.
    let art = theme.metrics.thumb * 1.6;
    let id = track.id;
    let artwork = root.library.read(cx).artwork_path(track, ThumbSize::Small);

    div()
        .id(SharedString::from(format!("home-{section}-{index}")))
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        // Every card the same width, worked out once from the window: a card
        // that sized itself to the row it landed in made the same song look
        // different depending on how many came before it.
        .flex_none()
        .w(width)
        .max_w_full()
        .p(px(6.))
        .rounded(radius)
        .bg(raised)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        // The same menu the track lists get. Home has no selection of its own,
        // so this acts on the one card rather than selecting it first.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                this.open_menu(event.position, MenuTarget::Track(id), cx);
            }),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(art)
                .rounded(radius)
                .bg(theme.background)
                .overflow_hidden()
                .map(|this| match artwork {
                    Some(path) => this.child(img(path).size(art).rounded(radius)),
                    None => this.child(
                        svg()
                            .path(icons::path("music"))
                            .size(px(20.))
                            .text_color(faint_color),
                    ),
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .child(SharedString::from(track.title.clone())),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(theme.text(ui::Text::Small))
                        .text_color(faint_color)
                        .child(SharedString::from(dash_if_empty(&track.artist))),
                ),
        )
        .child(favorite_button(
            SharedString::from(format!("home-favorite-{section}-{index}")),
            track.id,
            track.favorite,
            cx,
        ))
}

// -- groupings ------------------------------------------------------------

/// Edge length of an album cover in the grid. Sized so a maximised window fits
/// another column across than it did at 160.
const ALBUM_TILE: Pixels = px(140.);
/// Edge length of an artist portrait in the grid.
const ARTIST_TILE: Pixels = px(132.);
/// The gap between a tile's art and the hover highlight drawn around it. A card
/// is this much wider than its art on every side: the art is a fixed square, so
/// padding has to be paid for in width rather than taken out of it, or the art
/// hangs over the edge of its own highlight.
const TILE_PAD: Pixels = px(6.);

/// The cached thumbnail an album cover is drawn from.
const ALBUM_ART: ThumbSize = ThumbSize::Medium;

/// Height of the small grey lines under a card's title.
const CARD_SMALL: Pixels = px(15.);

/// Height of a card's title line. Follows the font size, since that is what the
/// user can change underneath it.
fn card_line(theme: &ui::Theme) -> Pixels {
    theme.font_size + px(6.)
}

/// Art, name, artist, track count — the whole album card, top to bottom.
///
/// Fixed rather than measured: a uniform list has to know how tall a row is
/// before it builds one, which is exactly what lets it skip the rows nobody can
/// see.
fn album_card_height(theme: &ui::Theme) -> Pixels {
    TILE_PAD * 2. + ALBUM_TILE + px(6.) * 3. + card_line(theme) + CARD_SMALL * 2.
}

/// The same for an artist: portrait, name, track count.
fn artist_card_height(theme: &ui::Theme) -> Pixels {
    TILE_PAD * 2. + ARTIST_TILE + px(6.) * 2. + card_line(theme) + CARD_SMALL
}

/// How many cards of `card` width fit across `available`, with `gap` between
/// them. At least one, however narrow the window gets.
fn grid_columns(available: Pixels, card: Pixels, gap: Pixels) -> usize {
    let fits = ((available + gap) / (card + gap)).floor();
    (fits as usize).max(1)
}

/// Albums as a grid of covers.
///
/// The grid is a uniform list of rows rather than a wrapping box: a thousand
/// albums is a thousand cards, and a wrapping box builds and lays out every one
/// of them on every frame, which is what made this page crawl. A row of cards is
/// always the same height, so the list can place row N without touching rows
/// before it, and only the rows on screen are ever built.
fn albums(root: &Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let count = root.library.read(cx).albums().len();
    if count == 0 {
        return Vacancy::new("album", "No albums yet").into_any_element();
    }

    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let columns = grid_columns(
        pane_width(root, window, cx),
        ALBUM_TILE + TILE_PAD * 2.,
        inset,
    );
    let rows = count.div_ceil(columns);
    let height = album_card_height(&theme) + inset;
    let scroll = root.list_scroll("albums");

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(header(
            "Albums".to_owned(),
            format!("{count} albums"),
            false,
            cx,
        ))
        .child(scrolled(
            uniform_list("album-grid", rows, {
                let root = cx.entity();
                move |range, _window, cx| {
                    root.update(cx, |root, cx| {
                        range
                            .map(|row| album_row(root, row, columns, height, cx))
                            .collect::<Vec<_>>()
                    })
                }
            })
            .track_scroll(&scroll)
            .flex_1()
            .size_full()
            .px(inset),
            Scrollbar::list("albums-scrollbar", &scroll),
        ))
        .into_any_element()
}

/// One row of the album grid: up to `columns` cards, always `height` tall.
fn album_row(
    root: &Root,
    row: usize,
    columns: usize,
    height: gpui::Pixels,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    // The visible slice only: this runs for the handful of rows on screen.
    let albums: Vec<library::models::Album> = root
        .library
        .read(cx)
        .albums()
        .iter()
        .skip(row * columns)
        .take(columns)
        .cloned()
        .collect();
    let covers: Vec<Option<std::path::PathBuf>> = albums
        .iter()
        .map(|album| {
            root.library
                .read(cx)
                .artwork_for(album.artwork_id, ALBUM_ART)
        })
        .collect();

    div()
        .flex()
        .items_start()
        .gap(inset)
        .h(height)
        .w_full()
        .children(
            albums
                .into_iter()
                .zip(covers)
                .map(|(album, cover)| album_card(album, cover, cx)),
        )
        .into_any_element()
}

fn album_card(
    album: library::models::Album,
    cover: Option<std::path::PathBuf>,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let (radius, hover, raised, faint_color) = (
        theme.radius,
        theme.hover,
        theme.raised,
        theme.faint_foreground,
    );
    let id = album.id;

    div()
        .id(SharedString::from(format!("album-{id}")))
        .flex()
        .flex_col()
        .flex_none()
        .w(ALBUM_TILE + TILE_PAD * 2.)
        .h(album_card_height(&theme))
        .gap(px(6.))
        .p(TILE_PAD)
        .rounded(radius)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(ALBUM_TILE)
                .rounded(radius)
                .bg(raised)
                .overflow_hidden()
                .map(|this| match cover {
                    Some(path) => this.child(img(path).size(ALBUM_TILE).rounded(radius)),
                    None => this.child(
                        svg()
                            .path(icons::path("album"))
                            .size(px(28.))
                            .text_color(faint_color),
                    ),
                }),
        )
        .child(
            div()
                .h(card_line(&theme))
                .truncate()
                .child(SharedString::from(album.name.clone())),
        )
        .child(
            div()
                .h(CARD_SMALL)
                .truncate()
                .text_size(px(11.))
                .text_color(faint_color)
                .child(SharedString::from(
                    album
                        .album_artist
                        .clone()
                        .unwrap_or_else(|| "Unknown artist".to_owned()),
                )),
        )
        .child(
            div()
                .h(CARD_SMALL)
                .text_size(px(11.))
                .text_color(faint_color)
                .child(SharedString::from(format!("{} tracks", album.track_count))),
        )
        .on_click(cx.listener(move |this, _, _window, cx| {
            this.go(Screen::Tracks(LibraryView::Album(id)), cx);
        }))
        .into_any_element()
}

/// Artists as circular portraits, the shape every music app uses for a person.
/// Virtualised the same way the album grid is, and for the same reason.
fn artists(root: &Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let count = root.library.read(cx).artists().len();
    if count == 0 {
        return Vacancy::new("artist", "No artists yet").into_any_element();
    }

    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let columns = grid_columns(
        pane_width(root, window, cx),
        ARTIST_TILE + TILE_PAD * 2.,
        inset,
    );
    let rows = count.div_ceil(columns);
    let height = artist_card_height(&theme) + inset;
    let scroll = root.list_scroll("artists");

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(header(
            "Artists".to_owned(),
            format!("{count} artists"),
            false,
            cx,
        ))
        .child(scrolled(
            uniform_list("artist-grid", rows, {
                let root = cx.entity();
                move |range, _window, cx| {
                    root.update(cx, |root, cx| {
                        range
                            .map(|row| artist_row(root, row, columns, height, cx))
                            .collect::<Vec<_>>()
                    })
                }
            })
            .track_scroll(&scroll)
            .flex_1()
            .size_full()
            .px(inset),
            Scrollbar::list("artists-scrollbar", &scroll),
        ))
        .into_any_element()
}

fn artist_row(
    root: &Root,
    row: usize,
    columns: usize,
    height: gpui::Pixels,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let artists: Vec<library::models::Artist> = root
        .library
        .read(cx)
        .artists()
        .iter()
        .skip(row * columns)
        .take(columns)
        .cloned()
        .collect();
    // Medium thumbnails even though the cells are half a tile: they are
    // downscaled, and reusing a cached size beats adding a fifth one to the
    // thumbnail cache for the sake of a quarter tile.
    let portraits: Vec<Vec<std::path::PathBuf>> = artists
        .iter()
        .map(|artist| {
            artist
                .artwork_ids
                .iter()
                .filter_map(|id| {
                    root.library
                        .read(cx)
                        .artwork_for(Some(*id), ThumbSize::Medium)
                })
                .collect()
        })
        .collect();

    div()
        .flex()
        .items_start()
        .gap(inset)
        .h(height)
        .w_full()
        .children(
            artists
                .into_iter()
                .zip(portraits)
                .map(|(artist, portrait)| artist_card(artist, portrait, cx)),
        )
        .into_any_element()
}

fn artist_card(
    artist: library::models::Artist,
    portrait: Vec<std::path::PathBuf>,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let (radius, hover, raised, muted) = (
        theme.radius,
        theme.hover,
        theme.raised,
        theme.muted_foreground,
    );
    let id = artist.id;
    let tile = mosaic(&portrait, ARTIST_TILE, "artist", cx);

    div()
        .id(SharedString::from(format!("artist-{id}")))
        .flex()
        .flex_col()
        .flex_none()
        .items_center()
        .w(ARTIST_TILE + TILE_PAD * 2.)
        .h(artist_card_height(&theme))
        .gap(px(6.))
        .p(TILE_PAD)
        .rounded(radius)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(
            // The mosaic draws a square; the round tile is this wrapper
            // clipping it, which is why `mosaic` stays shape-agnostic.
            div()
                .flex()
                .flex_none()
                .size(ARTIST_TILE)
                .rounded_full()
                .bg(raised)
                .overflow_hidden()
                .child(tile),
        )
        .child(
            div()
                .w_full()
                .h(card_line(&theme))
                .text_center()
                .truncate()
                .child(SharedString::from(artist.name.clone())),
        )
        .child(
            div()
                .h(CARD_SMALL)
                .text_size(px(11.))
                .text_color(muted)
                .child(SharedString::from(format!("{} tracks", artist.track_count))),
        )
        .on_click(cx.listener(move |this, _, _window, cx| {
            this.go(Screen::Tracks(LibraryView::Artist(id)), cx);
        }))
        .into_any_element()
}

/// Every playlist on one page: the hand-made ones on top, the smart ones
/// underneath.
///
/// The sidebar keeps its inline list — this is for when there are more of them
/// than fit down the side, and it is where the two kinds are told apart.
fn playlists(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let all = root.library.read(cx).playlists().to_vec();
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let gap = theme.metrics.gap;

    let (custom, smart) = split_playlists(all);
    let count = custom.len() + smart.len();
    let scroll = root.area_scroll("playlists");
    let actions = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            Button::new("page-new-playlist")
                .icon("plus")
                .label("New playlist")
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.show(Dialog::NewPlaylist, cx);
                })),
        )
        .child(
            Button::new("page-new-smart")
                .icon("equalizer")
                .label("New smart playlist")
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.show(Dialog::SmartPlaylist(None), cx);
                })),
        )
        .into_any_element();

    let body = div()
        .id("playlists")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap(gap)
        .p(inset)
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .child(eyebrow("Custom", cx))
        .child(match custom.is_empty() {
            true => faint("No playlists yet".to_owned(), cx).into_any_element(),
            false => playlist_cards(root, custom, cx),
        })
        .child(eyebrow("Smart", cx))
        .child(match smart.is_empty() {
            true => faint(
                "No smart playlists yet. A smart playlist is a set of rules \
                 rather than a list of songs, so it keeps itself up to date."
                    .to_owned(),
                cx,
            )
            .into_any_element(),
            false => playlist_cards(root, smart, cx),
        });

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(header_with(
            "Playlists".to_owned(),
            format!("{count} playlists"),
            false,
            Some(actions),
            cx,
        ))
        .child(scrolled(
            body,
            Scrollbar::area("playlists-scrollbar", &scroll),
        ))
        .into_any_element()
}

/// The two regions of the Playlists page: the ones the user made, and the ones
/// the app made.
///
/// Favorites is in neither, as it is nowhere else: it is the heart on a track
/// rather than a list you put things in, and the playlist row behind it holds
/// no `playlist_tracks` at all. Listing it gave it a card reading "0 tracks"
/// that opened an empty list. The sidebar, the add-to-playlist menus and
/// search each already refuse it for the same reason, and it has its own entry
/// down the side.
///
/// Pure, and separate from the rendering, because this is the third place that
/// has had to learn Favorites is not a playlist.
fn split_playlists(
    all: Vec<library::models::PlaylistRow>,
) -> (
    Vec<library::models::PlaylistRow>,
    Vec<library::models::PlaylistRow>,
) {
    use library::models::PlaylistKind;
    let mut custom = Vec::new();
    let mut smart = Vec::new();
    for list in all {
        match list.kind {
            PlaylistKind::Custom => custom.push(list),
            PlaylistKind::Smart => smart.push(list),
            PlaylistKind::Favorites => {}
        }
    }
    (custom, smart)
}

/// One region's worth of playlist cards.
fn playlist_cards(
    root: &Root,
    lists: Vec<library::models::PlaylistRow>,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let (radius, raised, hover, muted) = (
        theme.radius,
        theme.raised,
        theme.hover,
        theme.muted_foreground,
    );
    let tile = px(166.);
    let covers: Vec<Vec<std::path::PathBuf>> = lists
        .iter()
        .map(|list| {
            list.artwork_ids
                .iter()
                .filter_map(|id| {
                    root.library
                        .read(cx)
                        .artwork_for(Some(*id), ThumbSize::Medium)
                })
                .collect()
        })
        .collect();

    div()
        .flex()
        .flex_wrap()
        .gap(theme.metrics.gap)
        .children(lists.into_iter().zip(covers).map(|(list, cover)| {
            let id = list.id;
            let smart = list.kind == library::models::PlaylistKind::Smart;
            let icon = match smart {
                true => "equalizer",
                false => "playlist",
            };
            // A smart playlist's count is the number of rules it stores, which
            // is zero — what it holds is whatever matches right now, and that
            // is a question for the page it opens.
            let detail = match smart {
                true => "Smart playlist".to_owned(),
                false => format!("{} tracks", list.track_count),
            };

            div()
                .id(SharedString::from(format!("playlist-card-{id}")))
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(8.))
                .w(px(190.))
                .p(px(12.))
                .rounded(radius)
                .bg(raised)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .size(tile)
                        .rounded(radius)
                        .overflow_hidden()
                        .child(mosaic(&cover, tile, icon, cx)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .child(SharedString::from(list.name.clone())),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(muted)
                                .child(SharedString::from(detail)),
                        ),
                )
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.go(Screen::Tracks(LibraryView::Playlist(id)), cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                        this.open_menu(event.position, MenuTarget::Playlist(id), cx);
                    }),
                )
        }))
        .into_any_element()
}

/// Genres as colour-blocked cards. A genre has no artwork of its own, so it gets
/// a colour of its own instead — derived from the name, so the same genre is the
/// same colour every time the app starts. The cover most of its tracks carry
/// sits on the right, fading into that colour.
fn genres(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let genres = root.library.read(cx).genres().to_vec();
    let covers: Vec<Vec<std::path::PathBuf>> = genres
        .iter()
        .map(|(_, _, art)| {
            art.iter()
                .filter_map(|id| {
                    root.library
                        .read(cx)
                        .artwork_for(Some(*id), ThumbSize::covering(GENRE_ART))
                })
                .collect()
        })
        .collect();
    if genres.is_empty() {
        return Vacancy::new("genre", "No genres yet").into_any_element();
    }
    let count = genres.len();

    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let radius = theme.radius;
    let dark = theme.dark;
    let scroll = root.area_scroll("genres");

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(header(
            "Genres".to_owned(),
            format!("{count} genres"),
            false,
            cx,
        ))
        .child(scrolled(
            div()
                .id("genre-grid")
                .flex()
                .flex_wrap()
                .gap(theme.metrics.gap)
                .p(inset)
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .children(
                    genres
                        .into_iter()
                        .zip(covers)
                        .map(move |((name, tracks, _), cover)| {
                            let card = genre_color(&name, dark);
                            let label = name.clone();
                            div()
                                .id(SharedString::from(format!("genre-{name}")))
                                .relative()
                                .flex()
                                .flex_col()
                                .flex_none()
                                .justify_end()
                                .w(px(190.))
                                .h(px(96.))
                                .p(px(12.))
                                .rounded(radius)
                                // Left to right: the genre's colour, deepening as it
                                // goes, so the art on the right has something to sit on.
                                .bg(gpui::linear_gradient(
                                    90.,
                                    gpui::linear_color_stop(card, 0.),
                                    gpui::linear_color_stop(deepen(card), 1.),
                                ))
                                .overflow_hidden()
                                .cursor_pointer()
                                .hover(|style| style.opacity(0.88))
                                .children((!cover.is_empty()).then(|| {
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .right_0()
                                        .w(px(GENRE_ART as f32))
                                        // Four covers from four different
                                        // artists, so a genre one artist
                                        // dominates does not show them four
                                        // times.
                                        .child(mosaic(&cover, px(GENRE_ART as f32), "genre", cx))
                                        // The art is only ever a backdrop: the colour
                                        // washes over its left edge so the name stays
                                        // readable and the card keeps one silhouette.
                                        .child(div().absolute().inset_0().bg(
                                            gpui::linear_gradient(
                                                90.,
                                                gpui::linear_color_stop(deepen(card), 0.),
                                                gpui::linear_color_stop(clear(card), 0.85),
                                            ),
                                        ))
                                }))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(theme.text(ui::Text::Large))
                                        .text_color(gpui::white())
                                        .child(SharedString::from(label)),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(gpui::hsla(0., 0., 1., 0.75))
                                        .child(SharedString::from(format!("{tracks} tracks"))),
                                )
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.go(Screen::Tracks(LibraryView::Genre(name.clone())), cx);
                                }))
                        }),
                ),
            Scrollbar::area("genres-scrollbar", &scroll),
        ))
        .into_any_element()
}

/// How wide the cover strip on a genre card is, and which cached thumbnail
/// size that asks for.
const GENRE_ART: u32 = 96;

/// Up to four covers packed into a square.
///
/// Clipping to a circle is the caller's job — the artist tile is round and the
/// genre strip is not. Nothing here is absolutely positioned: the layouts are
/// rows of `flex_1` children inside a column, which is why three covers can be
/// two quarters over a band without any arithmetic.
///
/// Cells are `ObjectFit::Cover` so a cover that is not square is cropped rather
/// than squashed; a half-tile of stretched artwork looks broken in a way a crop
/// never does.
fn mosaic(
    covers: &[std::path::PathBuf],
    size: Pixels,
    fallback_icon: &'static str,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let cell = |path: &std::path::PathBuf| {
        div().flex_1().min_w_0().overflow_hidden().child(
            img(path.clone())
                .size_full()
                .object_fit(gpui::ObjectFit::Cover),
        )
    };
    let row = || div().flex().flex_1().min_h_0().w_full();

    let tile = div()
        .flex()
        .flex_col()
        .size(size)
        .overflow_hidden()
        .bg(theme.raised);

    match covers {
        // Nothing to draw: the placeholder icon, centred, as before.
        [] => tile.items_center().justify_center().child(
            svg()
                .path(icons::path(fallback_icon))
                .size(size * 0.36)
                .text_color(theme.faint_foreground),
        ),
        [one] => tile.child(row().child(cell(one))),
        // Two half-width columns, left and right.
        [one, two] => tile.child(row().child(cell(one)).child(cell(two))),
        // Two quarters on top, one full-width band underneath.
        [one, two, three] => tile
            .child(row().child(cell(one)).child(cell(two)))
            .child(row().child(cell(three))),
        // 2x2. Anything past the fourth is ignored rather than crammed in.
        [one, two, three, four, ..] => tile
            .child(row().child(cell(one)).child(cell(two)))
            .child(row().child(cell(three)).child(cell(four))),
    }
    .into_any_element()
}

/// The far end of a genre card's gradient: the same hue, darker.
fn deepen(color: gpui::Hsla) -> gpui::Hsla {
    gpui::Hsla {
        l: (color.l * 0.6).clamp(0., 1.),
        ..color
    }
}

/// The same colour with nothing left of it — the transparent end of a fade.
/// Fading to a transparent *black* would grey the art out on the way.
fn clear(color: gpui::Hsla) -> gpui::Hsla {
    gpui::Hsla { a: 0., ..color }
}

/// A stable colour for a genre name. The hash is ours rather than
/// `DefaultHasher`'s, whose output is explicitly not stable between releases —
/// and a genre that changes colour when the toolchain moves would be a bug
/// nobody could reproduce.
fn genre_color(name: &str, dark: bool) -> gpui::Hsla {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let hue = (hash % 360) as f32 / 360.0;
    match dark {
        true => gpui::hsla(hue, 0.44, 0.42, 1.0),
        false => gpui::hsla(hue, 0.52, 0.48, 1.0),
    }
}

fn years(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let years = root.library.read(cx).years().to_vec();
    if years.is_empty() {
        return Vacancy::new("calendar", "No years yet").into_any_element();
    }
    let count = years.len();

    list_screen(
        root,
        "years",
        "Years".to_owned(),
        format!("{count} years"),
        years
            .into_iter()
            .map(|(year, tracks)| ListRow {
                icon: "calendar",
                label: year.to_string(),
                detail: format!("{tracks} tracks"),
                view: LibraryView::Year(year),
                folder: None,
            })
            .collect(),
        None,
        cx,
    )
}

fn folders(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let folders = root.library.read(cx).folders().to_vec();
    if folders.is_empty() {
        return empty_library(root, false, cx).into_any_element();
    }
    let count = folders.len();

    // The one thing to do on this screen, where the eye looks for it.
    let add = Button::new("add-folder-header")
        .primary()
        .icon("plus")
        .label("Add Folder")
        .on_click(cx.listener(|this, _, window, cx| this.dispatch_open_folder(window, cx)))
        .into_any_element();

    list_screen(
        root,
        "folders",
        "Folders".to_owned(),
        format!("{count} folders"),
        folders
            .into_iter()
            .map(|folder| ListRow {
                icon: "folder",
                label: folder.path.display().to_string(),
                detail: String::new(),
                view: LibraryView::Folder(folder.id),
                folder: Some(folder.id),
            })
            .collect(),
        Some(add),
        cx,
    )
}

/// One row of a `list_screen`.
#[derive(Clone)]
struct ListRow {
    icon: &'static str,
    label: String,
    detail: String,
    view: LibraryView,
    /// The folder this row stands for, when it stands for one. That is what
    /// gives the row its remove button and its context menu; `None` for years,
    /// which are a fact about the tracks rather than something the library
    /// holds.
    folder: Option<i64>,
}

/// A simple one-line-per-entry screen, shared by years and folders. They differ
/// only in their icon, their destination, and the button in the corner.
fn list_screen(
    root: &Root,
    id: &'static str,
    title: String,
    subtitle: String,
    entries: Vec<ListRow>,
    action: Option<AnyElement>,
    cx: &mut Context<Root>,
) -> AnyElement {
    let scroll = root.list_scroll(id);
    let theme = cx.theme().clone();
    let row = theme.metrics.row;
    let inset = theme.metrics.inset;
    let gap = theme.metrics.gap;
    let hover = theme.hover;
    let muted = theme.muted_foreground;
    let total = entries.len();

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(header_with(title, subtitle, false, action, cx))
        .child(scrolled(
            uniform_list(id, total, {
                let root = cx.entity();
                move |range, _window, _cx: &mut App| {
                    range
                        .map(|index| {
                            let ListRow {
                                icon,
                                label,
                                detail,
                                view,
                                folder,
                            } = entries[index].clone();
                            let root = root.clone();
                            let root_for_remove = root.clone();
                            let root_for_menu = root.clone();
                            div()
                                .id(index)
                                // The same menu the sidebar's folders get, so a
                                // folder behaves the same wherever it is listed.
                                .when_some(folder, |this, id| {
                                    this.on_mouse_down(
                                        MouseButton::Right,
                                        move |event: &gpui::MouseDownEvent, _window, cx| {
                                            root_for_menu.update(cx, |root, cx| {
                                                root.open_menu(
                                                    event.position,
                                                    MenuTarget::Folder(id),
                                                    cx,
                                                );
                                            });
                                        },
                                    )
                                })
                                .flex()
                                .items_center()
                                .gap(gap)
                                .h(row)
                                .w_full()
                                .overflow_hidden()
                                .px(inset)
                                .cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .child(
                                    svg()
                                        .path(icons::path(icon))
                                        .size(px(15.))
                                        .flex_none()
                                        .text_color(muted),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(label))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(muted)
                                        .child(SharedString::from(detail)),
                                )
                                // Removing a folder never happens on the click
                                // itself: the button asks first, because what
                                // goes with the folder is every play count and
                                // favourite scanned from it.
                                .when_some(folder, |this, folder| {
                                    this.child(
                                        Button::new(("remove-folder-row", index as u64))
                                            .ghost()
                                            .small()
                                            .icon("trash")
                                            .on_click(move |_, _window, cx| {
                                                root_for_remove.update(cx, |root, cx| {
                                                    root.show(
                                                        Dialog::ConfirmRemoveFolder(folder),
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                })
                                .on_click(move |_, _window, cx| {
                                    root.update(cx, |root, cx| {
                                        root.go(Screen::Tracks(view.clone()), cx)
                                    });
                                })
                        })
                        .collect::<Vec<_>>()
                }
            })
            .track_scroll(&scroll)
            .flex_1()
            .size_full(),
            Scrollbar::list(format!("{id}-scrollbar"), &scroll),
        ))
        .into_any_element()
}

// -- queue ----------------------------------------------------------------

fn queue(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let total = root.player.read(cx).queue().len();

    if total == 0 {
        return Vacancy::new("queue", "The queue is empty")
            .detail("Play something, or send tracks here with Add to Queue.")
            .into_any_element();
    }

    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .px(inset)
                .py(theme.metrics.pad)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(heading("Queue".to_owned(), cx))
                        .child(faint(format!("{total} tracks"), cx)),
                )
                .child(
                    Button::new("clear-queue")
                        .icon("trash")
                        .label("Clear")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.player.update(cx, |player, cx| player.clear_queue(cx));
                        })),
                ),
        )
        .child(queue_list(root, "queue", false, cx))
        .into_any_element()
}

/// The height of one row of the full queue panel.
///
/// This and `strip_row_height` are the only place these are decided. Collapsing
/// the queue converts a scroll position from one to the other, and a height it
/// guessed wrong would make the list jump — so the lists and the conversion read
/// the same number.
pub fn queue_row_height(theme: &ui::Theme) -> gpui::Pixels {
    theme.metrics.list_row
}

/// The artwork edge in the collapsed strip.
pub fn strip_art_size(theme: &ui::Theme) -> gpui::Pixels {
    theme.metrics.thumb * 1.4
}

/// The height of one row of the collapsed strip, which is taller than a panel
/// row: it is all artwork.
pub fn strip_row_height(theme: &ui::Theme) -> gpui::Pixels {
    strip_art_size(theme) + theme.metrics.pad
}

/// The handful of theme colours a piece of queue artwork needs. Passed as one
/// value because the art is drawn from inside a list closure, which cannot
/// borrow the theme for the lifetime of the rows it builds.
#[derive(Clone, Copy)]
struct ArtStyle {
    radius: gpui::Pixels,
    background: gpui::Hsla,
    border: gpui::Hsla,
    playing: gpui::Hsla,
    icon: gpui::Hsla,
}

impl ArtStyle {
    fn of(theme: &ui::Theme) -> Self {
        Self {
            radius: theme.radius,
            background: theme.raised,
            border: theme.border,
            playing: theme.accent,
            icon: theme.faint_foreground,
        }
    }
}

/// One queue thumbnail: the album art, a music glyph when there is none, and a
/// border that turns to the accent colour on the track that is playing — which
/// is the only mark the mini strip has to go on.
fn queue_art(
    artwork: Option<std::path::PathBuf>,
    playing: bool,
    size: gpui::Pixels,
    style: &ArtStyle,
) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(style.radius)
        .bg(style.background)
        .border_1()
        .border_color(match playing {
            true => style.playing,
            false => style.border,
        })
        .overflow_hidden()
        .map(|this| match artwork {
            Some(path) => this.child(img(path).size(size).rounded(style.radius)),
            None => this.child(
                svg()
                    .path(icons::path("music"))
                    .size(px(14.))
                    .text_color(style.icon),
            ),
        })
}

/// The queue itself, without a header. Shared by the Queue screen and the panel
/// down the right-hand side, so the two can never show different things.
///
/// `id` names both the list and its scroll position: two of these can be on
/// screen at once, and they must not share either. `compact` drops the artist,
/// which does not fit beside a title in a side panel — and a row that does not
/// fit pushes its own remove button off the edge.
pub fn queue_list(
    root: &Root,
    id: &'static str,
    compact: bool,
    cx: &mut Context<Root>,
) -> AnyElement {
    let player = root.player.read(cx);
    let total = player.queue().len();
    let playing = player.queue().current_index();

    if total == 0 {
        // The panel stays on screen with an empty queue, so it says why it is
        // empty rather than showing a blank column that reads as a fault.
        let theme = cx.theme().clone();
        return div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .p(theme.metrics.inset)
            .child(
                svg()
                    .path(icons::path("queue"))
                    .size(px(24.))
                    .flex_none()
                    .text_color(theme.faint_foreground),
            )
            .child(div().text_center().child(faint("Nothing queued", cx)))
            .into_any_element();
    }

    let theme = cx.theme().clone();
    // The taller row: these carry a thumbnail now, and `row` is not tall enough
    // to hold one.
    let row = queue_row_height(&theme);
    let inset = theme.metrics.pad;
    let gap = theme.metrics.gap;
    let hover = theme.hover;
    let accent = theme.accent;
    let playing_bg = theme.accent.opacity(0.18);
    let thumb = theme.metrics.thumb;
    let faint_color = theme.faint_foreground;
    let art_style = ArtStyle::of(&theme);
    let scroll = root.list_scroll(id);

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(scrolled(
            uniform_list(id, total, {
                let root = cx.entity();
                move |range, _window, cx| {
                    // Tell the player what is on screen so it can read those
                    // rows, a page ahead in both directions.
                    root.update(cx, |root, cx| {
                        let window = range.start.saturating_sub(20)..range.end + 20;
                        root.player
                            .update(cx, |player, cx| player.ensure_queue_rows(window, cx));
                    });

                    range
                        .map(|index| {
                            let is_current = playing == Some(index);
                            let root_for_click = root.clone();
                            let root_for_remove = root.clone();
                            let root_for_favorite = root.clone();
                            let track = root.read(cx).player.read(cx).queue_row(index).cloned();
                            let artwork = track.as_ref().and_then(|track| {
                                root.read(cx)
                                    .library
                                    .read(cx)
                                    .artwork_path(track, ThumbSize::Small)
                            });
                            // An ellipsis until the row arrives, rather than a
                            // blank line that reads as a missing track.
                            let favorite = track.as_ref().map(|track| (track.id, track.favorite));
                            let id = track.as_ref().map(|track| track.id);
                            let root_for_menu = root.clone();
                            let (title, artist) = track
                                .map(|track| (track.title, track.artist))
                                .unwrap_or_else(|| ("…".to_owned(), String::new()));

                            div()
                                .id(index)
                                .flex()
                                .items_center()
                                .gap(gap)
                                .h(row)
                                // The same menu the track lists give a song. A
                                // row still loading has no track to act on, so
                                // it gets no menu rather than an empty one.
                                .when_some(id, |this, id| {
                                    this.on_mouse_down(
                                        MouseButton::Right,
                                        move |event: &gpui::MouseDownEvent, _window, cx| {
                                            root_for_menu.update(cx, |root, cx| {
                                                root.open_menu(
                                                    event.position,
                                                    MenuTarget::Track(id),
                                                    cx,
                                                );
                                            });
                                        },
                                    )
                                })
                                // A list item sizes to its content unless told
                                // otherwise: without this a long title runs off
                                // the end of a narrow panel instead of being
                                // truncated, taking the remove button with it.
                                .w_full()
                                .overflow_hidden()
                                .px(inset)
                                .cursor_pointer()
                                .when(is_current, |this| this.bg(playing_bg).text_color(accent))
                                .hover(move |style| style.bg(hover))
                                .child(queue_art(artwork, is_current, thumb, &art_style))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::from(title)),
                                )
                                .when(!compact && !artist.trim().is_empty(), |this| {
                                    this.child(
                                        div()
                                            .flex_none()
                                            .max_w(relative(0.35))
                                            .truncate()
                                            .text_color(faint_color)
                                            .child(SharedString::from(artist)),
                                    )
                                })
                                .when_some(favorite, |this, (track_id, is_favorite)| {
                                    // Built inline rather than through
                                    // `favorite_button`: the list closure is
                                    // handed an `App`, not a `Context<Root>`.
                                    let root = root_for_favorite.clone();
                                    this.child(
                                        Button::new(("queue-favorite", index as u64))
                                            .ghost()
                                            .small()
                                            .icon(match is_favorite {
                                                true => "heart-filled",
                                                false => "heart",
                                            })
                                            .tint(match is_favorite {
                                                true => accent,
                                                false => faint_color,
                                            })
                                            .on_click(move |_, _window, cx| {
                                                root.update(cx, |root, cx| {
                                                    root.toggle_favorite(track_id, cx)
                                                });
                                            }),
                                    )
                                })
                                .child(
                                    Button::new(("remove-from-queue", index as u64))
                                        .ghost()
                                        .small()
                                        .icon("close")
                                        .on_click(move |_, _window, cx| {
                                            root_for_remove.update(cx, |root, cx| {
                                                root.player.update(cx, |player, cx| {
                                                    player.remove_from_queue(index, cx)
                                                });
                                            });
                                        }),
                                )
                                .on_click(move |event: &gpui::ClickEvent, _window, cx| {
                                    if event.click_count() >= 2 {
                                        root_for_click.update(cx, |root, cx| {
                                            root.player
                                                .update(cx, |player, cx| player.jump_to(index, cx));
                                        });
                                    }
                                })
                        })
                        .collect::<Vec<_>>()
                }
            })
            .track_scroll(&scroll)
            .flex_1()
            .size_full(),
            Scrollbar::list(format!("{id}-scrollbar"), &scroll),
        ))
        .into_any_element()
}

/// The queue with the words taken away: a column of album art down the edge of
/// the window, shown in place of the full panel rather than hiding the queue
/// altogether. The playing track wears the accent border.
///
/// Double-click jumps, the same as the full list — a strip you cannot play from
/// is decoration.
pub fn queue_strip(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let player = root.player.read(cx);
    let total = player.queue().len();
    let playing = player.queue().current_index();

    let theme = cx.theme().clone();
    let art = strip_art_size(&theme);
    let row = strip_row_height(&theme);
    let inset = theme.metrics.pad;
    let hover = theme.hover;
    let playing_bg = theme.accent.opacity(0.18);
    let art_style = ArtStyle::of(&theme);
    let scroll = root.list_scroll("queue-strip");

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(art + inset * 2.)
        .h_full()
        .bg(theme.surface)
        // The strip is the only place the collapsed queue is visible, so it
        // carries its own way back out; the title bar toggle is a long way from
        // where the user is looking.
        .child(
            div().flex().flex_none().justify_center().py(px(4.)).child(
                Button::new("queue-expand")
                    .ghost()
                    .icon("chevron-left")
                    .icon_size(px(22.))
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_queue_panel(cx))),
            ),
        )
        .child(scrolled(
            uniform_list("queue-strip", total, {
                let root = cx.entity();
                move |range, _window, cx| {
                    root.update(cx, |root, cx| {
                        let window = range.start.saturating_sub(20)..range.end + 20;
                        root.player
                            .update(cx, |player, cx| player.ensure_queue_rows(window, cx));
                    });

                    range
                        .map(|index| {
                            let root_for_click = root.clone();
                            let root_for_menu = root.clone();
                            let track = root.read(cx).player.read(cx).queue_row(index).cloned();
                            let id = track.as_ref().map(|track| track.id);
                            let artwork = track.and_then(|track| {
                                root.read(cx)
                                    .library
                                    .read(cx)
                                    .artwork_path(&track, ThumbSize::Small)
                            });

                            div()
                                .id(index)
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .w_full()
                                .h(row)
                                .cursor_pointer()
                                // The same band the full rows wear, so the
                                // strip marks the playing track the same way
                                // the panel beside it does.
                                .when(playing == Some(index), |this| this.bg(playing_bg))
                                .hover(move |style| style.bg(hover))
                                // The strip is the queue with the words taken
                                // away, so it answers the right button the same
                                // way the panel does.
                                .when_some(id, |this, id| {
                                    this.on_mouse_down(
                                        MouseButton::Right,
                                        move |event: &gpui::MouseDownEvent, _window, cx| {
                                            root_for_menu.update(cx, |root, cx| {
                                                root.open_menu(
                                                    event.position,
                                                    MenuTarget::Track(id),
                                                    cx,
                                                );
                                            });
                                        },
                                    )
                                })
                                .child(queue_art(artwork, playing == Some(index), art, &art_style))
                                .on_click(move |event: &gpui::ClickEvent, _window, cx| {
                                    if event.click_count() >= 2 {
                                        root_for_click.update(cx, |root, cx| {
                                            root.player
                                                .update(cx, |player, cx| player.jump_to(index, cx));
                                        });
                                    }
                                })
                        })
                        .collect::<Vec<_>>()
                }
            })
            .track_scroll(&scroll)
            .flex_1()
            .size_full(),
            Scrollbar::list("queue-strip-scrollbar", &scroll),
        ))
        .into_any_element()
}

// -- search ---------------------------------------------------------------

/// The Search page: the field, the chips that narrow it, and either the results
/// or the searches the user ran before.
fn search(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let query = root.library.read(cx).query().to_owned();
    let searching = !query.trim().is_empty();

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(theme.metrics.gap)
                .px(inset)
                .pt(inset)
                .pb(theme.metrics.pad)
                .child(root.search.clone())
                .child(search_chips(root, cx)),
        )
        .child(match searching {
            true => search_results(root, &query, cx),
            false => search_history(root, cx),
        })
        .into_any_element()
}

/// The row of chips. Which one is lit decides which sections are drawn, so the
/// results below never disagree with the chip above them.
fn search_chips(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let current = root.search_filter;

    div()
        .flex()
        .flex_wrap()
        .gap(px(6.))
        .children(SearchFilter::ALL.into_iter().map(|filter| {
            Button::new(SharedString::from(format!("chip-{}", filter.label())))
                .small()
                .icon(filter.icon())
                .label(filter.label())
                .selected(filter == current)
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.search_filter = filter;
                    cx.notify();
                }))
        }))
}

/// What the page shows before anything is typed: the searches run before, and a
/// way to forget them.
fn search_history(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let history = root.settings.read(cx).get().search_history.clone();
    if history.is_empty() {
        return Vacancy::new("search", "Search your library")
            .detail("Songs, artists, albums, genres and playlists.")
            .into_any_element();
    }

    let theme = cx.theme().clone();
    let (raised, hover, muted) = (theme.raised, theme.hover, theme.muted_foreground);

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .items_center()
        .gap(theme.metrics.gap)
        .p(theme.metrics.inset)
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap(px(8.))
                .children(history.into_iter().enumerate().map(|(index, query)| {
                    let text = query.clone();
                    div()
                        .id(SharedString::from(format!("recent-search-{index}")))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .h(theme.metrics.field)
                        .px(theme.metrics.inset)
                        // Fully round: these read as pills, not buttons.
                        .rounded_full()
                        .bg(raised)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(
                            svg()
                                .path(icons::path("search"))
                                .size(px(13.))
                                .flex_none()
                                .text_color(muted),
                        )
                        .child(SharedString::from(query))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.run_search(text.clone(), window, cx);
                        }))
                })),
        )
        .child(
            Button::new("clear-search-history")
                .ghost()
                .small()
                .label("Clear search history")
                .on_click(cx.listener(|this, _, _window, cx| this.forget_searches(cx))),
        )
        // Takes the leftover height so the pills sit near the top rather than
        // being centred down the whole pane.
        .child(div().flex_1().min_h_0())
        .into_any_element()
}

/// The results themselves, under whichever chip is lit.
fn search_results(root: &Root, query: &str, cx: &mut Context<Root>) -> AnyElement {
    let results = root.library.read(cx).search_results().clone();
    let filter = root.search_filter;
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;

    // Every section is built up front rather than inside a `.when(..)` closure:
    // the rows need `&mut cx`, and a closure would have to carry that borrow out
    // with it.
    let mut sections: Vec<AnyElement> = Vec::new();

    if filter.shows(SearchFilter::Songs) && !results.tracks.is_empty() {
        sections.push(search_heading(results.tracks.len(), "Song", cx));
        // Playing a hit queues every hit, so the rest of the results follow
        // instead of playback stopping after the one that was clicked.
        let ids: Vec<library::models::TrackId> =
            results.tracks.iter().map(|track| track.id).collect();
        for (index, track) in results.tracks.iter().enumerate() {
            let artwork = root.library.read(cx).artwork_path(track, ThumbSize::Small);
            sections.push(search_song(index, track, artwork, &ids, cx));
        }
    }
    if filter.shows(SearchFilter::Playlists) && !results.playlists.is_empty() {
        sections.push(search_heading(results.playlists.len(), "Playlist", cx));
        for (index, list) in results.playlists.iter().enumerate() {
            let id = list.id;
            sections.push(
                search_row("playlist", index, list.name.clone(), cx)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.go(Screen::Tracks(LibraryView::Playlist(id)), cx);
                    }))
                    .into_any_element(),
            );
        }
    }
    if filter.shows(SearchFilter::Albums) && !results.albums.is_empty() {
        sections.push(search_heading(results.albums.len(), "Album", cx));
        for (index, album) in results.albums.iter().enumerate() {
            let id = album.id;
            sections.push(
                search_row("album", index, album.name.clone(), cx)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.go(Screen::Tracks(LibraryView::Album(id)), cx);
                    }))
                    .into_any_element(),
            );
        }
    }
    if filter.shows(SearchFilter::Artists) && !results.artists.is_empty() {
        sections.push(search_heading(results.artists.len(), "Artist", cx));
        for (index, artist) in results.artists.iter().enumerate() {
            let id = artist.id;
            sections.push(
                search_row("artist", index, artist.name.clone(), cx)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.go(Screen::Tracks(LibraryView::Artist(id)), cx);
                    }))
                    .into_any_element(),
            );
        }
    }
    if filter.shows(SearchFilter::Genres) && !results.genres.is_empty() {
        sections.push(search_heading(results.genres.len(), "Genre", cx));
        for (index, name) in results.genres.iter().enumerate() {
            let name = name.clone();
            sections.push(
                search_row("genre", index, name.clone(), cx)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.go(Screen::Tracks(LibraryView::Genre(name.clone())), cx);
                    }))
                    .into_any_element(),
            );
        }
    }

    if sections.is_empty() {
        // Names the chip when one is narrowing it: "nothing matches" over a
        // library that does have matching songs, because Albums is lit, reads
        // as a fault rather than a filter.
        let detail = match filter {
            SearchFilter::All => "Search covers songs, artists, albums, genres and playlists.",
            _ => "Nothing under this filter. Try All.",
        };
        return Vacancy::new("search", format!("Nothing matches “{query}”"))
            .detail(detail)
            .into_any_element();
    }

    let scroll = root.area_scroll("search");
    scrolled(
        div()
            .id("search-results")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            // Result rows carry artwork, which butts straight up against the
            // next row's without this.
            .gap(px(4.))
            .px(inset)
            .pb(inset)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .children(sections),
        Scrollbar::area("search-scrollbar", &scroll),
    )
    .into_any_element()
}

/// "154 Songs" over a section, the way the mock counts them.
fn search_heading(count: usize, noun: &str, cx: &mut Context<Root>) -> AnyElement {
    let label = match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    };
    let pad = cx.theme().metrics.inset;
    div()
        .flex()
        .flex_none()
        .items_center()
        .pt(pad)
        .pb(px(4.))
        .child(heading(label, cx))
        .into_any_element()
}

/// One song in the results, laid out like a row of the track lists.
fn search_song(
    index: usize,
    track: &Track,
    artwork: Option<std::path::PathBuf>,
    ids: &[library::models::TrackId],
    cx: &mut Context<Root>,
) -> AnyElement {
    let ids = ids.to_vec();
    let theme = cx.theme().clone();
    let id = track.id;
    let (hover, faint_color, radius, thumb) = (
        theme.hover,
        theme.faint_foreground,
        theme.radius,
        theme.metrics.thumb,
    );

    div()
        .id(SharedString::from(format!("search-song-{index}")))
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .h(theme.metrics.list_row)
        .w_full()
        .px(theme.metrics.pad)
        .rounded(radius)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                this.open_menu(event.position, MenuTarget::Track(id), cx);
            }),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(thumb)
                .rounded(radius)
                .bg(theme.raised)
                .overflow_hidden()
                .map(|this| match artwork {
                    Some(path) => this.child(img(path).size(thumb).rounded(radius)),
                    None => this.child(
                        svg()
                            .path(icons::path("music"))
                            .size(px(14.))
                            .text_color(faint_color),
                    ),
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(track.title.clone())),
        )
        .child(
            div()
                .flex_none()
                .w(relative(0.2))
                .truncate()
                .text_color(faint_color)
                .child(SharedString::from(dash_if_empty(&track.artist))),
        )
        .child(
            div()
                .flex_none()
                .w(relative(0.2))
                .truncate()
                .text_color(faint_color)
                .child(SharedString::from(
                    track.album.clone().unwrap_or_else(|| "—".to_owned()),
                )),
        )
        .child(
            div()
                .flex_none()
                .w(px(48.))
                .text_color(faint_color)
                .child(SharedString::from(
                    track
                        .year
                        .map(|year| year.to_string())
                        .unwrap_or_else(|| "—".to_owned()),
                )),
        )
        .child(favorite_button(
            ("search-favorite", index as u64),
            id,
            track.favorite,
            cx,
        ))
        .child(
            div()
                .flex_none()
                .w(px(44.))
                .text_color(faint_color)
                .child(SharedString::from(clock(track.duration))),
        )
        .on_click(cx.listener(move |this, _, _window, cx| {
            let ids = ids.clone();
            this.player
                .update(cx, |player, cx| player.play_all(ids, index, cx));
        }))
        .into_any_element()
}

/// One row of the search results.
///
/// The id is the section plus the position, never the label: two albums can
/// share a name, and two elements sharing an id is one element as far as GPUI is
/// concerned — the duplicate stops taking clicks.
fn search_row(
    icon: &'static str,
    index: usize,
    label: String,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme().clone();
    let hover = theme.hover;
    let muted = theme.muted_foreground;

    div()
        .id(SharedString::from(format!("result-{icon}-{index}")))
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .h(theme.metrics.row)
        .px(theme.metrics.pad)
        .rounded(theme.radius)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(15.))
                .flex_none()
                .text_color(muted),
        )
        .child(div().flex_1().min_w_0().truncate().child(label))
}

/// The table's column key for a sort key, so the header arrow lands on the right
/// column.
fn sort_column_key(key: library::models::SortKey) -> &'static str {
    use library::models::SortKey;
    match key {
        SortKey::Title => "title",
        SortKey::Artist => "artist",
        SortKey::Album => "album",
        SortKey::Duration => "duration",
        SortKey::TrackNumber => "track",
        SortKey::Year => "year",
        SortKey::DateAdded => "added",
        SortKey::LastPlayed => "played",
        SortKey::PlayCount => "plays",
        SortKey::Rating => "rating",
    }
}

/// A horizontal meter, used by the visualizer and the scan bar.
pub fn meter(fraction: f32, cx: &App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .h(px(4.))
        .w_full()
        .rounded_full()
        .bg(theme.border)
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0.0, 1.0)))
                .rounded_full()
                .bg(theme.accent),
        )
}

/// Kept public so the settings screen can reuse the same slider treatment.
pub fn labelled_slider(
    id: &'static str,
    label: String,
    value: f32,
    cx: &mut Context<Root>,
    on_change: impl Fn(&mut Root, f32, &mut Context<Root>) + 'static,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .child(div().w(px(150.)).flex_none().child(label))
        .child(Slider::new(id, value).on_change(
            cx.listener(move |this, value: &f32, _window, cx| on_change(this, *value, cx)),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use library::models::SortKey;

    #[test]
    fn the_playlists_page_never_offers_favorites_as_a_playlist() {
        use library::models::{PlaylistKind, PlaylistRow};

        let row = |id: i64, name: &str, kind: PlaylistKind| PlaylistRow {
            id,
            name: name.to_owned(),
            kind,
            rules: None,
            position: id,
            track_count: 0,
            artwork_ids: vec![],
            created_at: 0,
            modified_at: 0,
        };
        let (custom, smart) = split_playlists(vec![
            row(1, "Favorites", PlaylistKind::Favorites),
            row(2, "Rock", PlaylistKind::Custom),
            row(3, "Never Played", PlaylistKind::Smart),
        ]);

        // Favorites is backed by the flag on each track, so a card for it shows
        // no tracks and opens an empty list. It belongs to neither region.
        assert_eq!(
            custom.iter().map(|list| &list.name).collect::<Vec<_>>(),
            vec!["Rock"]
        );
        assert_eq!(
            smart.iter().map(|list| &list.name).collect::<Vec<_>>(),
            vec!["Never Played"]
        );

        // A library with nothing but Favorites in it has an empty page rather
        // than one card that does nothing.
        let (custom, smart) = split_playlists(vec![row(1, "Favorites", PlaylistKind::Favorites)]);
        assert!(custom.is_empty() && smart.is_empty());
    }

    #[test]
    fn a_grid_fits_as_many_cards_as_it_has_room_for() {
        // Five 172-wide cards with 12 between them need 908; 920 fits five and
        // not six.
        assert_eq!(grid_columns(px(920.), px(172.), px(12.)), 5);
        assert_eq!(grid_columns(px(907.), px(172.), px(12.)), 4);
        // Exactly one card's worth of room is one card, and a window narrower
        // than a single card still draws one rather than none.
        assert_eq!(grid_columns(px(172.), px(172.), px(12.)), 1);
        assert_eq!(grid_columns(px(40.), px(172.), px(12.)), 1);
    }

    #[test]
    fn sort_keys_and_column_keys_agree() {
        // Every sort key must name a column the header can actually draw an
        // arrow on, or the arrow silently vanishes.
        for (key, column) in [
            (SortKey::Title, "title"),
            (SortKey::Artist, "artist"),
            (SortKey::Album, "album"),
            (SortKey::Duration, "duration"),
            (SortKey::PlayCount, "plays"),
            (SortKey::Rating, "rating"),
            (SortKey::Year, "year"),
        ] {
            assert_eq!(sort_column_key(key), column);
        }
    }

    #[test]
    fn every_icon_the_screens_use_exists() {
        for name in [
            "music", "album", "artist", "genre", "calendar", "folder", "playlist", "queue",
            "search", "play", "shuffle", "clock", "plus", "trash", "close",
        ] {
            assert!(icons::source(name).is_some(), "{name} is missing");
        }
    }

    #[test]
    fn a_genre_keeps_its_colour() {
        // The card colour is derived, not stored, so it has to be the same on
        // every run and on every build — otherwise "the blue one" moves.
        assert_eq!(genre_color("Jazz", true), genre_color("Jazz", true));
        assert_eq!(genre_color("Jazz", true), genre_color("jazz", true));
        assert_ne!(genre_color("Jazz", true), genre_color("Metal", true));
        // Light and dark get their own weight of the same idea.
        assert_ne!(genre_color("Jazz", true), genre_color("Jazz", false));
    }

    #[test]
    fn every_genre_colour_is_a_real_colour() {
        for name in ["", "Jazz", "Drum & Bass", "日本のロック", "…"] {
            let color = genre_color(name, true);
            assert!((0.0..=1.0).contains(&color.h), "{name} has hue {}", color.h);
            assert!(color.a > 0.0, "{name} is invisible");
        }
    }
}
