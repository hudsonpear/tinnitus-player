//! A single-line text field.
//!
//! ponytail: no IME composition, no multi-line, no undo. This covers search,
//! renaming a playlist and the metadata editor, which is every text entry the
//! app has. If a field ever needs CJK composition, replace this with a full
//! GPUI input element rather than growing this one.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Task, TextRun,
    Window, div, px,
};

use crate::metrics::Text;
use crate::theme::ActiveTheme as _;

/// Half a blink. The Windows default is 530ms either way; this is close enough
/// that the caret does not read as a different speed to every other field on the
/// desktop.
const BLINK: Duration = Duration::from_millis(530);

#[derive(Debug, Clone, PartialEq)]
pub enum FieldEvent {
    /// The text changed. Search listens to this to filter as you type.
    Changed(String),
    /// Enter.
    Submit(String),
    /// Escape.
    Cancel,
}

pub struct Field {
    focus: FocusHandle,
    text: String,
    /// Byte offset of the caret — the end of the selection that moves. Always on
    /// a character boundary.
    caret: usize,
    /// The end of the selection that stays put. Equal to `caret` when nothing is
    /// selected, which is how "no selection" is represented rather than an
    /// `Option` every caller would have to unwrap.
    anchor: usize,
    /// A drag is in progress, so mouse movement extends the selection.
    dragging: bool,
    /// Which half of the blink the caret is in.
    blink_on: bool,
    /// The blink timer, alive only while the field has focus. Dropping it is how
    /// the blink stops.
    blink: Option<Task<()>>,
    placeholder: SharedString,
    icon: Option<SharedString>,
    borderless: bool,
    /// Where the text was last painted, so a click can be turned into a caret
    /// position. A `Cell` rather than ordinary state because it is filled during
    /// prepaint, and writing entity state there would repaint forever.
    text_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl EventEmitter<FieldEvent> for Field {}

impl Field {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: String::new(),
            caret: 0,
            anchor: 0,
            dragging: false,
            blink_on: true,
            blink: None,
            placeholder: SharedString::default(),
            icon: None,
            borderless: false,
            text_bounds: Rc::default(),
        }
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn icon(mut self, name: impl AsRef<str>) -> Self {
        self.icon = Some(icons::path(name));
        self
    }

    pub fn borderless(mut self) -> Self {
        self.borderless = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the contents without emitting `Changed`: used when the caller is
    /// the one that decided on the new value.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.move_caret(self.text.len(), false);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.text.is_empty() {
            return;
        }
        self.text.clear();
        self.move_caret(0, false);
        cx.emit(FieldEvent::Changed(String::new()));
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    // -- blinking ---------------------------------------------------------

    /// Shows the caret solid and restarts the blink from the top.
    ///
    /// Called on every keystroke and every click: a caret that blinks out from
    /// under the character you are typing is harder to follow than one that does
    /// not blink at all. Dropping the timer is enough — the next render starts a
    /// fresh one, so the visible half always begins now.
    fn wake_caret(&mut self) {
        self.blink_on = true;
        self.blink = None;
    }

    fn start_blinking(&mut self, cx: &mut Context<Self>) {
        self.blink_on = true;
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK).await;
                // The field is gone, and so is any reason to keep a timer alive.
                if this
                    .update(cx, |this, cx| {
                        this.blink_on = !this.blink_on;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    // -- selection --------------------------------------------------------

    /// The selected range, low end first. Empty when the two ends agree.
    fn selection(&self) -> (usize, usize) {
        selection_range(self.anchor, self.caret)
    }

    fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    fn selected_text(&self) -> &str {
        let (start, end) = self.selection();
        &self.text[start..end]
    }

    /// Moves the caret, dragging the anchor with it unless the selection is
    /// being extended.
    fn move_caret(&mut self, to: usize, extend: bool) {
        self.caret = to;
        if !extend {
            self.anchor = to;
        }
        self.wake_caret();
    }

    /// Removes the selected text. Returns whether there was any — the caller
    /// decides whether that alone is a change worth emitting.
    fn take_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (start, end) = self.selection();
        self.text.replace_range(start..end, "");
        self.move_caret(start, false);
        true
    }

    // -- editing ----------------------------------------------------------

    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.take_selection();
        self.text.insert_str(self.caret, text);
        self.move_caret(self.caret + text.len(), false);
        cx.emit(FieldEvent::Changed(self.text.clone()));
        cx.notify();
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        if !self.take_selection() {
            let Some(previous) = previous_boundary(&self.text, self.caret) else {
                return;
            };
            self.text.replace_range(previous..self.caret, "");
            self.move_caret(previous, false);
        }
        cx.emit(FieldEvent::Changed(self.text.clone()));
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if !self.take_selection() {
            let Some(next) = next_boundary(&self.text, self.caret) else {
                return;
            };
            self.text.replace_range(self.caret..next, "");
        }
        cx.emit(FieldEvent::Changed(self.text.clone()));
        cx.notify();
    }

