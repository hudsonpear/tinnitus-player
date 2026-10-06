//! Modals, the command palette, and the two full-screen panels (settings and
//! the equalizer).

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, MouseButton, SharedString, Window, deferred, div, img, px, svg,
};
use library::ThumbSize;
use library::models::TrackId;
use state::{Timeline, TimelinePlace};
use ui::theme::{Density, Look, Rounding};
use ui::{
    ActiveTheme as _, Button, Scrollbar, Separator, Slider, Text, eyebrow, faint, heading, scrolled,
};

use crate::actions::SHORTCUTS;
use crate::root::Root;
use crate::screen::Screen;

/// A modal over the window. Data only: the text fields live on `Root`, because
/// they have to survive a re-render.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    NewPlaylist,
    RenamePlaylist(i64),
    /// A new playlist with tracks already in it — the whole queue, or the one
    /// song playing. Carries the tracks rather than reading them back when the
    /// dialog is committed: the user can keep playing while it is open, and what
    /// they asked to save is what was there when they asked.
    NewPlaylistWith(Vec<TrackId>),
    /// Sending files to the Recycle Bin is always confirmed, and always names
    /// them.
    ConfirmDelete(Vec<TrackId>),
    /// Dropping a folder throws away every row scanned from it, which is a lot
    /// to lose to a mis-click on a button that sits on the row itself.
    ConfirmRemoveFolder(i64),
    EditMetadata(Vec<TrackId>),
    /// Everything we know about one file: its art, its tags, and the properties
    /// of the stream itself. Read-only — editing is the dialog above.
    MediaInfo(TrackId),
    /// A smart playlist's name and rules. `None` is a new one, `Some(id)` is
    /// editing the rules of one that exists.
    SmartPlaylist(Option<i64>),
    /// What this is and which build of it you are running.
    About,
}

/// The most rules one smart playlist may carry.
///
/// ponytail: a cap, so the value fields can come from a fixed pool of keys
/// rather than from a dynamic set of entities. Eight rules is far past what
/// anyone reads back later; raise it here and in `Dialog::fields` together if
/// that turns out to be wrong.
pub const MAX_RULES: usize = 8;

impl Dialog {
    pub fn title(&self) -> &'static str {
        match self {
            Self::NewPlaylist => "New playlist",
            Self::RenamePlaylist(_) => "Rename playlist",
            Self::NewPlaylistWith(_) => "New playlist",
            Self::ConfirmDelete(_) => "Delete files",
            Self::ConfirmRemoveFolder(_) => "Remove folder",
            Self::EditMetadata(_) => "Edit metadata",
            Self::MediaInfo(_) => "Media info",
            Self::SmartPlaylist(None) => "New smart playlist",
            Self::SmartPlaylist(Some(_)) => "Edit smart playlist",
            Self::About => "About Tinnitus Player",
        }
    }

    /// Fields the dialog needs, by key. Created when it opens.
    pub fn fields(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::NewPlaylist | Self::RenamePlaylist(_) | Self::NewPlaylistWith(_) => {
                &[("name", "Name")]
            }
            Self::ConfirmDelete(_)
            | Self::ConfirmRemoveFolder(_)
            | Self::MediaInfo(_)
            | Self::About => &[],
            // The name, then one value field per possible rule. Fixed keys, so
            // adding and removing rule rows moves data around on `Root` rather
            // than creating and destroying entities mid-render.
            Self::SmartPlaylist(_) => &[
                ("name", "Name"),
                ("rule-0", "Value"),
                ("rule-1", "Value"),
                ("rule-2", "Value"),
                ("rule-3", "Value"),
                ("rule-4", "Value"),
                ("rule-5", "Value"),
                ("rule-6", "Value"),
                ("rule-7", "Value"),
            ],
            Self::EditMetadata(_) => &[
                ("title", "Title"),
                ("artist", "Artist"),
                ("album", "Album"),
                ("album_artist", "Album artist"),
                ("genre", "Genre"),
                ("year", "Year"),
                ("track", "Track"),
                ("disc", "Disc"),
                ("composer", "Composer"),
                ("comment", "Comment"),
            ],
        }
    }
}

pub fn modal(root: &Root, dialog: Dialog, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let overlay = theme.overlay;
    let border = theme.border_strong;
    let radius = theme.radius;
    let inset = theme.metrics.inset;
    let gap = theme.metrics.gap;

    let body = match &dialog {
        Dialog::ConfirmDelete(ids) => confirm_delete(root, ids.clone(), cx),
        Dialog::ConfirmRemoveFolder(id) => confirm_remove_folder(root, *id, cx),
        Dialog::EditMetadata(ids) => edit_metadata(root, ids.clone(), cx),
        Dialog::MediaInfo(id) => media_info(root, *id, cx),
        Dialog::About => about(root, cx),
        Dialog::SmartPlaylist(_) => smart_playlist_form(root, cx),
        _ => name_form(root, cx),
    };

    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0., 0., 0., 0.45))
            // Clicking the backdrop closes, like every other modal on the desktop.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx)),
            )
            .child(
                div()
                    .w(px(520.))
                    .max_w(gpui::relative(0.9))
                    .flex()
                    .flex_col()
                    .gap(gap)
                    .p(inset)
                    .rounded(radius)
                    .bg(overlay)
                    .border_1()
                    .border_color(border)
                    .shadow_lg()
                    .occlude()
                    .child(heading(dialog.title().to_owned(), cx))
                    .child(body),
            ),
    )
    .with_priority(2)
    .into_any_element()
}

/// Who wrote this, and where it lives.
const AUTHOR: &str = "Hudson Pear";
const REPOSITORY: &str = "https://github.com/hudsonpear/tinnitus-player";

/// What the app is, what build it is, and how much of the user's music it has
/// found — the last of which is the only thing here they cannot see elsewhere.
fn about(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let library = root.library.read(cx);
    let (albums, artists, folders) = (
        library.albums().len(),
        library.artists().len(),
        library.folders().len(),
    );

    div()
        .flex()
        .flex_col()
        .gap(theme.metrics.gap)
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme.metrics.gap)
                .child(img(icons::BRAND).size(px(48.)).flex_none())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_size(theme.text(Text::Large)).child("Tinnitus"))
                        .child(faint(format!("Version {}", env!("CARGO_PKG_VERSION")), cx))
                        .child(faint(format!("By {AUTHOR}"), cx)),
                ),
        )
        .child(faint(
            "A music player for local files. Your library stays on your machine.".to_owned(),
            cx,
        ))
        .child(faint(
            format!("{albums} albums, {artists} artists, {folders} folders watched"),
            cx,
        ))
        .child(
            div()
                .id("about-repository")
                .w_full()
                .text_size(theme.text(Text::Small))
                .text_color(theme.accent)
                .cursor_pointer()
                .child(REPOSITORY)
                // The repository opens in whatever browser the user actually
                // uses, which is the platform's business rather than ours.
                .on_click(|_, _window, cx| cx.open_url(REPOSITORY)),
        )
        .child(
            div().flex().justify_end().child(
                Button::new("close-about")
                    .primary()
                    .label("Close")
                    .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
            ),
        )
        .into_any_element()
}

