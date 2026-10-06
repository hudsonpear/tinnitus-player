//! The parts of the window that are always there: title bar, sidebar, player
//! bar, and the compact player that replaces all three.

use gpui::prelude::*;
use gpui::{Context, MouseButton, SharedString, Window, WindowControlArea, div, img, px, svg};
use library::ThumbSize;
use library::models::{LibraryView, Track};
use playlist::Repeat;
use state::{Timeline, TimelinePlace};
use ui::{
    ActiveTheme as _, Button, Scrollbar, Separator, Slider, Text, clock, eyebrow, faint, scrolled,
};

use crate::root::{MenuTarget, Root};
use crate::screen::Screen;

/// The top bar: identity on the left, search in the middle, controls on the right.
pub fn title_bar(root: &Root, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let height = theme.metrics.title_bar;
    let inset = theme.metrics.pad;
    let gap = theme.metrics.gap;
    let surface = theme.surface;
    let hover = theme.hover;
    let label = theme.text(Text::Label);
    let can_back = root.history.can_go_back();
    let can_forward = root.history.can_go_forward();
    let settings = root.settings.read(cx).get();
    let queue_panel = settings.queue_panel;
    // The identity block is held to the width of the sidebar below it, so that
    // Back and Forward begin exactly where the content pane begins: they act on
    // that pane, and standing them beside the app name reads as if they belong
    // to the window.
    let identity = (px(settings.sidebar_width) + px(1.) - inset).max(px(0.));

    // Where the app button ended up last frame, so its menu can hang off the
    // bottom edge of it. Filled in by the canvas below and read by the click
    // handler on the next event, the same way the slider tracks its track.
    let anchor: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>> =
        std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::default()));
    let measure = {
        let anchor = anchor.clone();
        gpui::canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
            .absolute()
            .size_full()
    };

    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(gap)
        // The window buttons run the full height of the bar and sit flush in the
        // corner, so the right-hand padding stops before them.
        .h(height)
        .pl(inset)
        .bg(surface)
        .border_b_1()
        .border_color(theme.border)
        // Dragging anywhere that is not a control moves the window.
        .window_control_area(WindowControlArea::Drag)
        // The block is held to the width of the sidebar below it; the button
        // inside it is only as wide as the mark and the name, so the hover
        // highlight sits on the words rather than on the empty space beside
        // them.
        .child(
            div().flex().flex_none().w(identity).child(
                div()
                    .id("app-menu")
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(theme.radius)
                    .cursor_pointer()
                    // The mark is a menu, so it has to take the click rather
                    // than let the title bar behind it start dragging the
                    // window.
                    .occlude()
                    .hover(move |style| style.bg(hover))
                    .child(measure)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        // Under the button, not under the pointer: this is a
                        // menu bar, and a menu bar's menus hang off their own
                        // button wherever it was clicked.
                        let bounds = anchor.get();
                        this.open_menu(
                            gpui::point(bounds.origin.x, bounds.origin.y + bounds.size.height),
                            MenuTarget::App,
                            cx,
                        );
                    }))
                    // The mark is artwork with colours of its own, so unlike
                    // every other glyph in the bar it is an image rather than a
                    // tinted SVG.
                    .child(img(icons::BRAND).size(px(18.)).flex_none())
                    .child(
                        div()
                            .text_size(label)
                            .text_color(theme.foreground)
                            .child("Tinnitus"),
                    ),
            ),
        )
        .child(
            Button::new("back")
                .ghost()
                .small()
                .icon("chevron-left")
                .tooltip("Back")
                .disabled(!can_back)
                .on_click(cx.listener(|this, _, _window, cx| this.go_back(cx))),
        )
        .child(
            Button::new("forward")
                .ghost()
                .small()
                .icon("chevron-right")
                .tooltip("Forward")
                .disabled(!can_forward)
                .on_click(cx.listener(|this, _, _window, cx| this.go_forward(cx))),
        )
        // Search used to live here. It has a page of its own now, reached from
        // the sidebar — this is just the space that keeps the window controls
        // in the corner.
        .child(div().flex_1().min_w_0())
        .child(
            Button::new("queue-panel")
                .ghost()
                .icon("queue")
                .selected(queue_panel)
                .tooltip(match queue_panel {
                    true => "Hide the queue",
                    false => "Show the queue",
                })
                .on_click(cx.listener(|this, _, _window, cx| this.toggle_queue_panel(cx))),
        )
        .child(
            Button::new("equalizer")
                .ghost()
                .icon("equalizer")
                .tooltip("Equalizer")
                .on_click(cx.listener(|this, _, _window, cx| this.go(Screen::Equalizer, cx))),
        )
        .child(
            Button::new("settings")
                .ghost()
                .icon("settings")
                .tooltip("Settings")
                .on_click(cx.listener(|this, _, _window, cx| this.go(Screen::Settings, cx))),
        )
        .child(window_controls(height, window, cx))
}