    // -- pointer ----------------------------------------------------------

    /// The offset under `x`, or `None` before the text has been painted.
    ///
    /// The text is shaped again to ask which character sits there: glyph widths
    /// are the font's business, and guessing from a character count puts the
    /// caret in the wrong place in every proportional font. Past the halfway
    /// point of a character counts as after it, which is what makes clicking
    /// between two letters land where it looks like it should.
    fn offset_at(&self, x: Pixels, window: &mut Window) -> Option<usize> {
        if self.text.is_empty() {
            return Some(0);
        }
        let bounds = self.text_bounds.get()?;

        let style = window.text_style();
        let run = TextRun {
            len: self.text.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            self.text.clone().into(),
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );

        let local = x - bounds.origin.x;
        let index = line.index_for_x(local).unwrap_or(self.text.len());
        let next = next_boundary(&self.text, index).unwrap_or(index);
        let (left, right) = (line.x_for_index(index), line.x_for_index(next));
        Some(match next != index && local > left + (right - left) * 0.5 {
            true => next,
            false => index,
        })
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if let Some(offset) = self.offset_at(event.position.x, window) {
            // Shift-click extends what is already selected rather than starting
            // over, the same as shift-arrow.
            self.move_caret(offset, event.modifiers.shift);
        }
        self.dragging = true;
        cx.notify();
    }

    /// ponytail: only fires while the pointer is over the field, so a drag that
    /// leaves it stops extending rather than running to the end. The text is
    /// clipped to the field anyway. Grab a window-level mouse handler if
    /// selecting by overshooting the edge ever matters.
    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dragging {
            return;
        }
        // The button can be released outside the field, where no mouse-up
        // reaches us. Noticing it is gone here is what stops the selection
        // following the pointer around afterwards.
        if event.pressed_button != Some(MouseButton::Left) {
            self.dragging = false;
            return;
        }
        if let Some(offset) = self.offset_at(event.position.x, window) {
            self.move_caret(offset, true);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.dragging = false;
    }

    // -- keyboard ---------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modified = keystroke.modifiers.control || keystroke.modifiers.platform;
        let extend = keystroke.modifiers.shift;
        // Even the keys that change nothing: the caret should be solid while
        // somebody is working the keyboard.
        self.wake_caret();

        // Typing must not also trigger the window's shortcuts: pressing space in
        // the search box types a space, it does not pause playback.
        let typing = keystroke.key_char.is_some() && !modified;
        if typing || matches!(keystroke.key.as_str(), "backspace" | "delete" | "space") {
            cx.stop_propagation();
        }

        match keystroke.key.as_str() {
            "backspace" => self.backspace(cx),
            "delete" => self.delete(cx),
            "left" => {
                // With a selection and no shift, the arrow collapses to its edge
                // rather than stepping a character further, which is what every
                // other text field does.
                let to = match (self.has_selection(), extend) {
                    (true, false) => self.selection().0,
                    _ => previous_boundary(&self.text, self.caret).unwrap_or(self.caret),
                };
                self.move_caret(to, extend);
                cx.notify();
            }
            "right" => {
                let to = match (self.has_selection(), extend) {
                    (true, false) => self.selection().1,
                    _ => next_boundary(&self.text, self.caret).unwrap_or(self.caret),
                };
                self.move_caret(to, extend);
                cx.notify();
            }
            "home" => {
                self.move_caret(0, extend);
                cx.notify();
            }
            "end" => {
                self.move_caret(self.text.len(), extend);
                cx.notify();
            }
            "enter" => cx.emit(FieldEvent::Submit(self.text.clone())),
            "escape" => cx.emit(FieldEvent::Cancel),
            "a" if modified => {
                self.anchor = 0;
                self.caret = self.text.len();
                cx.notify();
            }
            "c" if modified => {
                if self.has_selection() {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.selected_text().to_owned(),
                    ));
                }
            }
            "x" if modified => {
                if self.has_selection() {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.selected_text().to_owned(),
                    ));
                    self.take_selection();
                    cx.emit(FieldEvent::Changed(self.text.clone()));
                    cx.notify();
                }
            }
            "v" if modified => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    // A pasted newline would become an invisible character in a
                    // single-line field.
                    let flattened = text.replace(['\n', '\r'], " ");
                    self.insert(&flattened, cx);
                }
            }
            _ => {
                if let Some(character) = keystroke.key_char.as_ref().filter(|_| !modified) {
                    // Control characters are not text.
                    let printable: String = character.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
                        self.insert(&printable, cx);
                    }
                }
            }
        }
    }
}