/// The single-field dialogs: new playlist, rename playlist.
fn name_form(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let field = root.dialog_field("name");

    div()
        .flex()
        .flex_col()
        .gap(theme.metrics.gap)
        .when_some(field, |this, field| this.child(field))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    Button::new("cancel")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
                )
                .child(
                    Button::new("confirm")
                        .primary()
                        .label("Save")
                        .on_click(cx.listener(|this, _, _window, cx| this.commit_dialog(cx))),
                ),
        )
        .into_any_element()
}

/// The smart playlist editor: a name, a row per rule, and the two things that
/// apply to the set as a whole.
///
/// The rule rows are data on `Root` rather than fields created on the fly; the
/// value boxes come from a fixed pool of keys. That is what keeps adding and
/// removing a row from being entity bookkeeping in the middle of a render.
fn smart_playlist_form(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let gap = theme.metrics.gap;
    let rules = root.smart_draft.clone();
    let picker = root.smart_picker;
    let full = rules.rules.len() >= MAX_RULES;
    let capped = rules.limit.is_some();

    // Collected rather than left lazy: a lazy iterator would hold `cx` for the
    // rest of the form, and everything below here needs it too.
    let rows: Vec<_> = rules
        .rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let only = rules.rules.len() <= 1;
            let field_open = picker == Some((index, true));
            let op_open = picker == Some((index, false));
            let (field, op) = (rule.field, rule.op);

            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            Button::new(SharedString::from(format!("rule-field-{index}")))
                                .small()
                                .label(field.label())
                                .selected(field_open)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.toggle_rule_picker(index, true, cx);
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("rule-op-{index}")))
                                .small()
                                .label(op.label())
                                .selected(op_open)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.toggle_rule_picker(index, false, cx);
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .children(root.dialog_field(&format!("rule-{index}"))),
                        )
                        .child(
                            Button::new(SharedString::from(format!("rule-drop-{index}")))
                                .ghost()
                                .small()
                                .icon("minus")
                                .disabled(only)
                                .tooltip("Remove this rule")
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.remove_rule(index, cx);
                                })),
                        ),
                )
                // The lists open under the row they belong to, one at a time, so
                // the dialog is a form rather than a wall of buttons.
                .when(field_open, |this| {
                    this.child(picker_row(
                        library::smart::Field::ALL.iter().map(|choice| {
                            let choice = *choice;
                            (
                                format!("pick-field-{index}-{}", choice.label()),
                                choice.label(),
                                choice == field,
                            )
                        }),
                        move |this, label, cx| {
                            if let Some(choice) = library::smart::Field::ALL
                                .iter()
                                .find(|choice| choice.label() == label)
                            {
                                this.set_rule_field(index, *choice, cx);
                            }
                        },
                        cx,
                    ))
                })
                .when(op_open, |this| {
                    let allowed = library::smart::Op::for_field(field);
                    this.child(picker_row(
                        allowed.iter().map(|choice| {
                            let choice = *choice;
                            (
                                format!("pick-op-{index}-{}", choice.label()),
                                choice.label(),
                                choice == op,
                            )
                        }),
                        move |this, label, cx| {
                            if let Some(choice) = library::smart::Op::for_field(field)
                                .iter()
                                .find(|choice| choice.label() == label)
                            {
                                this.set_rule_op(index, *choice, cx);
                            }
                        },
                        cx,
                    ))
                })
        })
        .collect();

    div()
        .flex()
        .flex_col()
        .gap(gap)
        .children(root.dialog_field("name"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(faint("Match".to_owned(), cx))
                .child(
                    Button::new("rules-all")
                        .small()
                        .label("all rules")
                        .selected(rules.match_all)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.set_rule_match_all(true, cx);
                        })),
                )
                .child(
                    Button::new("rules-any")
                        .small()
                        .label("any rule")
                        .selected(!rules.match_all)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.set_rule_match_all(false, cx);
                        })),
                ),
        )
        .child(div().flex().flex_col().gap(px(6.)).children(rows))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    Button::new("rule-add")
                        .small()
                        .icon("plus")
                        .label("Add rule")
                        .disabled(full)
                        .on_click(cx.listener(|this, _, _window, cx| this.add_rule(cx))),
                )
                .child(
                    Button::new("rule-limit")
                        .small()
                        .label(match rules.limit {
                            Some(limit) => format!("Newest {limit}"),
                            None => "Everything that matches".to_owned(),
                        })
                        .selected(capped)
                        // Cycles through the handful of caps anyone actually
                        // wants, rather than earning a number field of its own.
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            let next = match rules.limit {
                                None => Some(25),
                                Some(25) => Some(50),
                                Some(50) => Some(100),
                                _ => None,
                            };
                            this.set_rule_limit(next, cx);
                        })),
                ),
        )
        .child(faint(
            "A smart playlist stores rules rather than songs, so it is up to \
             date every time you open it."
                .to_owned(),
            cx,
        ))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    Button::new("cancel")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
                )
                .child(
                    Button::new("confirm")
                        .primary()
                        .label("Save")
                        .on_click(cx.listener(|this, _, _window, cx| this.commit_dialog(cx))),
                ),
        )
        .into_any_element()
}