/// Minimise, maximise and close. On Windows the platform handles the maximise
/// button itself once the region is declared, which is what makes snap layouts
/// work.
///
/// The buttons are square, the full height of the title bar, and butted against
/// each other, so close occupies the very corner of the window — where a thrown
/// pointer lands.
fn window_controls(
    height: gpui::Pixels,
    window: &mut Window,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let (muted, foreground, danger, danger_foreground) = (
        theme.muted_foreground,
        theme.foreground,
        theme.danger,
        theme.danger_foreground,
    );
    let hover = theme.active;
    let maximized = window.is_maximized();
    let supported = window.window_controls();
    // Square: as wide as the bar is tall.
    let width = height;

    let button =
        move |name: &'static str, icon: &'static str, area: WindowControlArea, closes: bool| {
            div()
                .id(name)
                .group(name)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(width)
                .h(height)
                .cursor_pointer()
                .occlude()
                .window_control_area(area)
                .hover(move |style| style.bg(if closes { danger } else { hover }))
                .child(
                    svg()
                        .path(icons::path(icon))
                        .id("glyph")
                        .size(px(14.))
                        .flex_none()
                        .text_color(muted)
                        .group_hover(name, move |style| {
                            style.text_color(if closes {
                                danger_foreground
                            } else {
                                foreground
                            })
                        }),
                )
        };

    div()
        .flex()
        .flex_none()
        .items_center()
        .h_full()
        .ml(px(4.))
        .when(supported.minimize, |this| {
            this.child(
                button("window-minimize", "minus", WindowControlArea::Min, false).on_click(
                    |_, window: &mut Window, _| {
                        window.minimize_window();
                    },
                ),
            )
        })
        .when(supported.maximize, |this| {
            this.child(button(
                "window-maximize",
                match maximized {
                    true => "restore",
                    false => "maximize",
                },
                WindowControlArea::Max,
                false,
            ))
        })
        .child(
            button("window-close", "close", WindowControlArea::Close, true).on_click(
                |_, window: &mut Window, _| {
                    window.remove_window();
                },
            ),
        )
}