impl Focusable for Field {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Field {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        // Focus has no observer here — `Field::new` has no window to register
        // one with — so the timer is started and dropped from render, which runs
        // on every focus change anyway. The blink's own notify lands back here
        // with the task already in place, so it does not restart itself.
        match (focused, self.blink.is_some()) {
            (true, false) => self.start_blinking(cx),
            (false, true) => self.wake_caret(),
            _ => {}
        }
        let caret_on = focused && self.blink_on;

        let theme = cx.theme();
        let showing_placeholder = self.text.is_empty();
        let text_color = match showing_placeholder {
            true => theme.faint_foreground,
            false => theme.foreground,
        };
        let border = match focused {
            true => theme.accent,
            false => theme.border,
        };

        // The caret and the highlight sit between spans rather than being
        // positioned by measurement: splitting the string is what puts them
        // mid-text without this field having to lay out glyphs itself.
        let (start, end) = self.selection();
        let spans = [
            SharedString::from(self.text[..start].to_owned()),
            SharedString::from(self.text[start..end].to_owned()),
            SharedString::from(self.text[end..].to_owned()),
        ];
        let caret_at_start = self.caret == start;

        let hover = theme.hover;
        let (raised, muted, accent, selection, radius, field, pad, gap, font_size, body) = (
            theme.raised,
            theme.muted_foreground,
            theme.accent,
            theme.selected,
            theme.radius,
            theme.metrics.field,
            theme.metrics.pad,
            theme.metrics.gap,
            theme.font_size,
            theme.text(Text::Body),
        );

        div()
            .id("field")
            .track_focus(&self.focus)
            .key_context("Field")
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .items_center()
            .gap(gap)
            .h(field)
            .px(pad)
            .rounded(radius)
            .bg(raised)
            .when(!self.borderless, |this| {
                this.border_1().border_color(border)
            })
            .text_size(body)
            .text_color(text_color)
            .cursor_text()
            // The search box sits inside the title bar's drag region. Without
            // this the drag hitbox is still under the pointer, the platform
            // answers HTCAPTION, and clicking into the field moves the window
            // instead of placing the caret.
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when_some(self.icon.clone(), |this, path| {
                this.child(
                    gpui::svg()
                        .path(path)
                        .size(px(15.))
                        .flex_none()
                        .text_color(muted),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    // Where the text landed, so the next click can be turned
                    // into an offset. The first child starts at the text
                    // origin, which is the only part needed.
                    .on_children_prepainted({
                        let bounds = self.text_bounds.clone();
                        move |children, _window, _cx| bounds.set(children.first().copied())
                    })
                    .map(|this| match showing_placeholder {
                        true => this
                            .when(focused, |this| {
                                this.child(caret(font_size, accent, caret_on))
                            })
                            .child(div().flex_none().child(self.placeholder.clone())),
                        false => {
                            let [before, selected, after] = spans;
                            this.child(div().flex_none().child(before))
                                .when(focused && caret_at_start, |this| {
                                    this.child(caret(font_size, accent, caret_on))
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .when(start != end, |this| {
                                            this.bg(selection).rounded(px(2.))
                                        })
                                        .child(selected),
                                )
                                .when(focused && !caret_at_start, |this| {
                                    this.child(caret(font_size, accent, caret_on))
                                })
                                .child(div().flex_none().child(after))
                        }
                    }),
            )
            // Only once there is something to clear: an X over an empty field is
            // a control that does nothing.
            .when(!showing_placeholder, |this| {
                this.child(
                    div()
                        .id("clear-field")
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(18.))
                        .rounded_full()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        // The colour goes on the svg itself: it paints with its
                        // own text style rather than inheriting the parent's,
                        // and without this the glyph comes out invisible.
                        .child(
                            gpui::svg()
                                .path(icons::path("close"))
                                .size(px(13.))
                                .flex_none()
                                .text_color(muted),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear(cx);
                            // Clearing is the start of typing something else,
                            // not the end of using the field.
                            window.focus(&this.focus, cx);
                        })),
                )
            })
    }
}

