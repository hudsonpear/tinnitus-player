//! A colour picker: a saturation/brightness square over a hue strip.
//!
//! Both are the same interaction as the slider — press to jump, drag to follow —
//! so the mouse handling follows `slider.rs`, down to keeping the "is it held"
//! flag in window state rather than in the element, which is rebuilt every frame.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Hsla, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, Render,
    SharedString, Window, canvas, div, linear_color_stop, linear_gradient, px, relative, rgb,
};

/// Hue, saturation and value, each `0.0..=1.0`.
///
/// Kept apart from the RGB the app stores because RGB forgets the hue of a grey
/// or a black: drag into the bottom of the square and the hue strip would snap to
/// red if the picker only had the colour to go on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub fn from_rgb(color: u32) -> Self {
        let channel = |shift: u32| ((color >> shift) & 0xff) as f32 / 255.0;
        let (r, g, b) = (channel(16), channel(8), channel(0));
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let h = match delta == 0.0 {
            true => 0.0,
            false if max == r => ((g - b) / delta).rem_euclid(6.0),
            false if max == g => (b - r) / delta + 2.0,
            false => (r - g) / delta + 4.0,
        } / 6.0;
        let s = match max == 0.0 {
            true => 0.0,
            false => delta / max,
        };
        Self { h, s, v: max }
    }

    pub fn to_rgb(self) -> u32 {
        let h = self.h.clamp(0.0, 1.0) * 6.0;
        let (s, v) = (self.s.clamp(0.0, 1.0), self.v.clamp(0.0, 1.0));
        let sector = h.floor();
        let fraction = h - sector;
        let (p, q, t) = (
            v * (1.0 - s),
            v * (1.0 - s * fraction),
            v * (1.0 - s * (1.0 - fraction)),
        );
        let (r, g, b) = match sector as u32 % 6 {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        let byte = |value: f32| (value * 255.0).round() as u32;
        byte(r) << 16 | byte(g) << 8 | byte(b)
    }
}

/// Dragging in GPUI needs a value to drag; this one carries nothing.
#[derive(Clone)]
struct Grab;

impl Render for Grab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

#[derive(Default)]
struct Held(bool);

/// Called with a position inside the surface as fractions of its width and
/// height, `0.0..=1.0` from the top left.
type Pick = Rc<dyn Fn(f32, f32, &mut Window, &mut App)>;