/// A wrapped row of small buttons, one of them lit. Used for the field and
/// operator lists under a rule row.
fn picker_row(
    choices: impl Iterator<Item = (String, &'static str, bool)>,
    pick: impl Fn(&mut Root, &'static str, &mut Context<Root>) + Clone + 'static,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_wrap()
        .gap(px(4.))
        .pl(px(8.))
        .children(choices.map(|(id, label, selected)| {
            let pick = pick.clone();
            Button::new(SharedString::from(id))
                .small()
                .label(label)
                .selected(selected)
                .on_click(cx.listener(move |this, _, _window, cx| pick(this, label, cx)))
        }))
}

/// Deleting files is the one destructive thing here, so it names the files and
/// says where they are going.
fn confirm_delete(root: &Root, ids: Vec<TrackId>, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let names = root.track_names(&ids, cx);
    let count = ids.len();

    div()
        .flex()
        .flex_col()
        .gap(theme.metrics.gap)
        .child(faint(
            match count {
                1 => {
                    "This file goes to the Recycle Bin, and its row leaves the library.".to_owned()
                }
                _ => format!(
                    "{count} files go to the Recycle Bin, and their rows leave the library."
                ),
            },
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .max_h(px(180.))
                .overflow_hidden()
                .children(names.into_iter().take(8).map(|name| faint(name, cx))),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    Button::new("cancel-delete")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
                )
                .child(
                    Button::new("confirm-delete")
                        .danger()
                        .icon("trash")
                        .label("Delete")
                        .on_click(cx.listener(|this, _, _window, cx| this.commit_dialog(cx))),
                ),
        )
        .into_any_element()
}

/// Removing a folder takes its tracks out of the library. It names the folder,
/// and says plainly that the files themselves are left alone — otherwise the
/// wording is indistinguishable from deleting them.
fn confirm_remove_folder(root: &Root, id: i64, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let path = root
        .folder_path(id, cx)
        .unwrap_or_else(|| "this folder".to_owned());

    div()
        .flex()
        .flex_col()
        .gap(theme.metrics.gap)
        .child(faint(
            "Its tracks leave the library, along with their play counts and \
             favourites. Nothing is deleted from disk."
                .to_owned(),
            cx,
        ))
        .child(div().truncate().child(SharedString::from(path)))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    Button::new("cancel-remove-folder")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
                )
                .child(
                    Button::new("confirm-remove-folder")
                        .danger()
                        .icon("trash")
                        .label("Remove")
                        .on_click(cx.listener(|this, _, _window, cx| this.commit_dialog(cx))),
                ),
        )
        .into_any_element()
}

/// The metadata editor. Blank fields are left alone, which is what makes batch
/// editing across a mixed selection safe.
fn edit_metadata(root: &Root, ids: Vec<TrackId>, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let count = ids.len();
    let fields_scroll = root.area_scroll("metadata-fields");

    div()
        .flex()
        .flex_col()
        .gap(theme.metrics.gap)
        .child(faint(
            match count {
                1 => "Changes are written to the file when you save.".to_owned(),
                _ => format!("Editing {count} tracks. Fields you leave blank are not changed."),
            },
            cx,
        ))
        .child(
            div()
                .group(ui::SCROLL_REGION)
                .relative()
                .flex()
                .flex_col()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .max_h(px(340.))
                        .id("metadata-fields")
                        .overflow_y_scroll()
                        .track_scroll(&fields_scroll)
                        .children(Dialog::EditMetadata(ids).fields().iter().filter_map(
                            |(key, label)| {
                                let field = root.dialog_field(key)?;
                                Some(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(theme.metrics.gap)
                                        .child(div().w(px(110.)).flex_none().child(*label))
                                        .child(div().flex_1().child(field)),
                                )
                            },
                        )),
                )
                .child(Scrollbar::area("metadata-fields-scrollbar", &fields_scroll)),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    Button::new("cancel-edit")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
                )
                .child(
                    Button::new("apply-edit")
                        .primary()
                        .label("Apply")
                        .on_click(cx.listener(|this, _, _window, cx| this.commit_dialog(cx))),
                ),
        )
        .into_any_element()
}

/// A `YYYY-MM-DD HH:MM` stamp from milliseconds since the epoch.
///
/// UTC rather than local time: the platform's timezone database is a dependency
/// we do not otherwise carry, and a stamp that is off by an hour still answers
/// the only question anyone asks of it — which of two files is older.
fn stamp(millis: library::models::Millis) -> String {
    if millis <= 0 {
        return String::new();
    }
    let seconds = millis / 1000;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (hour, minute) = (rest / 3600, (rest % 3600) / 60);

    // Howard Hinnant's civil-from-days, with the era shifted so 1970-01-01 is
    // day zero.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// `4.7 MB`. Bytes are never the unit anyone wants for a song.
fn file_size(bytes: i64) -> String {
    if bytes <= 0 {
        return String::new();
    }
    let bytes = bytes as f64;
    match bytes {
        b if b < 1024. => format!("{bytes:.0} bytes"),
        b if b < 1024. * 1024. => format!("{:.0} KB", b / 1024.),
        b => format!("{:.1} MB", b / (1024. * 1024.)),
    }
}

/// What the tag names its channel count as words, since "2" is not what anyone
/// calls it.
fn channels(count: u16) -> String {
    match count {
        1 => "Mono".to_owned(),
        2 => "Stereo".to_owned(),
        other => format!("{other} channels"),
    }
}

/// A labelled value in the media info panel. Blank values never reach here —
/// the panel drops them rather than showing a row with nothing in it.
fn info_row(label: &'static str, value: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .items_start()
        .gap(theme.metrics.gap)
        .child(
            div()
                .w(px(104.))
                .flex_none()
                .text_size(theme.text(Text::Small))
                .text_color(theme.faint_foreground)
                .child(label),
        )
        .child(
            // No `truncate`: a path is the one value here worth wrapping onto a
            // second line rather than cutting off.
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.text(Text::Small))
                .child(SharedString::from(value)),
        )
        .into_any_element()
}