/// Library, playlists and folders down the left-hand side.
pub fn sidebar(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let width = px(root.settings.read(cx).get().sidebar_width);
    let surface = theme.surface;
    let inset = theme.metrics.pad;
    let scan = root.library.read(cx).scan_progress().clone();

    let entries: Vec<(&'static str, &'static str, Screen)> = vec![
        ("home", "Home", Screen::Home),
        ("search", "Search", Screen::Search),
        ("music", "All Songs", Screen::Tracks(LibraryView::AllSongs)),
        ("album", "Albums", Screen::Albums),
        ("artist", "Artists", Screen::Artists),
        ("genre", "Genres", Screen::Genres),
        ("folder", "Folders", Screen::Folders),
        ("playlist", "Playlists", Screen::Playlists),
        // Years, Most Played, Recently Added and Recently Played are
        // deliberately absent: years are reached by clicking one, the other
        // three live on Home.
        ("heart", "Favorites", Screen::Tracks(LibraryView::Favorites)),
        ("queue", "Queue", Screen::Queue),
    ];

    let current = root.history.current().clone();
    // The built-in Favorites is left out: it is the "Favorites" entry above,
    // backed by the flag on each track. Listing it here as well put two rows
    // called Favorites in the sidebar, only one of which had anything in it.
    let playlists: Vec<_> = root
        .library
        .read(cx)
        .playlists()
        .iter()
        // Smart playlists are left out as well: they are not lists the user
        // keeps, they are questions the library answers, and they live on the
        // Playlists page under a heading that says so. Putting them here made
        // them read as ordinary playlists that had gone strange.
        .filter(|list| {
            !matches!(
                list.kind,
                library::models::PlaylistKind::Favorites | library::models::PlaylistKind::Smart
            )
        })
        .cloned()
        .collect();
    let folders = root.library.read(cx).folders().to_vec();

    let scroll = root.area_scroll("sidebar");
    let lists = div()
        .id("sidebar")
        .flex()
        .flex_col()
        .w_full()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .child(
            div()
                .flex()
                .flex_col()
                .p(inset)
                .gap(px(1.))
                .children(entries.into_iter().map(|(icon, label, screen)| {
                    let selected = current == screen;
                    sidebar_row(format!("sidebar-nav-{label}"), icon, label, selected, cx).on_click(
                        cx.listener(move |this, _, _window, cx| {
                            this.go(screen.clone(), cx);
                        }),
                    )
                })),
        )
        .child(Separator::horizontal())
        .child(
            div()
                .flex()
                .flex_col()
                .p(inset)
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(inset)
                        .pb(px(4.))
                        .child(eyebrow("Playlists", cx))
                        .child(
                            Button::new("new-playlist")
                                .ghost()
                                .small()
                                .icon("plus")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.show(crate::dialogs::Dialog::NewPlaylist, cx);
                                })),
                        ),
                )
                .children(playlists.into_iter().map(|list| {
                    let id = list.id;
                    let selected = current == Screen::Tracks(LibraryView::Playlist(id));
                    let icon = match list.kind {
                        library::models::PlaylistKind::Favorites => "heart",
                        _ => "playlist",
                    };
                    sidebar_row(
                        format!("sidebar-playlist-{id}"),
                        icon,
                        list.name.clone(),
                        selected,
                        cx,
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
                })),
        )
        .child(Separator::horizontal())
        .child(
            div()
                .flex()
                .flex_col()
                .p(inset)
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(inset)
                        .pb(px(4.))
                        .child(eyebrow("Folders", cx))
                        .child(
                            Button::new("add-folder")
                                .ghost()
                                .small()
                                .icon("plus")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.dispatch_open_folder(window, cx);
                                })),
                        ),
                )
                .children(folders.into_iter().map(|folder| {
                    let id = folder.id;
                    let selected = current == Screen::Tracks(LibraryView::Folder(id));
                    let name = crate::screens::folder_name(&folder.path);
                    sidebar_row(format!("sidebar-folder-{id}"), "folder", name, selected, cx)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.go(Screen::Tracks(LibraryView::Folder(id)), cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                                this.open_menu(event.position, MenuTarget::Folder(id), cx);
                            }),
                        )
                })),
        );

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(width)
        .h_full()
        .bg(surface)
        .child(scrolled(
            lists,
            Scrollbar::area("sidebar-scrollbar", &scroll),
        ))
        // Progress stays pinned under the lists rather than scrolling away.
        .when(scan.running, |this| this.child(scan_progress(&scan, cx)))
}

