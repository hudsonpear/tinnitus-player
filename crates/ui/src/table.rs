//! A virtualized table.
//!
//! Rows are rendered only for the visible range, so a hundred-thousand-track
//! library costs the same to scroll as a hundred. The caller supplies a row
//! renderer rather than a vector of rows, which is what keeps the whole library
//! out of memory.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ElementId, Pixels, SharedString, UniformListScrollHandle, Window, div, px,
    uniform_list,
};

use crate::metrics::Text;
use crate::scrollbar::Scrollbar;
use crate::theme::ActiveTheme as _;

/// How wide a column is. Fixed columns keep their size; flexible ones share what
/// is left, so a window resize moves the title column and not the duration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    Fixed(Pixels),
    Flex(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub key: SharedString,
    pub label: SharedString,
    pub width: Width,
    /// Right-aligned. Durations and counts read better that way.
    pub numeric: bool,
    pub sortable: bool,
}

impl Column {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>, width: Width) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            width,
            numeric: false,
            sortable: true,
        }
    }

    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    /// A decorative column — artwork, a favourite toggle — that nothing sorts by.
    pub fn fixed(mut self) -> Self {
        self.sortable = false;
        self
    }
}

/// Applies a column's width and alignment. Used for both the header cell and
/// every body cell, so the two cannot drift apart.
pub fn sized<T: Styled>(element: T, column: &Column) -> T {
    let element = match column.width {
        Width::Fixed(width) => element.w(width).flex_none(),
        // The share is the grow factor over a zero basis, so columns divide the
        // space in proportion to each other. Sizing by a relative width instead
        // lets an empty cell collapse to nothing, which pulls every column after
        // it out of line with its header.
        Width::Flex(share) => element.w(px(0.)).flex_grow(share).flex_shrink(1.),
    };
    match column.numeric {
        true => element.justify_end().text_right(),
        false => element,
    }
}

type RenderRow = Box<dyn Fn(usize, &mut Window, &mut App) -> AnyElement + 'static>;
/// A `SharedString` rather than a `&str` so `cx.listener` can supply it: GPUI
/// listeners take their event by reference, and `str` is unsized.
type SortBy = std::rc::Rc<dyn Fn(&SharedString, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Table {
    id: ElementId,
    columns: Vec<Column>,
    rows: usize,
    /// Which column is sorted, and whether it is descending.
    sorted: Option<(SharedString, bool)>,
    render_row: RenderRow,
    on_sort: Option<SortBy>,
    show_header: bool,
    /// Owned by the view, not by the table: a table is rebuilt every frame, and
    /// a handle rebuilt with it would forget where the user had scrolled to.
    scroll: Option<UniformListScrollHandle>,
}

impl Table {
    /// `render_row` is called only for rows the user can actually see.
    pub fn new(
        id: impl Into<ElementId>,
        rows: usize,
        render_row: impl Fn(usize, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            columns: vec![],
            rows,
            sorted: None,
            render_row: Box::new(render_row),
            on_sort: None,
            show_header: true,
            scroll: None,
        }
    }

    /// Tracks the view's scroll handle, which is what puts a scrollbar on the
    /// list. Without one the table still scrolls, it just cannot draw the bar.
    pub fn track_scroll(mut self, handle: &UniformListScrollHandle) -> Self {
        self.scroll = Some(handle.clone());
        self
    }

    pub fn column(mut self, column: Column) -> Self {
        self.columns.push(column);
        self
    }

    pub fn columns(mut self, columns: impl IntoIterator<Item = Column>) -> Self {
        self.columns.extend(columns);
        self
    }

    pub fn sorted(mut self, key: impl Into<SharedString>, descending: bool) -> Self {
        self.sorted = Some((key.into(), descending));
        self
    }

    pub fn on_sort(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_sort = Some(std::rc::Rc::new(handler));
        self
    }

    pub fn headerless(mut self) -> Self {
        self.show_header = false;
        self
    }
}

impl RenderOnce for Table {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let header_height = theme.metrics.header;
        let inset = theme.metrics.inset;
        let gap = theme.metrics.gap;
        let border = theme.border;
        let faint = theme.faint_foreground;
        let foreground = theme.foreground;
        let hover = theme.hover;
        let small = theme.text(Text::Small);

        let sorted = self.sorted;
        let on_sort = self.on_sort;
        let render_row = self.render_row;
        let has_header = self.show_header && !self.columns.is_empty();
        let scroll = self.scroll;
        let bar_id = SharedString::from(format!("{:?}-scrollbar", self.id));

        let header = div()
            .flex()
            .items_center()
            .gap(gap)
            .px(inset)
            .h(header_height)
            .flex_none()
            .border_b_1()
            .border_color(border)
            .text_size(small)
            .text_color(faint)
            .children(self.columns.iter().map(|column| {
                let active = sorted.as_ref().is_some_and(|(key, _)| *key == column.key);
                let descending = sorted.as_ref().is_some_and(|(_, descending)| *descending);
                let key = column.key.clone();
                let handler = on_sort.clone();
                let sortable = column.sortable && handler.is_some();

                sized(
                    div()
                        .id(SharedString::from(format!("column-{}", column.key)))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .min_w_0()
                        .h_full()
                        .when(active, |this| this.text_color(foreground))
                        .when(sortable, |this| {
                            this.cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .on_click(move |_, window, cx| {
                                    if let Some(handler) = handler.as_ref() {
                                        handler(&key, window, cx);
                                    }
                                })
                        })
                        // Not `flex_1`: the label sizes to its text so the sort
                        // arrow sits beside it rather than at the far edge of
                        // the column.
                        .child(div().min_w_0().truncate().child(column.label.clone()))
                        .when(active, |this| {
                            this.child(
                                gpui::svg()
                                    .path(icons::path(match descending {
                                        true => "chevron-down",
                                        false => "chevron-up",
                                    }))
                                    .size(px(12.))
                                    .flex_none()
                                    .text_color(foreground),
                            )
                        }),
                    column,
                )
            }));

        // The row height comes from the rows themselves; uniform_list measures
        // the first one and reuses that for the scroll maths.
        let list = uniform_list(self.id, self.rows, move |range, window, cx| {
            range
                .map(|index| render_row(index, window, cx))
                .collect::<Vec<_>>()
        })
        .flex_1()
        .size_full();
        let list = match &scroll {
            Some(handle) => list.track_scroll(handle),
            None => list,
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when(has_header, |this| this.child(header))
            .child(
                // The scrollbar overlays the rows rather than sitting beside
                // them, so switching it on never reflows the columns.
                div()
                    .group(crate::scrollbar::REGION)
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .when_some(scroll, |this, handle| {
                        this.child(Scrollbar::list(bar_id, &handle))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_carry_their_alignment_and_sortability() {
        let title = Column::new("title", "Title", Width::Flex(0.4));
        assert!(title.sortable && !title.numeric);

        let duration = Column::new("duration", "Duration", Width::Fixed(px(72.))).numeric();
        assert!(duration.numeric);

        let art = Column::new("art", "", Width::Fixed(px(40.))).fixed();
        assert!(!art.sortable, "a decorative column must not offer a sort");
    }

    #[test]
    fn the_sort_arrow_icons_exist() {
        assert!(icons::source("chevron-up").is_some());
        assert!(icons::source("chevron-down").is_some());
    }
}