/// Everything one file carries: its cover, the tags a tag editor would show, and
/// the properties of the stream itself. Read-only — the pencil is next door.
fn media_info(root: &Root, id: TrackId, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let library = root.library.read(cx);
    let Some(track) = library.track(id) else {
        return faint("That track is no longer in the library.".to_owned(), cx).into_any_element();
    };
    let artwork = library.artwork_path(&track, ThumbSize::Medium);
    let scroll = root.area_scroll("media-info");
    let art = px(96.);

    let text = |value: &Option<String>| value.clone().unwrap_or_default();
    let number = |value: Option<u32>| value.map(|n| n.to_string()).unwrap_or_default();
    let gain = |db: Option<f32>| db.map(|db| format!("{db:+.2} dB")).unwrap_or_default();
    let peak = |peak: Option<f32>| peak.map(|peak| format!("{peak:.6}")).unwrap_or_default();

    let sections: Vec<(&'static str, Vec<(&'static str, String)>)> = vec![
        (
            "Tags",
            vec![
                ("Title", track.title.clone()),
                ("Artist", track.artist.clone()),
                ("Album", text(&track.album)),
                ("Album artist", text(&track.album_artist)),
                ("Composer", text(&track.composer)),
                ("Genre", text(&track.genre)),
                (
                    "Year",
                    track.year.map(|y| y.to_string()).unwrap_or_default(),
                ),
                ("Track", number(track.track_number)),
                ("Disc", number(track.disc_number)),
                ("BPM", number(track.bpm)),
                ("Comment", text(&track.comment)),
            ],
        ),
        (
            "Audio",
            vec![
                ("Format", text(&track.codec).to_uppercase()),
                ("Length", ui::clock(track.duration)),
                (
                    "Bitrate",
                    track
                        .bitrate
                        .map(|rate| format!("{rate} kbps"))
                        .unwrap_or_default(),
                ),
                (
                    "Sample rate",
                    track
                        .sample_rate
                        .map(|rate| format!("{:.1} kHz", f64::from(rate) / 1000.))
                        .unwrap_or_default(),
                ),
                ("Channels", track.channels.map(channels).unwrap_or_default()),
                ("Track gain", gain(track.replay_gain.track_gain)),
                ("Track peak", peak(track.replay_gain.track_peak)),
                ("Album gain", gain(track.replay_gain.album_gain)),
                ("Album peak", peak(track.replay_gain.album_peak)),
            ],
        ),
        (
            "File",
            vec![
                ("Location", track.path.display().to_string()),
                ("Size", file_size(track.file_size)),
                ("Added", stamp(track.date_added)),
                (
                    "Last played",
                    track.last_played.map(stamp).unwrap_or_default(),
                ),
                (
                    "Plays",
                    match track.play_count {
                        0 => String::new(),
                        count => count.to_string(),
                    },
                ),
                (
                    "Rating",
                    match track.rating {
                        0 => String::new(),
                        stars => "★".repeat(stars.min(5) as usize),
                    },
                ),
                (
                    "Status",
                    match track.missing {
                        true => "File not found on disk".to_owned(),
                        false => String::new(),
                    },
                ),
            ],
        ),
    ];

    let heading_line = track.title.clone();
    let under = [track.artist.clone(), text(&track.album)]
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" — ");

    div()
        .flex()
        .flex_col()
        .gap(theme.metrics.gap)
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme.metrics.gap)
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(art)
                        .rounded(theme.radius)
                        .bg(theme.background)
                        .overflow_hidden()
                        .map(|this| match artwork {
                            Some(path) => this.child(img(path).size(art).rounded(theme.radius)),
                            None => this.child(
                                svg()
                                    .path(icons::path("music"))
                                    .size(px(28.))
                                    .text_color(theme.faint_foreground),
                            ),
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(theme.text(Text::Large))
                                .truncate()
                                .child(SharedString::from(heading_line)),
                        )
                        .child(faint(under, cx)),
                ),
        )
        .child(
            div()
                .group(ui::SCROLL_REGION)
                .relative()
                .flex()
                .flex_col()
                .min_h_0()
                .child(
                    div()
                        .id("media-info")
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .max_h(px(340.))
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .children(sections.into_iter().filter_map(|(name, rows)| {
                            let rows: Vec<AnyElement> = rows
                                .into_iter()
                                .filter(|(_, value)| !value.trim().is_empty())
                                .map(|(label, value)| info_row(label, value, cx))
                                .collect();
                            // A section with nothing in it — a file with no
                            // ReplayGain, say — is left out rather than headed.
                            if rows.is_empty() {
                                return None;
                            }
                            Some(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(eyebrow(name, cx))
                                    .children(rows),
                            )
                        })),
                )
                .child(Scrollbar::area("media-info-scrollbar", &scroll)),
        )
        .child(
            div().flex().justify_end().child(
                Button::new("close-media-info")
                    .primary()
                    .label("Close")
                    .on_click(cx.listener(|this, _, _window, cx| this.dismiss_dialog(cx))),
            ),
        )
        .into_any_element()
}

/// The command palette: songs first, then commands.
pub fn palette(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(field) = root.palette.clone() else {
        return div().into_any_element();
    };
    let results = root.library.read(cx).search_results().clone();
    let palette_scroll = root.area_scroll("palette");

    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(90.))
            .bg(gpui::hsla(0., 0., 0., 0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.palette = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(560.))
                    .max_w(gpui::relative(0.92))
                    .flex()
                    .flex_col()
                    .rounded(theme.radius)
                    .bg(theme.overlay)
                    .border_1()
                    .border_color(theme.border_strong)
                    .shadow_lg()
                    .occlude()
                    .child(div().p(px(8.)).child(field))
                    .child(Separator::horizontal())
                    .child(
                        // A capped popover, not a pane that fills its parent, so
                        // the scrollbar is layered on by hand rather than with
                        // `scrolled`, which stretches what it wraps.
                        div()
                            .group(ui::SCROLL_REGION)
                            .relative()
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .child(
                                div()
                                    .id("palette-results")
                                    .flex()
                                    .flex_col()
                                    .max_h(px(380.))
                                    .overflow_y_scroll()
                                    .track_scroll(&palette_scroll)
                                    .p(px(6.))
                                    .when(!results.tracks.is_empty(), |this| {
                                        this.child(eyebrow("Songs", cx).px(px(8.)).py(px(4.)))
                                            .children(results.tracks.iter().take(6).map(|track| {
                                                let id = track.id;
                                                palette_row(
                                                    "music",
                                                    format!("{} — {}", track.title, track.artist),
                                                    None,
                                                    cx,
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, _window, cx| {
                                                        this.player.update(cx, |player, cx| {
                                                            player.play_now(id, cx)
                                                        });
                                                        this.palette = None;
                                                        cx.notify();
                                                    },
                                                ))
                                            }))
                                    })
                                    .child(eyebrow("Commands", cx).px(px(8.)).py(px(4.)))
                                    .children(commands(cx)),
                            )
                            .child(Scrollbar::area("palette-scrollbar", &palette_scroll)),
                    ),
            ),
    )
    .with_priority(2)
    .into_any_element()
}