/// One row of the sidebar.
///
/// `id` is passed in rather than derived from the label, and every caller scopes
/// it to its own section. The built-in Favorites playlist is named "Favorites"
/// and so is the nav entry above it — two elements sharing an id is one element
/// as far as GPUI is concerned, and the second of them stops taking clicks.
fn sidebar_row(
    id: impl Into<SharedString>,
    icon: &str,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme().clone();
    let label: SharedString = label.into();

    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .h(theme.metrics.sidebar_row)
        .px(theme.metrics.pad)
        .rounded(theme.radius)
        .cursor_pointer()
        .text_size(theme.text(Text::Small))
        .text_color(match selected {
            true => theme.foreground,
            false => theme.muted_foreground,
        })
        .when(selected, |this| this.bg(theme.selected))
        .hover({
            let hover = theme.hover;
            move |style| style.bg(hover)
        })
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(15.))
                .flex_none()
                .text_color(match selected {
                    true => theme.accent,
                    false => theme.muted_foreground,
                }),
        )
        .child(div().flex_1().min_w_0().truncate().child(label))
}

/// What is playing next, down the right-hand side. Always the same list as the
/// Queue screen — the panel is a place to see it without leaving the page you
/// are on, not a second copy of it.
pub fn queue_panel(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.pad;
    let total = root.player.read(cx).queue().len();

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(272.))
        .h_full()
        .bg(theme.surface)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(theme.metrics.gap)
                .px(theme.metrics.inset)
                .py(inset)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(eyebrow("Queue", cx))
                        .child(faint(
                            match total {
                                1 => "1 track".to_owned(),
                                _ => format!("{total} tracks"),
                            },
                            cx,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.))
                        // Everything the queue as a whole can do lives behind
                        // one button rather than a row of them.
                        .child(Button::new("queue-menu").ghost().icon("more").on_click(
                            cx.listener(|this, event: &gpui::ClickEvent, _window, cx| {
                                this.open_menu(event.position(), MenuTarget::Queue, cx);
                            }),
                        ))
                        // The way out, next to the panel's own edge: the title
                        // bar toggle is a long way from where the user is
                        // looking, and the collapsed strip has the matching '<'.
                        .child(
                            Button::new("queue-collapse")
                                .ghost()
                                .icon("chevron-right")
                                .icon_size(px(22.))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.toggle_queue_panel(cx)),
                                ),
                        ),
                ),
        )
        .child(Separator::horizontal())
        .child(crate::screens::queue_list(root, "queue-panel", true, cx))
}

/// The scan indicator at the bottom of the sidebar, with its own cancel.
fn scan_progress(scan: &state::ScanProgress, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let fraction = scan.fraction();
    let inset = theme.metrics.pad;

    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(inset)
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(faint("Scanning music…", cx))
                .child(
                    Button::new("cancel-scan")
                        .ghost()
                        .small()
                        .icon("close")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.library
                                .update(cx, |library, cx| library.cancel_scan(cx));
                        })),
                ),
        )
        .child(
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(fraction))
                        .rounded_full()
                        .bg(theme.accent),
                ),
        )
        .child(faint(format!("{} / {} files", scan.done, scan.total), cx))
}

