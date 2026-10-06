//! A vertical scrollbar for the app's scrolling regions.
//!
//! GPUI's lists scroll with the wheel but draw nothing, so a long list gives no
//! sign of how long it is or where in it you are. This draws that, and lets the
//! user drag it.
//!
//! It sits absolutely inside whatever it scrolls, so it costs no layout space
//! and the content underneath keeps its full width. Both kinds of scrolling
//! region are covered: a `uniform_list`, whose content height is virtual, and a
//! plain `overflow_y_scroll` div, whose content height is real.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, ElementId, MouseButton, MouseDownEvent, Pixels, Render, ScrollHandle,
    SharedString, UniformListScrollHandle, Window, canvas, div, point, px,
};

use crate::theme::ActiveTheme as _;

/// The gutter. Wider than the thumb so there is something to hit.
const GUTTER: Pixels = px(11.);
const THUMB: Pixels = px(6.);
/// Names the scrolling region a scrollbar belongs to, so the thumb can appear
/// when the pointer is anywhere over it and stay out of the way otherwise.
///
/// Anything that places a `Scrollbar` by hand rather than through `scrolled`
/// has to put this group on the element they share, or the thumb has nothing to
/// notice the pointer and never shows at all.
pub const REGION: &str = "scroll-region";
/// A thumb shorter than this is impossible to grab, however long the list is.
const MIN_THUMB: Pixels = px(24.);

/// Dragging in GPUI needs a value to drag; this one carries nothing but the
/// scrollbar's identity, and draws nothing.
#[derive(Clone)]
struct Grab(#[allow(dead_code)] ElementId);

impl Render for Grab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// A drag in progress: whether the thumb is held, and where along it the pointer
/// took hold so it does not jump out from under them.
///
/// This lives in the window's element state, not in the element. A `RenderOnce`
/// is built again from nothing every frame, so a flag the press set would be
/// gone by the next move event — the drag would read a fresh `false` and give
/// up, and the thumb would sit still however far you dragged it.
#[derive(Default)]
struct Hold {
    held: bool,
    /// Pixels from the top of the thumb to the pointer.
    offset: f32,
}

/// The scrolling region a scrollbar is attached to.
#[derive(Clone)]
enum Target {
    /// A virtualized list. Its content height is the row height times the row
    /// count, which the list records at layout.
    List(UniformListScrollHandle),
    /// A plain scrolling div.
    Area(ScrollHandle),
}

/// How far a region scrolls, in pixels. `offset` is measured downward from the
/// top, unlike GPUI's, which counts up from zero as a negative number.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Extent {
    viewport: f32,
    content: f32,
    offset: f32,
}

impl Extent {
    /// How far the region can scroll. Zero when everything already fits.
    fn range(self) -> f32 {
        (self.content - self.viewport).max(0.0)
    }

    fn scrollable(self) -> bool {
        // A sub-pixel overhang is rounding, not something to draw a bar for.
        self.range() > 1.0
    }
}

impl Target {
    fn extent(&self) -> Extent {
        match self {
            Self::List(handle) => {
                let state = handle.0.borrow();
                let Some(size) = state.last_item_size else {
                    return Extent::default();
                };
                Extent {
                    viewport: size.item.height / px(1.),
                    content: size.contents.height / px(1.),
                    offset: -(state.base_handle.offset().y / px(1.)),
                }
            }
            Self::Area(handle) => {
                let viewport = handle.bounds().size.height / px(1.);
                Extent {
                    viewport,
                    // A scrolling div only knows how far it may scroll, which is
                    // the same thing said differently.
                    content: viewport + (handle.max_offset().y / px(1.)).max(0.0),
                    offset: -(handle.offset().y / px(1.)),
                }
            }
        }
    }

    /// Scrolls to `offset` pixels from the top, keeping any horizontal offset.
    fn scroll_to(&self, offset: f32) {
        let y = px(-offset);
        match self {
            Self::List(handle) => {
                let state = handle.0.borrow();
                let x = state.base_handle.offset().x;
                state.base_handle.set_offset(point(x, y));
            }
            Self::Area(handle) => handle.set_offset(point(handle.offset().x, y)),
        }
    }
}