fn commands(cx: &mut Context<Root>) -> Vec<gpui::Stateful<gpui::Div>> {
    let entries: Vec<(&'static str, &'static str, &'static str, Screen)> = vec![
        ("equalizer", "Open Equalizer", "Ctrl+E", Screen::Equalizer),
        ("settings", "Open Settings", "Ctrl+,", Screen::Settings),
        ("queue", "Show Queue", "Ctrl+J", Screen::Queue),
        ("album", "Browse Albums", "", Screen::Albums),
    ];

    let mut rows: Vec<gpui::Stateful<gpui::Div>> = entries
        .into_iter()
        .map(|(icon, label, keys, screen)| {
            palette_row(icon, label.to_owned(), Some(keys.to_owned()), cx).on_click(cx.listener(
                move |this, _, _window, cx| {
                    this.go(screen.clone(), cx);
                    this.palette = None;
                    cx.notify();
                },
            ))
        })
        .collect();

    rows.push(
        palette_row("shuffle", "Toggle Shuffle".to_owned(), Some("S".into()), cx).on_click(
            cx.listener(|this, _, window, cx| {
                this.dispatch_shuffle(window, cx);
                this.palette = None;
                cx.notify();
            }),
        ),
    );
    rows.push(
        palette_row(
            "refresh",
            "Rescan Library".to_owned(),
            Some("F5".into()),
            cx,
        )
        .on_click(cx.listener(|this, _, _window, cx| {
            this.library.update(cx, |library, cx| library.rescan(cx));
            this.palette = None;
            cx.notify();
        })),
    );
    rows
}

fn palette_row(
    icon: &'static str,
    label: String,
    keys: Option<String>,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme().clone();
    let hover = theme.hover;
    let muted = theme.muted_foreground;
    let faint_color = theme.faint_foreground;

    div()
        .id(SharedString::from(format!("palette-{label}")))
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .h(theme.metrics.row)
        .px(px(8.))
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
        .when_some(keys.filter(|keys| !keys.is_empty()), |this, keys| {
            this.child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(faint_color)
                    .child(keys),
            )
        })
}

// -- settings -------------------------------------------------------------

pub fn settings_screen(root: &Root, _window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    let gap = theme.metrics.gap;
    let settings = root.settings.read(cx).get().clone();

    let scroll = root.area_scroll("settings");
    let bar = Scrollbar::area("settings-scrollbar", &scroll);
    let pane =
        div()
            .id("settings")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(gap)
            .p(inset)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(heading("Settings".to_owned(), cx))
            .child(section("Appearance", cx))
            .child(choice_row(
                "Theme",
                LOOK_LABELS,
                look_index(settings.look),
                cx,
                |this, index, cx| {
                    let look = LOOKS[index];
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.look = look, cx));
                    this.apply_theme(cx);
                },
            ))
            .child(accent_row(settings.accent, cx))
            .child(choice_row(
                "Corners",
                &["Square", "Subtle", "Rounded", "Round"],
                Rounding::ALL
                    .iter()
                    .position(|value| *value == settings.rounding)
                    .unwrap_or(1),
                cx,
                |this, index, cx| {
                    let rounding = Rounding::ALL[index];
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.rounding = rounding, cx));
                    this.apply_theme(cx);
                },
            ))
            .child(choice_row(
                "Density",
                &["Compact", "Normal", "Comfortable"],
                Density::ALL
                    .iter()
                    .position(|value| *value == settings.density)
                    .unwrap_or(1),
                cx,
                |this, index, cx| {
                    let density = Density::ALL[index];
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.density = density, cx));
                    this.apply_theme(cx);
                },
            ))
            .child(choice_row(
                "Timeline",
                &["Bar", "Waveform"],
                Timeline::ALL
                    .iter()
                    .position(|value| *value == settings.timeline)
                    .unwrap_or(0),
                cx,
                |this, index, cx| {
                    let timeline = Timeline::ALL[index];
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.timeline = timeline, cx));
                    // The waveform costs a decode of the playing file, so the player
                    // only reads one while something is drawing it.
                    this.apply_playback(cx);
                },
            ))
            .child(choice_row(
                "Timeline position",
                &["Above the buttons", "Below the buttons"],
                TimelinePlace::ALL
                    .iter()
                    .position(|value| *value == settings.timeline_place)
                    .unwrap_or(0),
                cx,
                |this, index, cx| {
                    let place = TimelinePlace::ALL[index];
                    this.settings.update(cx, |store, cx| {
                        store.update(|s| s.timeline_place = place, cx)
                    });
                },
            ))
            .child(slider_row(
                "font-size",
                format!("Font size ({:.0}px)", settings.font_size),
                (settings.font_size - ui::MIN_FONT) / (ui::MAX_FONT - ui::MIN_FONT),
                cx,
                |this, value, cx| {
                    let size = ui::MIN_FONT + value * (ui::MAX_FONT - ui::MIN_FONT);
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.font_size = size, cx));
                    this.apply_theme(cx);
                },
            ))
            .child(section("Playback", cx))
            .child(toggle_row(
                "Resume where playback stopped",
                settings.resume_playback,
                cx,
                |this, on, cx| {
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.resume_playback = on, cx));
                },
            ))
            .child(toggle_row(
                "Remember volume and equalizer per song",
                settings.remember_per_track_audio,
                cx,
                |this, on, cx| {
                    this.settings.update(cx, |store, cx| {
                        store.update(|s| s.remember_per_track_audio = on, cx)
                    });
                    this.apply_playback(cx);
                },
            ))
            .child(faint(
                "With this on, changing the volume or the equalizer while a song \
                 plays is remembered for that song. Songs with nothing remembered \
                 keep playing at the settings here. Files outside the library have \
                 no row to remember against, so they always do."
                    .to_owned(),
                cx,
            ))
            .child(toggle_row(
                "Crossfade between tracks",
                settings.crossfade.enabled,
                cx,
                |this, on, cx| {
                    this.settings.update(cx, |store, cx| {
                        store.update(|s| s.crossfade.enabled = on, cx)
                    });
                    this.apply_playback(cx);
                },
            ))
            .child(slider_row(
                "crossfade-seconds",
                format!("Crossfade length ({:.1}s)", settings.crossfade.seconds),
                settings.crossfade.seconds / audio::CrossfadeSettings::MAX_SECONDS,
                cx,
                |this, value, cx| {
                    let seconds = (value * audio::CrossfadeSettings::MAX_SECONDS).max(0.5);
                    this.settings.update(cx, |store, cx| {
                        store.update(|s| s.crossfade.seconds = seconds, cx)
                    });
                    this.apply_playback(cx);
                },
            ))
            .child(faint(
                "Crossfade and gapless playback cannot both be on. With crossfade enabled, \
             it wins."
                    .to_owned(),
                cx,
            ))
            .child(slider_row(
                "max-volume",
                format!("Maximum volume ({:.0}%)", settings.max_volume * 100.),
                (settings.max_volume - 1.0) / 9.0,
                cx,
                |this, value, cx| {
                    // Snapped to whole multipliers: "340%" is a number nobody
                    // asked for, and a slider that lands on it is one that
                    // cannot be put back on exactly 300%.
                    let ceiling = (1.0 + value * 9.0).round().clamp(1.0, 10.0);
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.max_volume = ceiling, cx));
                    this.apply_playback(cx);
                },
            ))
            .child(faint(
                "Above 100% the volume slider amplifies past the level in the file. \
                 Loud music at a high setting can damage your hearing and your \
                 speakers. The signal is rounded off rather than clipped square, \
                 but the loudness is real."
                    .to_owned(),
                cx,
            ))
            .child(choice_row(
                "ReplayGain",
                &["Off", "Track", "Album"],
                match settings.replay_gain.mode {
                    audio::ReplayGainMode::Off => 0,
                    audio::ReplayGainMode::Track => 1,
                    audio::ReplayGainMode::Album => 2,
                },
                cx,
                |this, index, cx| {
                    let mode = [
                        audio::ReplayGainMode::Off,
                        audio::ReplayGainMode::Track,
                        audio::ReplayGainMode::Album,
                    ][index];
                    this.settings.update(cx, |store, cx| {
                        store.update(|s| s.replay_gain.mode = mode, cx)
                    });
                    this.apply_playback(cx);
                },
            ))
            .child(toggle_row(
                "Prevent clipping",
                settings.replay_gain.prevent_clipping,
                cx,
                |this, on, cx| {
                    this.settings.update(cx, |store, cx| {
                        store.update(|s| s.replay_gain.prevent_clipping = on, cx)
                    });
                    this.apply_playback(cx);
                },
            ))
            .child(section("Window", cx))
            .child(section("Library", cx))
            .child(toggle_row(
                "Watch folders for changes",
                settings.watch_folders,
                cx,
                |this, on, cx| {
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.watch_folders = on, cx));
                },
            ))
            .child(toggle_row(
                "Scan at start-up",
                settings.scan_at_startup,
                cx,
                |this, on, cx| {
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.scan_at_startup = on, cx));
                },
            ))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(
                        Button::new("add-music-folder")
                            .icon("plus")
                            .label("Add Folder")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dispatch_open_folder(window, cx)
                            })),
                    )
                    .child(
                        Button::new("rescan-now")
                            .icon("refresh")
                            .label("Rescan Now")
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.library.update(cx, |library, cx| library.rescan(cx));
                            })),
                    ),
            )
            .child(section("Keyboard", cx))
            .children(SHORTCUTS.iter().map(|(label, keys)| {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(theme.metrics.row)
                    .child(SharedString::from(*label))
                    .child(faint((*keys).to_owned(), cx))
            }));

    scrolled(pane, bar).into_any_element()
}