/// The caret itself. Zero-width in flow terms would be invisible, so it takes
/// the pixel and a half every text cursor takes.
///
/// Blinking turns it transparent rather than removing it: it is a flex child
/// between two spans of text, so taking it out shunts the text sideways twice a
/// second.
fn caret(height: Pixels, color: gpui::Hsla, on: bool) -> impl IntoElement {
    div().w(px(1.5)).h(height).flex_none().bg(match on {
        true => color,
        false => gpui::transparent_black(),
    })
}

/// The selected range, low end first.
///
/// A drag right to left leaves the anchor after the caret, and every caller
/// wants the pair in order.
fn selection_range(anchor: usize, caret: usize) -> (usize, usize) {
    match anchor <= caret {
        true => (anchor, caret),
        false => (caret, anchor),
    }
}

/// The byte offset of the character before `caret`, or `None` at the start.
fn previous_boundary(text: &str, caret: usize) -> Option<usize> {
    if caret == 0 || caret > text.len() {
        return None;
    }
    text[..caret].char_indices().next_back().map(|(at, _)| at)
}

/// The byte offset after the character at `caret`, or `None` at the end.
fn next_boundary(text: &str, caret: usize) -> Option<usize> {
    if caret >= text.len() {
        return None;
    }
    text[caret..]
        .chars()
        .next()
        .map(|character| caret + character.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_reads_the_same_dragged_either_way() {
        // Dragging right to left puts the anchor after the caret, and every
        // caller wants the range in order.
        assert_eq!(selection_range(2, 7), (2, 7));
        assert_eq!(selection_range(7, 2), (2, 7));

        let text = "hello world";
        let (start, end) = selection_range(7, 2);
        assert_eq!(&text[start..end], "llo w");
    }

    #[test]
    fn no_selection_when_the_ends_agree() {
        let (start, end) = selection_range(3, 3);
        assert_eq!(start, end);
        assert_eq!(&"hello"[start..end], "");
    }

    #[test]
    fn a_selection_over_multibyte_text_cuts_on_boundaries() {
        // "naïve" — the ï is two bytes, so the ends are byte offsets rather
        // than character counts. Walking with the boundary helpers is what
        // keeps them off the middle of a character.
        let mut text = "naïve".to_owned();
        let mut end = 0;
        for _ in 0..3 {
            end = next_boundary(&text, end).unwrap();
        }
        assert_eq!(end, 4, "three characters of naïve are four bytes");

        let (start, end) = selection_range(end, 0);
        assert_eq!(&text[start..end], "naï");
        text.replace_range(start..end, "");
        assert_eq!(text, "ve");
    }

    #[test]
    fn boundaries_step_over_whole_characters() {
        // Multi-byte text must not be split mid-character, which would panic on
        // the next slice.
        let text = "naïve 日本";
        let mut caret = text.len();
        let mut steps = 0;
        while let Some(previous) = previous_boundary(text, caret) {
            caret = previous;
            steps += 1;
            assert!(text.is_char_boundary(caret));
        }
        assert_eq!(caret, 0);
        assert_eq!(steps, text.chars().count());

        while let Some(next) = next_boundary(text, caret) {
            caret = next;
            assert!(text.is_char_boundary(caret));
        }
        assert_eq!(caret, text.len());
    }

    #[test]
    fn boundaries_stop_at_both_ends() {
        assert_eq!(previous_boundary("abc", 0), None);
        assert_eq!(next_boundary("abc", 3), None);
        assert_eq!(next_boundary("", 0), None);
        assert_eq!(previous_boundary("", 0), None);
    }

    #[test]
    fn a_caret_past_the_end_does_not_index_out_of_bounds() {
        assert_eq!(next_boundary("abc", 99), None);
        assert_eq!(previous_boundary("abc", 99), None);
    }
}