#[derive(IntoElement)]
pub struct Scrollbar {
    id: ElementId,
    target: Target,
}

impl Scrollbar {
    /// For a `uniform_list` tracking `handle`.
    pub fn list(id: impl Into<ElementId>, handle: &UniformListScrollHandle) -> Self {
        Self {
            id: id.into(),
            target: Target::List(handle.clone()),
        }
    }

    /// For a scrolling div tracking `handle`.
    pub fn area(id: impl Into<ElementId>, handle: &ScrollHandle) -> Self {
        Self {
            id: id.into(),
            target: Target::Area(handle.clone()),
        }
    }
}

impl RenderOnce for Scrollbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let extent = self.target.extent();
        // Nothing to scroll is nothing to draw: a bar that fills its own gutter
        // is just a stripe down the side of every short list.
        if !extent.scrollable() {
            return div().into_any_element();
        }

        let theme = cx.theme();
        let thumb_color = theme.border_strong;
        let hover_color = theme.muted_foreground;

        let (thumb_height, top) = thumb(extent, extent.viewport);

        // Filled in by the canvas below and read by the mouse handlers on the
        // next event, exactly as the slider does it.
        let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
        // Held across frames, so the press and the moves that follow it are the
        // same gesture.
        let hold = window.use_keyed_state(
            SharedString::from(format!("{:?}-hold", self.id)),
            cx,
            |_, _| Hold::default(),
        );
        // A drag suppresses hover styling for as long as it lasts, so a thumb
        // that appears on hover alone vanishes the moment it is grabbed —
        // exactly when the user is watching it. Being held counts as reason
        // enough to be visible.
        let held = hold.read(cx).held;

        let target = self.target.clone();
        let down = {
            let (target, bounds, hold) = (target.clone(), bounds.clone(), hold.clone());
            move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                let extent = target.extent();
                let track = bounds.get();
                let (thumb_height, top) = thumb(extent, track.size.height / px(1.));
                let at = (event.position.y - track.origin.y) / px(1.);

                let on_thumb = at >= top && at <= top + thumb_height;
                hold.update(cx, |hold, _| {
                    hold.held = true;
                    hold.offset = match on_thumb {
                        // On the thumb: take hold of it where it was clicked.
                        true => at - top,
                        // On the gutter: the thumb jumps, centred on the click.
                        false => thumb_height / 2.0,
                    };
                });
                if !on_thumb {
                    target.scroll_to(offset_for(extent, track, at - thumb_height / 2.0));
                    window.refresh();
                }
                // The rows underneath must not also take this click as a
                // selection.
                cx.stop_propagation();
            }
        };

        let dragged = {
            let (target, bounds, hold) = (target.clone(), bounds.clone(), hold.clone());
            move |event: &gpui::DragMoveEvent<Grab>, window: &mut Window, cx: &mut App| {
                let hold = hold.read(cx);
                if !hold.held {
                    return;
                }
                let track = bounds.get();
                let at = (event.event.position.y - track.origin.y) / px(1.) - hold.offset;
                target.scroll_to(offset_for(target.extent(), track, at));
                window.refresh();
            }
        };

        let released = move |_: &gpui::MouseUpEvent, _window: &mut Window, cx: &mut App| {
            hold.update(cx, |hold, _| hold.held = false);
        };

        let id = self.id;
        let capture = {
            let bounds = bounds.clone();
            canvas(move |measured, _, _| bounds.set(measured), |_, _, _, _| {})
                .absolute()
                .size_full()
        };

        div()
            .id(id.clone())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(GUTTER)
            .flex()
            .justify_center()
            .cursor_default()
            .on_mouse_down(MouseButton::Left, down)
            .on_drag(Grab(id), |grab, _, _, cx| cx.new(|_| grab.clone()))
            .on_drag_move(dragged)
            .on_mouse_up(MouseButton::Left, released.clone())
            .on_mouse_up_out(MouseButton::Left, released)
            .child(capture)
            .child(
                div()
                    .id("scrollbar-thumb")
                    .absolute()
                    .top(px(top))
                    .w(THUMB)
                    .h(px(thumb_height))
                    .rounded_full()
                    .bg(thumb_color)
                    // Invisible until the pointer is over the list it belongs
                    // to. A thumb parked beside a list nobody is using is a
                    // stripe down the window, and the queue has one on every
                    // panel at once.
                    .opacity(match held {
                        true => 1.,
                        false => 0.,
                    })
                    .group_hover(REGION, |style| style.opacity(1.))
                    .hover(move |style| style.bg(hover_color)),
            )
            .into_any_element()
    }
}