fn section(title: &'static str, cx: &mut Context<Root>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .pt(px(12.))
        .child(eyebrow(title, cx))
        .child(Separator::horizontal())
}

/// The colours offered for the accent. The first is Tinnitus's own iris, which is
/// what an unset accent resolves to — so "Default" is a swatch like any other
/// rather than a separate reset button.
const ACCENTS: &[(&str, u32)] = &[
    ("Iris", ui::theme::DEFAULT_ACCENT),
    ("Electric", 0x614efd),
    ("Rust", 0xc65d32),
    ("Ember", 0xd7452f),
    ("Amber", 0xd39a2b),
    ("Moss", 0x5f9c50),
    ("Teal", 0x2f9c93),
    ("Ocean", 0x3b7fd4),
    ("Violet", 0x8464d9),
    ("Magenta", 0xc4529c),
];

/// The colour picker, opened from the last swatch and floating over the window.
/// `at` is where the swatch was pressed; the panel hangs below and to the left of
/// it, pulled back inside the window when that would run off an edge.
pub fn accent_popover(
    root: &Root,
    at: gpui::Point<gpui::Pixels>,
    viewport: gpui::Size<gpui::Pixels>,
    cx: &mut Context<Root>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let current = root
        .settings
        .read(cx)
        .get()
        .accent
        .unwrap_or(ui::theme::DEFAULT_ACCENT);

    // The picker is 240 wide inside 12 of padding; the height is a little over
    // 230 with the Done button. Rounded up so the clamp errs towards on-screen.
    let (width, height) = (px(264.), px(240.));
    let left = (at.x - width + px(16.))
        .min(viewport.width - width - px(8.))
        .max(px(8.));
    let top = (at.y + px(16.))
        .min(viewport.height - height - px(8.))
        .max(px(8.));

    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            // Clicking anywhere else closes it, like a menu.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.accent_picker = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(width)
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(12.))
                    .rounded(theme.radius)
                    .bg(theme.overlay)
                    .border_1()
                    .border_color(theme.border_strong)
                    .shadow_lg()
                    .occlude()
                    .child(ui::ColorPicker::new(current).on_change(cx.listener(
                        |this, value: &u32, _window, cx| {
                            let value = *value;
                            this.settings.update(cx, |store, cx| {
                                store.update(|s| s.accent = Some(value), cx)
                            });
                            this.apply_theme(cx);
                        },
                    )))
                    .child(
                        div().flex().justify_end().child(
                            Button::new("accent-done")
                                .label("Done")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.accent_picker = None;
                                    cx.notify();
                                })),
                        ),
                    ),
            ),
    )
    .with_priority(2)
    .into_any_element()
}

/// The accent picker: a row of swatches, the one in force wearing a ring, and a
/// last round button that opens the colour picker for any other colour.
fn accent_row(accent: Option<u32>, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    // An unset accent is the default one, so the default swatch reads as chosen
    // rather than nothing reading as chosen at all.
    let current = accent.unwrap_or(ui::theme::DEFAULT_ACCENT);
    // A colour that is none of the swatches is the custom one, and the button
    // takes its colour and the ring so the choice still reads as made.
    let custom = !ACCENTS.iter().any(|(_, value)| *value == current);
    let (ring, border) = (theme.foreground, theme.border);
    let fill = match custom {
        true => gpui::linear_gradient(
            0.,
            gpui::linear_color_stop(gpui::rgb(current), 0.),
            gpui::linear_color_stop(gpui::rgb(current), 1.),
        ),
        false => gpui::linear_gradient(
            45.,
            gpui::linear_color_stop(gpui::rgb(0xff6b6b), 0.),
            gpui::linear_color_stop(gpui::rgb(0x6b6bff), 1.),
        ),
    };

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(theme.metrics.gap)
        .h(theme.metrics.field)
        .child(SharedString::from("Theme colour"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(swatch_row(accent, cx))
                .child(
                    div()
                        .id("accent-custom")
                        .flex_none()
                        .size(px(22.))
                        .rounded_full()
                        .bg(fill)
                        .border_2()
                        .border_color(match custom {
                            true => ring,
                            false => border,
                        })
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, _window, cx| {
                                this.accent_picker = match this.accent_picker {
                                    Some(_) => None,
                                    None => Some(event.position),
                                };
                                cx.notify();
                            }),
                        ),
                ),
        )
}

