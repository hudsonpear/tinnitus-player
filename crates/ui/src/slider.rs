//! A draggable track: the seek bar, the volume control and every equalizer band.
//!
//! One element covers all three because they are the same interaction — press
//! anywhere to jump, drag to scrub, release to commit — and a player that
//! behaves differently in two places is a player that feels wrong.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, ElementId, Hsla, MouseButton, MouseDownEvent, PathBuilder, Pixels, Point,
    Render, SharedString, Window, canvas, div, point, px, relative,
};

use crate::theme::ActiveTheme as _;

/// Dragging in GPUI needs a value to drag; this one carries nothing but the
/// slider's identity, and draws nothing.
#[derive(Clone)]
struct Grab(#[allow(dead_code)] ElementId);

impl Render for Grab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Whether this slider is being held right now.
///
/// It lives in the window's element state rather than in the element, because a
/// `RenderOnce` is built again from nothing on every frame: a flag the press set
/// would belong to a value that no longer exists by the time the release
/// arrives, the release would read a fresh `false` and give up, and the commit
/// would never fire. That is the difference between a seek bar you can click and
/// one that looks dead.
#[derive(Default)]
struct Held(bool);

/// Takes the value by reference so `cx.listener` can produce one directly:
/// GPUI listeners are always `Fn(&mut View, &Event, ..)`.
type Change = Rc<dyn Fn(&f32, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Slider {
    id: ElementId,
    /// 0.0..=1.0.
    value: f32,
    /// A second, dimmer fill drawn behind the value.
    ghost: Option<f32>,
    vertical: bool,
    disabled: bool,
    thickness: Option<Pixels>,
    fill: Option<Hsla>,
    /// Drawn in place of the plain track: one bar per value, lit up to the
    /// handle. Already squeezed to the number of bars wanted.
    waveform: Option<Vec<f32>>,
    /// Fires continuously while dragging, so the UI can follow the thumb.
    on_change: Option<Change>,
    /// Fires once on release, for the expensive action (an actual seek).
    on_commit: Option<Change>,
}

impl Slider {
    pub fn new(id: impl Into<ElementId>, value: f32) -> Self {
        Self {
            id: id.into(),
            value: sane(value),
            ghost: None,
            vertical: false,
            disabled: false,
            thickness: None,
            fill: None,
            waveform: None,
            on_change: None,
            on_commit: None,
        }
    }

    pub fn vertical(mut self) -> Self {
        self.vertical = true;
        self
    }

    pub fn ghost(mut self, value: f32) -> Self {
        self.ghost = Some(sane(value));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn thickness(mut self, thickness: Pixels) -> Self {
        self.thickness = Some(thickness);
        self
    }

    pub fn fill(mut self, color: Hsla) -> Self {
        self.fill = Some(color);
        self
    }

    /// Draws the song's own shape instead of a line. One value per bar, each
    /// `0.0..=1.0`; dragging and clicking work exactly as they do on the line.
    pub fn waveform(mut self, bars: Vec<f32>) -> Self {
        self.waveform = Some(bars);
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    pub fn on_commit(mut self, handler: impl Fn(&f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_commit = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Slider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let track = theme.border;
        let fill_color = self.fill.unwrap_or(theme.accent);
        let ghost_color = theme.border_strong;
        let thumb_color = theme.foreground;
        // Waveform colours, taken here with the rest: the theme cannot be held
        // across the mouse handlers, which need the app context themselves.
        let wave = WaveStyle {
            lit: fill_color,
            ahead: theme.muted_foreground.opacity(0.55),
            head: theme.foreground,
            tub: theme.raised,
            radius: theme.radius,
        };

        let thickness = self.thickness.unwrap_or(px(4.));
        // The hit area is deliberately taller than the visible track: a
        // four-pixel seek bar that only responds on those four pixels is
        // miserable to use.
        let reach = thickness.max(px(16.));
        let thumb = thickness.max(px(10.));

        let value = self.value;
        let vertical = self.vertical;
        let enabled = !self.disabled;

        // Filled in by the canvas below, read by the mouse handlers on the next
        // event. A slider that has never been laid out cannot be dragged, which
        // is exactly right.
        let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));

        let on_change = self.on_change;
        let on_commit = self.on_commit;

        // True between a press on this slider and the release that ends it.
        //
        // `on_mouse_up_out` exists so a drag released past the end of the track
        // still commits — but it fires for *any* mouse-up elsewhere in the
        // window, so without this guard clicking anywhere would silently commit
        // a value taken from wherever the pointer happened to be.
        let grabbed = window.use_keyed_state(
            SharedString::from(format!("{:?}-held", self.id)),
            cx,
            |_, _| Held::default(),
        );

        let position = {
            let bounds = bounds.clone();
            move |point: Point<Pixels>| fraction(point, bounds.get(), vertical)
        };

        let down = {
            let position = position.clone();
            let on_change = on_change.clone();
            let grabbed = grabbed.clone();
            move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                grabbed.update(cx, |held, _| held.0 = true);
                let value = position(event.position);
                if let Some(handler) = on_change.as_ref() {
                    handler(&value, window, cx);
                }
                cx.stop_propagation();
            }
        };

        let dragged = {
            let position = position.clone();
            let grabbed = grabbed.clone();
            move |event: &gpui::DragMoveEvent<Grab>, window: &mut Window, cx: &mut App| {
                if !grabbed.read(cx).0 {
                    return;
                }
                let value = position(event.event.position);
                if let Some(handler) = on_change.as_ref() {
                    handler(&value, window, cx);
                }
            }
        };

        let released = {
            let position = position.clone();
            let grabbed = grabbed.clone();
            move |event: &gpui::MouseUpEvent, window: &mut Window, cx: &mut App| {
                if !grabbed.update(cx, |held, _| std::mem::replace(&mut held.0, false)) {
                    return;
                }
                let value = position(event.position);
                if let Some(handler) = on_commit.as_ref() {
                    handler(&value, window, cx);
                }
            }
        };

        let id = self.id;
        let capture = {
            let bounds = bounds.clone();
            canvas(move |measured, _, _| bounds.set(measured), |_, _, _, _| {})
                .absolute()
                .size_full()
        };

        let filled = move |color: Hsla, amount: f32| {
            div()
                .absolute()
                .rounded_full()
                .bg(color)
                .map(move |this| match vertical {
                    true => this
                        .bottom(px(0.))
                        .left(px(0.))
                        .w_full()
                        .h(relative(amount)),
                    false => this.left(px(0.)).top(px(0.)).h_full().w(relative(amount)),
                })
        };

        // The waveform replaces the line and its thumb: the boundary between lit
        // and unlit bars is the play head, and a knob on top of it only hides
        // the shape the user asked to see.
        let body = match self.waveform {
            Some(peaks) => waves(peaks, value, wave, thickness),
            None => div()
                .relative()
                .rounded_full()
                .bg(track)
                .map(|this| match vertical {
                    true => this.w(thickness).h_full(),
                    false => this.h(thickness).w_full(),
                })
                .when_some(self.ghost, |this, amount| {
                    this.child(filled(ghost_color, amount))
                })
                .child(filled(fill_color, value))
                .when(enabled, |this| {
                    this.child(
                        div()
                            .absolute()
                            .size(thumb)
                            .rounded_full()
                            .bg(thumb_color)
                            .map(|this| match vertical {
                                true => this
                                    .left((thickness - thumb) / 2.)
                                    .bottom(relative(value))
                                    .mb(Pixels::ZERO - thumb / 2.),
                                false => this
                                    .top((thickness - thumb) / 2.)
                                    .left(relative(value))
                                    .ml(Pixels::ZERO - thumb / 2.),
                            }),
                    )
                }),
        };

        div()
            .id(id.clone())
            .relative()
            .flex()
            .map(|this| match vertical {
                true => this.justify_center().h_full().w(reach),
                false => this.items_center().w_full().h(reach),
            })
            .when(enabled, |this| {
                this.cursor_pointer()
                    .on_mouse_down(MouseButton::Left, down)
                    .on_drag(Grab(id.clone()), |grab, _, _, cx| cx.new(|_| grab.clone()))
                    .on_drag_move(dragged)
                    .on_mouse_up(MouseButton::Left, released.clone())
                    .on_mouse_up_out(MouseButton::Left, released)
            })
            .child(capture)
            .child(body)
    }
}

/// The handful of colours a waveform is drawn in.
#[derive(Clone, Copy)]
struct WaveStyle {
    lit: Hsla,
    ahead: Hsla,
    head: Hsla,
    tub: Hsla,
    radius: Pixels,
}

/// The waveform body: one continuous shape, mirrored about the middle line and
/// lit as far as the play head, with a hairline marking the head itself.
///
/// Drawn as a filled path rather than a row of bars — a bar chart reads as a
/// chart, and this is meant to read as the sound. The played part is the same
/// shape painted again on top in the accent colour, which keeps the outline
/// identical on both sides of the head.
fn waves(peaks: Vec<f32>, value: f32, style: WaveStyle, height: Pixels) -> gpui::Div {
    div()
        .relative()
        .w_full()
        .h(height)
        .rounded(style.radius)
        .bg(style.tub)
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    if let Some(path) = envelope(bounds, &peaks, 1.0) {
                        window.paint_path(path, style.ahead);
                    }
                    if let Some(path) = envelope(bounds, &peaks, value) {
                        window.paint_path(path, style.lit);
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        // Where the music actually is, in case the colour boundary falls in a
        // silent stretch with no shape to show it.
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(px(1.))
                .left(relative(value))
                .bg(style.head),
        )
}

/// The mirrored outline of the peaks from the left edge up to `upto`, a
/// fraction of the width, as a fillable path.
///
/// The first peak sits on the left edge and the last on the right, so a
/// fraction of the width is the same fraction of the peaks — that is what keeps
/// the lit shape and the play-head line on the same pixel. The cut itself falls
/// between two peaks more often than not, so the final point is interpolated
/// rather than snapped to whichever peak is nearest.
///
/// Points are joined with straight lines. Curving through them looks smoother
/// and says less: the corners are the transients, and rounding them off is what
/// turns a song into a sausage.
fn envelope(bounds: Bounds<Pixels>, peaks: &[f32], upto: f32) -> Option<gpui::Path<Pixels>> {
    let count = peaks.len();
    if count < 2 {
        return None;
    }
    let last = (count - 1) as f32 * upto.clamp(0.0, 1.0);
    if last <= 0.0 {
        return None;
    }

    let middle = bounds.origin.y + bounds.size.height * 0.5;
    // Plenty of headroom: the loudest moment reaches this far, so everything
    // quieter has somewhere to be short. A wave drawn to the edges has no room
    // left to say anything about the quiet parts.
    let reach = bounds.size.height * 0.3;
    let span = 1.0 / (count - 1) as f32;

    // `at` is a position in peak-index space, whole or not.
    let x = |at: f32| bounds.origin.x + bounds.size.width * (at * span);
    let height = |at: f32| {
        let index = at.floor() as usize;
        let blend = at - index as f32;
        let (here, next) = (peaks[index], peaks[(index + 1).min(count - 1)]);
        // A floor, so silence is a thin line through the middle rather than a
        // gap. No ceiling: the values overshoot 1.0 by design, and `reach`
        // already leaves the room for it.
        reach * (here + (next - here) * blend).max(0.02)
    };
    let top = |at: f32| point(x(at), middle - height(at));
    let bottom = |at: f32| point(x(at), middle + height(at));

    // Every whole peak up to the cut, and then the cut itself.
    let mut stops: Vec<f32> = (0..=last.floor() as usize)
        .map(|index| index as f32)
        .collect();
    if last > *stops.last().unwrap_or(&0.0) {
        stops.push(last);
    }
    if stops.len() < 2 {
        return None;
    }

    let mut builder = PathBuilder::fill();
    builder.move_to(top(stops[0]));
    for stop in stops.iter().skip(1) {
        builder.line_to(top(*stop));
    }
    for stop in stops.iter().rev() {
        builder.line_to(bottom(*stop));
    }
    builder.close();
    builder.build().ok()
}

/// Where a point falls along a slider, as 0.0..=1.0.
fn fraction(point: Point<Pixels>, bounds: Bounds<Pixels>, vertical: bool) -> f32 {
    let (offset, span) = match vertical {
        // Vertical sliders grow upward, so the maths is inverted.
        true => (
            bounds.origin.y + bounds.size.height - point.y,
            bounds.size.height,
        ),
        false => (point.x - bounds.origin.x, bounds.size.width),
    };
    let span = span / px(1.);
    if span <= 0.0 {
        return 0.0;
    }
    sane(offset / px(1.) / span)
}

fn sane(value: f32) -> f32 {
    match value.is_finite() {
        true => value.clamp(0.0, 1.0),
        false => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn horizontal() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(200.), px(16.)),
        }
    }

    #[test]
    fn a_click_maps_to_a_position_along_the_track() {
        let bounds = horizontal();
        assert_eq!(fraction(point(px(100.), px(58.)), bounds, false), 0.0);
        assert_eq!(fraction(point(px(200.), px(58.)), bounds, false), 0.5);
        assert_eq!(fraction(point(px(300.), px(58.)), bounds, false), 1.0);
    }

    #[test]
    fn dragging_past_either_end_clamps_instead_of_overshooting() {
        let bounds = horizontal();
        assert_eq!(fraction(point(px(-500.), px(58.)), bounds, false), 0.0);
        assert_eq!(fraction(point(px(9999.), px(58.)), bounds, false), 1.0);
    }

    #[test]
    fn a_vertical_slider_grows_upward() {
        let bounds = Bounds {
            origin: point(px(10.), px(20.)),
            size: size(px(16.), px(100.)),
        };
        // The bottom of the track is zero and the top is one.
        assert_eq!(fraction(point(px(18.), px(120.)), bounds, true), 0.0);
        assert_eq!(fraction(point(px(18.), px(70.)), bounds, true), 0.5);
        assert_eq!(fraction(point(px(18.), px(20.)), bounds, true), 1.0);
    }

    #[test]
    fn a_slider_that_has_never_been_laid_out_reports_zero() {
        // Before the first frame the bounds are empty; dividing by that span
        // would be a NaN fed straight into a seek.
        let empty = Bounds::default();
        assert_eq!(fraction(point(px(50.), px(50.)), empty, false), 0.0);
        assert_eq!(fraction(point(px(50.), px(50.)), empty, true), 0.0);
    }

    #[test]
    fn nonsense_values_are_refused_at_the_door() {
        assert_eq!(sane(f32::NAN), 0.0);
        assert_eq!(sane(f32::INFINITY), 0.0);
        assert_eq!(sane(-3.0), 0.0);
        assert_eq!(sane(4.0), 1.0);
        assert_eq!(sane(0.37), 0.37);
    }
}