/// The thumb's height and its distance from the top of a `track`-pixel gutter.
fn thumb(extent: Extent, track: f32) -> (f32, f32) {
    if track <= 0.0 || extent.content <= 0.0 {
        return (0.0, 0.0);
    }
    // The thumb is as much of the gutter as the viewport is of the content —
    // showing that ratio is the scrollbar's whole job.
    let height = (track * (extent.viewport / extent.content)).clamp(MIN_THUMB / px(1.), track);
    let travel = track - height;
    let range = extent.range();
    let top = match range > 0.0 {
        true => travel * (extent.offset / range).clamp(0.0, 1.0),
        false => 0.0,
    };
    (height, top)
}

/// The scroll offset that puts the top of the thumb `top` pixels down the
/// gutter. The inverse of `thumb`.
fn offset_for(extent: Extent, track: Bounds<Pixels>, top: f32) -> f32 {
    let track = track.size.height / px(1.);
    let (height, _) = thumb(extent, track);
    let travel = track - height;
    if travel <= 0.0 {
        return 0.0;
    }
    (top / travel).clamp(0.0, 1.0) * extent.range()
}

/// Puts a scrollbar over a scrolling element rather than beside it, in a column
/// that fills its parent. The content keeps its full width; only the last few
/// pixels are shared.
pub fn scrolled(child: impl IntoElement, scrollbar: Scrollbar) -> gpui::Div {
    div()
        .group(REGION)
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(child)
        .child(scrollbar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point as gpui_point, size};

    fn track(height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: gpui_point(px(500.), px(100.)),
            size: size(GUTTER, px(height)),
        }
    }

    fn long() -> Extent {
        Extent {
            viewport: 400.0,
            content: 1600.0,
            offset: 0.0,
        }
    }

    #[test]
    fn a_list_that_fits_gets_no_bar() {
        let fits = Extent {
            content: 400.0,
            ..long()
        };
        assert!(!fits.scrollable());
        // Half a pixel of overhang is rounding, not a scrollable list.
        assert!(
            !Extent {
                content: 400.5,
                ..fits
            }
            .scrollable()
        );
        assert!(long().scrollable());
    }

    #[test]
    fn the_thumb_is_as_much_of_the_gutter_as_the_screen_is_of_the_list() {
        let (height, top) = thumb(long(), 400.0);
        assert_eq!(height, 100.0);
        assert_eq!(top, 0.0);
    }

    #[test]
    fn the_thumb_reaches_the_bottom_at_the_bottom() {
        let extent = Extent {
            offset: 1200.0,
            ..long()
        };
        let (height, top) = thumb(extent, 400.0);
        assert_eq!(height + top, 400.0, "the thumb stopped short of the end");
    }

    #[test]
    fn an_enormous_list_still_has_a_grabbable_thumb() {
        let extent = Extent {
            content: 4_000_000.0,
            ..long()
        };
        let (height, _) = thumb(extent, 400.0);
        assert_eq!(height, MIN_THUMB / px(1.));
    }

    #[test]
    fn dragging_maps_back_to_the_offset_it_came_from() {
        let extent = Extent {
            offset: 300.0,
            ..long()
        };
        let (_, top) = thumb(extent, 400.0);
        let back = offset_for(extent, track(400.0), top);
        assert!((back - extent.offset).abs() < 0.01, "{back} is not 300");
    }

    #[test]
    fn dragging_past_either_end_clamps() {
        assert_eq!(offset_for(long(), track(400.0), -9999.0), 0.0);
        assert_eq!(offset_for(long(), track(400.0), 9999.0), long().range());
    }

    #[test]
    fn a_region_that_has_never_been_laid_out_does_not_divide_by_zero() {
        let extent = Extent::default();
        assert_eq!(thumb(extent, 0.0), (0.0, 0.0));
        assert_eq!(offset_for(extent, track(0.0), 50.0), 0.0);
    }
}