fn swatch_row(accent: Option<u32>, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    // An unset accent is the default one, so the default swatch reads as chosen
    // rather than nothing reading as chosen at all.
    let current = accent.unwrap_or(ui::theme::DEFAULT_ACCENT);
    let (ring, border) = (theme.foreground, theme.border);

    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .children(ACCENTS.iter().map(|(name, value)| {
            let value = *value;
            let selected = value == current;
            div()
                .id(SharedString::from(format!("accent-{name}")))
                .flex()
                .flex_none()
                .size(px(22.))
                .rounded_full()
                .bg(gpui::rgb(value))
                .border_2()
                .border_color(match selected {
                    true => ring,
                    false => border,
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.settings
                        .update(cx, |store, cx| store.update(|s| s.accent = Some(value), cx));
                    this.apply_theme(cx);
                }))
        }))
}

fn choice_row(
    label: &'static str,
    options: &'static [&'static str],
    selected: usize,
    cx: &mut Context<Root>,
    on_pick: impl Fn(&mut Root, usize, &mut Context<Root>) + Clone + 'static,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(theme.metrics.gap)
        .h(theme.metrics.field)
        .child(SharedString::from(label))
        .child(
            div()
                .flex()
                .gap(px(4.))
                .children(options.iter().enumerate().map(|(index, option)| {
                    let on_pick = on_pick.clone();
                    Button::new(SharedString::from(format!("{label}-{option}")))
                        .small()
                        .label(*option)
                        .selected(index == selected)
                        .on_click(cx.listener(move |this, _, _window, cx| on_pick(this, index, cx)))
                })),
        )
}

fn toggle_row(
    label: &'static str,
    on: bool,
    cx: &mut Context<Root>,
    change: impl Fn(&mut Root, bool, &mut Context<Root>) + 'static,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(theme.metrics.field)
        .child(SharedString::from(label))
        .child(
            Button::new(SharedString::from(format!("toggle-{label}")))
                .small()
                .label(match on {
                    true => "On",
                    false => "Off",
                })
                .selected(on)
                .on_click(cx.listener(move |this, _, _window, cx| change(this, !on, cx))),
        )
}

fn slider_row(
    id: &'static str,
    label: String,
    value: f32,
    cx: &mut Context<Root>,
    change: impl Fn(&mut Root, f32, &mut Context<Root>) + 'static,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .h(theme.metrics.field)
        .child(div().w(px(220.)).flex_none().child(label))
        .child(
            Slider::new(id, value.clamp(0.0, 1.0)).on_change(
                cx.listener(move |this, value: &f32, _window, cx| change(this, *value, cx)),
            ),
        )
}

/// Theme choices, and their labels, in one place so the two cannot disagree.
const LOOKS: [Look; 3] = [Look::Dark, Look::Light, Look::System];
const LOOK_LABELS: &[&str] = &["Dark", "Light", "System"];

fn look_index(look: Look) -> usize {
    LOOKS.iter().position(|value| *value == look).unwrap_or(0)
}

// -- equalizer ------------------------------------------------------------

pub fn equalizer_screen(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let inset = theme.metrics.inset;
    // The curve being heard, not the one in the settings file: with per-song
    // audio on those are different, and the sliders have to show what is
    // actually in force.
    let settings = root.player.read(cx).equalizer().clone();
    let enabled = settings.enabled;
    let per_track = root.player.read(cx).per_track_target().is_some();

    let span = audio::dsp::eq::MAX_GAIN_DB - audio::dsp::eq::MIN_GAIN_DB;
    let to_slider = move |db: f32| (db - audio::dsp::eq::MIN_GAIN_DB) / span;
    let from_slider = move |value: f32| audio::dsp::eq::MIN_GAIN_DB + value * span;

    let scroll = root.area_scroll("equalizer");
    let bar = Scrollbar::area("equalizer-scrollbar", &scroll);
    let pane =
        div()
            .id("equalizer")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(theme.metrics.gap)
            .p(inset)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(heading("Equalizer".to_owned(), cx))
                    .child(
                        Button::new("eq-enabled")
                            .label(match enabled {
                                true => "On",
                                false => "Off",
                            })
                            .selected(enabled)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.apply_equalizer(|eq| eq.enabled = !enabled, cx);
                            })),
                    ),
            )
            .child(eyebrow("Presets", cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(4.))
                    .children(audio::Preset::ALL.iter().map(|preset| {
                        let preset = *preset;
                        Button::new(SharedString::from(format!("preset-{}", preset.name())))
                            .small()
                            .label(preset.name())
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.apply_equalizer(
                                    move |eq| *eq = audio::EqSettings::from_preset(preset),
                                    cx,
                                );
                            }))
                    })),
            )
            .child(eyebrow("Bands", cx))
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_center()
                    .gap(px(18.))
                    .h(px(220.))
                    .children(audio::dsp::eq::BANDS.iter().enumerate().map(
                        |(index, frequency)| {
                            let gain = settings.gains[index];
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(4.))
                                .h_full()
                                // Fixed width: the gain label goes from "+3" to
                                // "+12" as it is dragged, and a column that
                                // sizes to its text would shove its neighbours
                                // sideways every time it crossed ten.
                                .w(px(40.))
                                .flex_none()
                                .child(faint(format!("{gain:+.0}"), cx))
                                .child(
                                    div().flex_1().child(
                                        Slider::new(
                                            SharedString::from(format!("band-{index}")),
                                            to_slider(gain),
                                        )
                                        .vertical()
                                        .on_change(
                                            cx.listener(move |this, value: &f32, _window, cx| {
                                                let db = from_slider(*value);
                                                this.apply_equalizer(
                                                    move |eq| eq.gains[index] = db,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                                )
                                .child(faint(format_hz(*frequency), cx))
                        },
                    )),
            )
            .child(slider_row(
                "preamp",
                format!("Preamp ({:+.1} dB)", settings.preamp),
                to_slider(settings.preamp),
                cx,
                move |this, value, cx| {
                    let db = from_slider(value);
                    this.apply_equalizer(move |eq| eq.preamp = db, cx);
                },
            ))
            .child(
                Button::new("eq-flat")
                    .label("Reset to flat")
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.apply_equalizer(
                            |eq| *eq = audio::EqSettings::from_preset(audio::Preset::Flat),
                            cx,
                        );
                    })),
            )
            // Only while the curve being edited is a song's rather than everyone's,
            // so the screen does not explain a mode nobody turned on.
            .when(per_track, |this| {
                this.child(faint(
                    "Per-song audio is on, so these changes are remembered for the \
                 track that is playing rather than for everything."
                        .to_owned(),
                    cx,
                ))
            });

    scrolled(pane, bar).into_any_element()
}