/// The bar along the bottom: artwork, what is playing, transport, volume.
pub fn player_bar(root: &Root, window: &Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let player = root.player.read(cx);
    let playing = player.is_playing();
    let position = player.position();
    let length = player.length();
    let progress = player.progress();
    let volume = player.volume();
    let max_volume = player.max_volume();
    let muted = player.muted();
    let shuffle = player.shuffle();
    let repeat = player.repeat();
    let speed = player.speed();
    let track = player.current().cloned();
    let title = player.now_playing();
    let detail = player.now_playing_detail();
    let current_id = track.as_ref().map(|track| track.id);
    let favorite = track.as_ref().map(|track| track.favorite).unwrap_or(false);

    let place = root.settings.read(cx).get().timeline_place;
    let height = theme.metrics.player_bar;
    let inset = theme.metrics.inset;
    let gap = theme.metrics.gap;
    let accent = theme.accent;
    let surface = theme.surface;
    let muted_color = theme.muted_foreground;

    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(gap)
        .h(height)
        .px(inset)
        .bg(surface)
        .child(now_playing(
            root,
            track.as_ref(),
            title.clone(),
            detail.clone(),
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(2.))
                // The seek bar sits on whichever side of the buttons the user
                // chose. Built twice rather than hoisted into a variable: each
                // branch needs the context, and only one of them ever runs.
                .when(place == TimelinePlace::Above, |this| {
                    this.child(seek_row(root, window, position, length, progress, cx))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        // The heart for what is playing, where the eye already
                        // is. Nothing playing means nothing to favourite.
                        .child(
                            Button::new("favorite-current")
                                .ghost()
                                .icon(match favorite {
                                    true => "heart-filled",
                                    false => "heart",
                                })
                                .tint(match favorite {
                                    true => accent,
                                    false => muted_color,
                                })
                                .tooltip(match favorite {
                                    true => "Remove from Favorites",
                                    false => "Add to Favorites",
                                })
                                .disabled(current_id.is_none())
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    if let Some(id) = current_id {
                                        this.toggle_favorite(id, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("shuffle")
                                .ghost()
                                .icon("shuffle")
                                .selected(shuffle)
                                .tint(match shuffle {
                                    true => accent,
                                    false => muted_color,
                                })
                                .tooltip(match shuffle {
                                    true => "Shuffle: on",
                                    false => "Shuffle: off",
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.dispatch_shuffle(window, cx)
                                })),
                        )
                        .child(
                            Button::new("previous")
                                .ghost()
                                .icon("previous")
                                .tooltip("Previous")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.player.update(cx, |player, cx| player.previous(cx));
                                })),
                        )
                        .child(
                            Button::new("play")
                                .primary()
                                .icon(match playing {
                                    true => "pause",
                                    false => "play",
                                })
                                .tooltip(match playing {
                                    true => "Pause",
                                    false => "Play",
                                })
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.player.update(cx, |player, cx| player.toggle(cx));
                                })),
                        )
                        .child(
                            Button::new("next")
                                .ghost()
                                .icon("next")
                                .tooltip("Next")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.player.update(cx, |player, cx| player.next(cx));
                                })),
                        )
                        .child(
                            Button::new("repeat")
                                .ghost()
                                .icon(match repeat {
                                    Repeat::One => "repeat-one",
                                    _ => "repeat",
                                })
                                .selected(repeat != Repeat::Off)
                                .tint(match repeat {
                                    Repeat::Off => muted_color,
                                    _ => accent,
                                })
                                .tooltip(match repeat {
                                    Repeat::Off => "Repeat: off",
                                    Repeat::All => "Repeat: all",
                                    Repeat::One => "Repeat: this track",
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.dispatch_repeat(window, cx)
                                })),
                        )
                        .child(
                            Button::new("add-current-to-playlist")
                                .ghost()
                                .icon("playlist")
                                .tooltip("Add to playlist")
                                .disabled(current_id.is_none())
                                .on_click(cx.listener(
                                    move |this, event: &gpui::ClickEvent, _window, cx| {
                                        if let Some(id) = current_id {
                                            this.open_menu(
                                                event.position(),
                                                MenuTarget::AddToPlaylist(id),
                                                cx,
                                            );
                                        }
                                    },
                                )),
                        ),
                )
                .when(place == TimelinePlace::Below, |this| {
                    this.child(seek_row(root, window, position, length, progress, cx))
                }),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                // The percentage label only exists while the ceiling is raised,
                // so the strip only widens for the people who asked for it.
                .w(px(if max_volume > 1.0 { 238. } else { 190. }))
                .child(
                    Button::new("speed")
                        .ghost()
                        .small()
                        .label(format!("{speed:.2}x"))
                        .tooltip("Playback speed")
                        .on_click(cx.listener(|this, _, _window, cx| this.cycle_speed(cx))),
                )
                .child(
                    Button::new("mute")
                        .ghost()
                        .icon(match (muted, volume) {
                            (true, _) => "volume-mute",
                            (false, level) if level < 0.5 => "volume-low",
                            _ => "volume",
                        })
                        .tooltip(match muted {
                            true => "Unmute",
                            false => "Mute",
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.player.update(cx, |player, cx| player.toggle_mute(cx));
                        })),
                )
                .child(
                    Slider::new(
                        "volume",
                        match muted {
                            true => 0.0,
                            false => volume,
                        },
                    )
                    .on_change(cx.listener(|this, value: &f32, _window, cx| {
                        this.player
                            .update(cx, |player, cx| player.set_volume(*value, cx));
                    }))
                    .on_commit(cx.listener(
                        |this, value: &f32, _window, cx| {
                            // With per-song audio on, the player has already
                            // remembered this against the track. Writing it to
                            // the settings file as well would make one song's
                            // volume the default for every song without one.
                            if this.player.read(cx).per_track_target().is_some() {
                                return;
                            }
                            this.settings
                                .update(cx, |store, cx| store.update(|s| s.volume = *value, cx));
                        },
                    )),
                )
                // Boosting past the file's own level is not something to leave
                // unsaid: once the ceiling is up, the bar says how loud it is
                // actually asking for.
                .when(max_volume > 1.0, |this| {
                    let gain = state::effective_gain(volume, max_volume);
                    this.child(
                        div()
                            .flex_none()
                            .w(px(42.))
                            .text_size(theme.text(Text::Small))
                            .text_color(match muted {
                                true => theme.faint_foreground,
                                false => theme.warning,
                            })
                            .child(format!("{:.0}%", gain * 100.)),
                    )
                }),
        )
}