/// A region that reports where it is pressed and dragged.
fn surface(id: &'static str, window: &mut Window, cx: &mut App, pick: Pick) -> gpui::Stateful<gpui::Div> {
    // Filled in by the canvas below and read by the handlers on the next event,
    // so a surface that has never been laid out cannot be dragged.
    let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let held = window.use_keyed_state(SharedString::from(format!("{id}-held")), cx, |_, _| {
        Held::default()
    });

    let at = {
        let bounds = bounds.clone();
        move |point: Point<Pixels>| {
            let area = bounds.get();
            (
                along(point.x - area.origin.x, area.size.width),
                along(point.y - area.origin.y, area.size.height),
            )
        }
    };

    let down = {
        let (at, pick, held) = (at.clone(), pick.clone(), held.clone());
        move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
            held.update(cx, |held, _| held.0 = true);
            let (x, y) = at(event.position);
            pick(x, y, window, cx);
            cx.stop_propagation();
        }
    };
    let dragged = {
        let (at, pick, held) = (at.clone(), pick.clone(), held.clone());
        move |event: &gpui::DragMoveEvent<Grab>, window: &mut Window, cx: &mut App| {
            if !held.read(cx).0 {
                return;
            }
            let (x, y) = at(event.event.position);
            pick(x, y, window, cx);
        }
    };
    // Also on release outside the surface, so a drag let go past the edge ends.
    let released = {
        let held = held.clone();
        move |_: &MouseUpEvent, _: &mut Window, cx: &mut App| {
            held.update(cx, |held, _| held.0 = false);
        }
    };

    div()
        .id(id)
        .relative()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, down)
        .on_drag(Grab, |grab, _, _, cx| cx.new(|_| grab.clone()))
        .on_drag_move(dragged)
        .on_mouse_up(MouseButton::Left, released.clone())
        .on_mouse_up_out(MouseButton::Left, released)
        .child(
            canvas(move |measured, _, _| bounds.set(measured), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
}

/// How far along a span an offset is, clamped to the span.
fn along(offset: Pixels, span: Pixels) -> f32 {
    let span = span / px(1.);
    match span > 0.0 {
        true => (offset / px(1.) / span).clamp(0.0, 1.0),
        false => 0.0,
    }
}

fn solid(color: u32) -> Hsla {
    rgb(color).into()
}

type Change = Rc<dyn Fn(&u32, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct ColorPicker {
    color: u32,
    on_change: Option<Change>,
}

impl ColorPicker {
    pub fn new(color: u32) -> Self {
        Self {
            color,
            on_change: None,
        }
    }

    /// Fires continuously while dragging, with an `0xRRGGBB` colour.
    pub fn on_change(mut self, handler: impl Fn(&u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

const WIDTH: f32 = 240.0;

impl RenderOnce for ColorPicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state("color-picker-hsv", cx, {
            let color = self.color;
            move |_, _| Hsv::from_rgb(color)
        });
        // The colour can change from somewhere else — a swatch, another window —
        // and then the remembered hue belongs to a colour that is gone.
        let mut hsv = *state.read(cx);
        if hsv.to_rgb() != self.color {
            hsv = Hsv::from_rgb(self.color);
            state.update(cx, |state, _| *state = hsv);
        }

        let report = {
            let state = state.clone();
            let on_change = self.on_change;
            Rc::new(move |next: Hsv, window: &mut Window, cx: &mut App| {
                state.update(cx, |state, _| *state = next);
                if let Some(handler) = on_change.as_ref() {
                    handler(&next.to_rgb(), window, cx);
                }
            })
        };

        let pad_pick: Pick = {
            let (state, report) = (state.clone(), report.clone());
            Rc::new(move |x, y, window, cx| {
                let current = *state.read(cx);
                report(
                    Hsv {
                        s: x,
                        v: 1.0 - y,
                        ..current
                    },
                    window,
                    cx,
                );
            })
        };
        let hue_pick: Pick = {
            let state = state.clone();
            Rc::new(move |x, _, window, cx| {
                let current = *state.read(cx);
                report(Hsv { h: x, ..current }, window, cx);
            })
        };

        let white = Hsla {
            a: 1.0,
            ..gpui::white()
        };
        let black = gpui::black();
        let clear = |color: Hsla| Hsla { a: 0.0, ..color };
        let pure = |h: f32| solid(Hsv { h, s: 1.0, v: 1.0 }.to_rgb());
        let ring = gpui::white();

        let pad = surface("picker-pad", window, cx, pad_pick)
            .w(px(WIDTH))
            .h(px(140.))
            .rounded(px(4.))
            .overflow_hidden()
            .bg(pure(hsv.h))
            // White fading in from the left, then black from the bottom: together
            // they are the saturation and brightness axes.
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(white, 0.),
                linear_color_stop(clear(white), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(clear(black), 0.),
                linear_color_stop(black, 1.),
            )))
            .child(
                div()
                    .absolute()
                    .size(px(14.))
                    .left(relative(hsv.s))
                    .top(relative(1.0 - hsv.v))
                    .ml(Pixels::ZERO - px(7.))
                    .mt(Pixels::ZERO - px(7.))
                    .rounded_full()
                    .border_2()
                    .border_color(ring)
                    .bg(solid(self.color)),
            );

        // Six runs of the rainbow, each a two-colour gradient, because a
        // gradient here has only two stops.
        let strip = surface("picker-hue", window, cx, hue_pick)
            .flex()
            .w(px(WIDTH))
            .h(px(16.))
            .rounded(px(4.))
            .overflow_hidden()
            .children((0..6).map(|step| {
                div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(pure(step as f32 / 6.0), 0.),
                    linear_color_stop(pure((step + 1) as f32 / 6.0), 1.),
                ))
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(px(6.))
                    .left(relative(hsv.h))
                    .ml(Pixels::ZERO - px(3.))
                    .rounded(px(2.))
                    .border_2()
                    .border_color(ring),
            );

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(pad)
            .child(strip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_survive_the_trip_through_hsv() {
        for color in [0x000000, 0xffffff, 0x6a5cd6, 0xff0000, 0x00ff00, 0x0000ff, 0x808080, 0xc65d32] {
            assert_eq!(Hsv::from_rgb(color).to_rgb(), color, "{color:06x}");
        }
    }

    #[test]
    fn the_primary_hues_land_where_the_strip_draws_them() {
        assert_eq!(Hsv { h: 0.0, s: 1.0, v: 1.0 }.to_rgb(), 0xff0000);
        assert_eq!(Hsv { h: 1.0 / 3.0, s: 1.0, v: 1.0 }.to_rgb(), 0x00ff00);
        assert_eq!(Hsv { h: 2.0 / 3.0, s: 1.0, v: 1.0 }.to_rgb(), 0x0000ff);
        // The end of the strip is the start again.
        assert_eq!(Hsv { h: 1.0, s: 1.0, v: 1.0 }.to_rgb(), 0xff0000);
    }

    #[test]
    fn a_press_maps_to_a_fraction_and_clamps_at_the_edges() {
        assert_eq!(along(px(50.), px(200.)), 0.25);
        assert_eq!(along(px(-30.), px(200.)), 0.0);
        assert_eq!(along(px(999.), px(200.)), 1.0);
        // Not laid out yet: no division by an empty span.
        assert_eq!(along(px(10.), px(0.)), 0.0);
    }
}