fn format_hz(frequency: f32) -> String {
    match frequency >= 1000.0 {
        true => format!("{:.0}k", frequency / 1000.0),
        false => format!("{frequency:.0}"),
    }
}

/// Which settings the metadata editor writes back, given the text in each field.
///
/// What an empty field means depends on how many tracks are being edited, and
/// that is the whole reason this takes a flag. Editing one track, the fields
/// arrive filled with what the file already says, so emptying one is the user
/// saying "take this out" — `clear_blanks`. Editing several, the form starts
/// blank because the tracks disagree, and an empty field has to mean "leave
/// alone" or saving one field would wipe every other.
pub fn tag_edit_from(values: &[(&str, String)], clear_blanks: bool) -> library::TagEdit {
    let get = |key: &str| {
        let value = values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.trim().to_owned())?;
        match value.is_empty() && !clear_blanks {
            true => None,
            false => Some(value),
        }
    };

    library::TagEdit {
        title: get("title"),
        artist: get("artist"),
        album: get("album"),
        album_artist: get("album_artist"),
        genre: get("genre"),
        composer: get("composer"),
        comment: get("comment"),
        year: get("year").map(|value| value.parse().ok()),
        track_number: get("track").map(|value| value.parse().ok()),
        disc_number: get("disc").map(|value| value.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_accent_offered_is_the_one_an_unset_accent_resolves_to() {
        // The picker shows no separate "reset" control, so the default has to
        // be one of the swatches or there is no way back to it.
        assert_eq!(ACCENTS[0].1, ui::theme::DEFAULT_ACCENT);
        assert_eq!(ACCENTS[0].1, 0x6a5cd6);
    }

    #[test]
    fn every_accent_offered_is_distinct() {
        // Two swatches of the same colour would both ring as selected.
        for (index, (_, value)) in ACCENTS.iter().enumerate() {
            assert!(
                !ACCENTS[index + 1..].iter().any(|(_, other)| other == value),
                "{value:#08x} appears twice"
            );
        }
    }

    #[test]
    fn an_empty_form_changes_nothing_across_a_batch() {
        let edit = tag_edit_from(
            &[
                ("title", String::new()),
                ("artist", "   ".to_owned()),
                ("year", String::new()),
            ],
            false,
        );
        assert!(edit.is_empty(), "blank fields must not be written");
    }

    #[test]
    fn a_batch_writes_only_the_fields_that_were_filled() {
        let edit = tag_edit_from(
            &[
                ("title", String::new()),
                ("artist", "Pink Floyd".to_owned()),
                ("year", "1973".to_owned()),
            ],
            false,
        );
        assert_eq!(edit.title, None);
        assert_eq!(edit.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(edit.year, Some(Some(1973)));
    }

    #[test]
    fn emptying_a_field_on_one_track_empties_the_tag() {
        // The form was filled in from the file, so a field the user cleared is
        // an instruction, not an omission.
        let edit = tag_edit_from(
            &[
                ("title", "Time".to_owned()),
                ("comment", String::new()),
                ("year", "  ".to_owned()),
            ],
            true,
        );
        assert_eq!(edit.title.as_deref(), Some("Time"));
        assert_eq!(edit.comment.as_deref(), Some(""), "the comment is removed");
        assert_eq!(edit.year, Some(None), "the year is removed");
        assert!(!edit.is_empty());
    }

    #[test]
    fn a_year_that_is_not_a_number_clears_rather_than_corrupts() {
        let edit = tag_edit_from(&[("year", "nineteen seventy three".to_owned())], false);
        // Some(None) means "the user asked for this field to be emptied", which
        // is safer than writing a nonsense year.
        assert_eq!(edit.year, Some(None));
    }

    #[test]
    fn frequencies_read_the_way_a_person_would_write_them() {
        assert_eq!(format_hz(60.0), "60");
        assert_eq!(format_hz(1000.0), "1k");
        assert_eq!(format_hz(16000.0), "16k");
    }

    #[test]
    fn every_dialog_knows_its_fields() {
        assert_eq!(Dialog::NewPlaylist.fields().len(), 1);
        assert!(Dialog::ConfirmDelete(vec![1]).fields().is_empty());
        assert!(Dialog::EditMetadata(vec![1]).fields().len() >= 8);
        // Media info is read-only: it has no fields to create, and a Close
        // button rather than a Save.
        assert!(Dialog::MediaInfo(1).fields().is_empty());
    }

    #[test]
    fn timestamps_read_as_dates() {
        assert_eq!(stamp(0), "", "no timestamp shows no row");
        assert_eq!(stamp(1_000), "1970-01-01 00:00");
        // A leap day, which is where a hand-rolled calendar goes wrong first.
        assert_eq!(stamp(1_709_208_000_000), "2024-02-29 12:00");
        assert_eq!(stamp(1_757_548_800_000), "2025-09-11 00:00");
    }

    #[test]
    fn file_sizes_read_in_the_unit_a_person_would_use() {
        assert_eq!(file_size(0), "");
        assert_eq!(file_size(900), "900 bytes");
        assert_eq!(file_size(64 * 1024), "64 KB");
        assert_eq!(file_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }

    #[test]
    fn every_icon_the_dialogs_use_exists() {
        for name in [
            "trash",
            "music",
            "equalizer",
            "settings",
            "queue",
            "album",
            "shuffle",
            "refresh",
            "plus",
            "info",
        ] {
            assert!(icons::source(name).is_some(), "{name} is missing");
        }
    }
}