/// How far apart the points of the waveform sit. Fine enough that the curve
/// through them follows the song, coarse enough that it reads as a shape rather
/// than as noise.
const WAVE_PITCH: gpui::Pixels = px(4.);

/// How tall the waveform stands. Enough to read as a shape, low enough that it
/// stays a seek bar rather than taking over the player.
const WAVE_HEIGHT: gpui::Pixels = px(26.);

/// How much of the window the waveform strip takes. Short of the full width:
/// running it edge to edge leaves the clocks crammed into the corners with
/// nothing between them and the window frame.
const WAVE_WIDTH: f32 = 0.88;

/// The seek bar and the two clocks around it, drawn as a plain line or as the
/// song's own shape depending on the setting.
///
/// The waveform is the same `Slider`, so clicking and dragging to seek behave
/// identically — only the paint differs. A track whose peaks have not been read
/// yet falls back to the line rather than a row of stubs.
fn seek_row(
    root: &Root,
    window: &Window,
    position: f64,
    length: f64,
    progress: f32,
    cx: &mut Context<Root>,
) -> gpui::AnyElement {
    // Enough points to fill the width the row will actually get: a fixed count
    // draws fat stubs on a wide window and a smear on a narrow one.
    let bars =
        ((window.viewport_size().width * WAVE_WIDTH - px(240.)) / WAVE_PITCH).max(32.) as usize;
    // A track whose peaks are still being read draws as a flat line inside the
    // same strip, rather than the row turning back into a plain seek bar for a
    // second: the player must not change shape every time a song starts.
    let waveform = match root.settings.read(cx).get().timeline {
        Timeline::Waveform => Some(audio::peaks::resample(
            root.player.read(cx).waveform(),
            bars,
        )),
        Timeline::Bar => None,
    };
    let wave = waveform.is_some();

    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .w_full()
        // The line is a control among other controls, so it keeps to the middle
        // of the bar. The waveform is the picture of the song and wants room —
        // most of the window, though not quite all of it.
        .map(|this| match wave {
            true => this.max_w(gpui::relative(WAVE_WIDTH)),
            false => this.max_w(px(560.)),
        })
        .child(faint(clock(position), cx).flex_none())
        .child(
            Slider::new("seek", progress)
                .disabled(length <= 0.0)
                .when_some(waveform, |slider, bars| {
                    slider.waveform(bars).thickness(WAVE_HEIGHT)
                })
                .on_change(cx.listener(|this, value: &f32, _window, cx| {
                    this.player
                        .update(cx, |player, cx| player.scrub_to(*value, cx));
                }))
                .on_commit(cx.listener(|this, _value: &f32, _window, cx| {
                    this.player.update(cx, |player, cx| player.commit_scrub(cx));
                })),
        )
        .child(faint(clock(length), cx).flex_none())
        .into_any_element()
}

/// Artwork plus title and artist, at the left end of the player bar.
fn now_playing(
    root: &Root,
    track: Option<&Track>,
    title: Option<String>,
    detail: Option<String>,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let cover = theme.metrics.cover;
    let radius = theme.radius;
    let raised = theme.raised;
    let faint_color = theme.faint_foreground;

    let artwork =
        track.and_then(|track| root.library.read(cx).artwork_path(track, ThumbSize::Small));
    let album = track
        .and_then(|track| track.album.clone())
        .filter(|album| !album.trim().is_empty());

    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(theme.metrics.gap)
        .w(px(280.))
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(cover)
                .rounded(radius)
                .bg(raised)
                .overflow_hidden()
                .map(|this| match artwork {
                    Some(path) => this.child(img(path).size(cover).rounded(radius)),
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
                // Three lines in the height of two: the default line box leaves
                // enough air between them that title, artist and album read as
                // separate things rather than as one block about one song.
                .line_height(px(18.))
                .child(
                    div()
                        .truncate()
                        .text_color(match title.is_some() {
                            true => theme.foreground,
                            false => theme.muted_foreground,
                        })
                        .child(SharedString::from(
                            title.unwrap_or_else(|| "Nothing playing".to_owned()),
                        )),
                )
                .child(faint(detail.unwrap_or_else(|| "—".to_owned()), cx))
                // The album, when the file admits to one. A third line is only
                // worth the height when it says something, so a single or an
                // untagged file keeps the two it had.
                .when_some(album, |this, album| {
                    this.child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(faint_color)
                            .child(SharedString::from(album)),
                    )
                }),
        )
}

/// A dismissible message strip. Errors are shown, not swallowed.
pub fn notice(message: String, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();

    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(theme.metrics.gap)
        .px(theme.metrics.inset)
        .py(px(6.))
        .bg(theme.raised)
        .border_t_1()
        .border_color(theme.border)
        .text_size(theme.text(Text::Small))
        .child(
            svg()
                .path(icons::path("warning"))
                .size(px(15.))
                .flex_none()
                .text_color(theme.warning),
        )
        .child(div().flex_1().min_w_0().truncate().child(message))
        .child(
            Button::new("dismiss-notice")
                .ghost()
                .small()
                .icon("close")
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.notice = None;
                    cx.notify();
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_the_chrome_asks_for_exists() {
        for name in [
            // The app mark is not here: it is a PNG rather than an SVG, and
            // `assets` covers it.
            "chevron-left",
            "chevron-right",
            "equalizer",
            "settings",
            "home",
            "minus",
            "maximize",
            "restore",
            "close",
            "music",
            "album",
            "artist",
            "genre",
            "calendar",
            "folder",
            "clock",
            "refresh",
            "heart",
            "queue",
            "playlist",
            "more",
            "plus",
            "shuffle",
            "previous",
            "play",
            "pause",
            "next",
            "repeat",
            "repeat-one",
            "volume",
            "volume-low",
            "volume-mute",
            "warning",
        ] {
            assert!(icons::source(name).is_some(), "{name} is missing");
        }
    }
}
