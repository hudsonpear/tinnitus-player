//! Context menus.
//!
//! ponytail: no nested submenus. "Add to playlist" becomes a labelled group
//! inside the same menu, which is one popup instead of two and behaves better
//! near a screen edge. Add real submenus only if a menu grows past a screenful.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, ElementId, MouseButton, Pixels, Point, SharedString, Window, anchored, deferred, div, px,
};

use crate::metrics::Text;
use crate::theme::ActiveTheme as _;

type Select = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

pub enum MenuEntry {
    Item {
        id: ElementId,
        label: SharedString,
        icon: Option<SharedString>,
        /// Shown right-aligned, e.g. `Ctrl+O`.
        shortcut: Option<SharedString>,
        /// A tick down the right-hand edge: this row names something that is
        /// already the case.
        checked: bool,
        danger: bool,
        disabled: bool,
        on_select: Select,
    },
    /// A dividing line.
    Divider,
    /// A heading over the entries that follow, in place of a submenu.
    Group(SharedString),
}

impl MenuEntry {
    pub fn item(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        on_select: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self::Item {
            id: id.into(),
            label: label.into(),
            icon: None,
            shortcut: None,
            checked: false,
            danger: false,
            disabled: false,
            on_select: Rc::new(on_select),
        }
    }

    pub fn icon(mut self, name: impl AsRef<str>) -> Self {
        if let Self::Item { icon, .. } = &mut self {
            *icon = Some(icons::path(name));
        }
        self
    }

    pub fn shortcut(mut self, keys: impl Into<SharedString>) -> Self {
        if let Self::Item { shortcut, .. } = &mut self {
            *shortcut = Some(keys.into());
        }
        self
    }

    pub fn checked(mut self, value: bool) -> Self {
        if let Self::Item { checked, .. } = &mut self {
            *checked = value;
        }
        self
    }

    pub fn danger(mut self) -> Self {
        if let Self::Item { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }

    pub fn disabled(mut self, value: bool) -> Self {
        if let Self::Item { disabled, .. } = &mut self {
            *disabled = value;
        }
        self
    }
}

/// A menu floating at a point, drawn above everything else.
#[derive(IntoElement)]
pub struct Menu {
    id: ElementId,
    at: Point<Pixels>,
    entries: Vec<MenuEntry>,
    width: Pixels,
    on_dismiss: Option<Select>,
}

impl Menu {
    pub fn new(id: impl Into<ElementId>, at: Point<Pixels>) -> Self {
        Self {
            id: id.into(),
            at,
            entries: vec![],
            width: px(220.),
            on_dismiss: None,
        }
    }

    pub fn entry(mut self, entry: MenuEntry) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn entries(mut self, entries: impl IntoIterator<Item = MenuEntry>) -> Self {
        self.entries.extend(entries);
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Called after any item is chosen, and when the backdrop is clicked.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Menu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let overlay = theme.overlay;
        let border = theme.border_strong;
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let faint = theme.faint_foreground;
        let danger_color = theme.danger;
        let accent = theme.accent;
        let hover = theme.hover;
        let radius = theme.radius;
        let row = theme.metrics.row;
        let pad = theme.metrics.pad;
        let gap = theme.metrics.gap;
        let body = theme.text(Text::Body);
        let tiny = theme.text(Text::Tiny);

        let dismiss = self.on_dismiss.clone();
        let backdrop_dismiss = self.on_dismiss;
        let width = self.width;

        let items = self.entries.into_iter().map(move |entry| match entry {
            MenuEntry::Divider => div()
                .my(px(4.))
                .mx(pad)
                .h(px(1.))
                .bg(border)
                .into_any_element(),
            MenuEntry::Group(label) => div()
                .px(pad)
                .pt(px(6.))
                .pb(px(2.))
                .text_size(tiny)
                .text_color(faint)
                .child(SharedString::from(label.to_uppercase()))
                .into_any_element(),
            MenuEntry::Item {
                id,
                label,
                icon,
                shortcut,
                checked,
                danger,
                disabled,
                on_select,
            } => {
                let color = match (disabled, danger) {
                    (true, _) => faint,
                    (false, true) => danger_color,
                    (false, false) => foreground,
                };
                let dismiss = dismiss.clone();
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(gap)
                    .h(row)
                    .px(pad)
                    .mx(px(4.))
                    .rounded(radius)
                    .text_size(body)
                    .text_color(color)
                    .when(!disabled, |this| {
                        this.cursor_pointer().hover(move |style| style.bg(hover))
                    })
                    .when_some(icon, |this, path| {
                        this.child(
                            gpui::svg()
                                .path(path)
                                .size(px(15.))
                                .flex_none()
                                .text_color(color),
                        )
                    })
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    // On the right, where it does not push the labels of the
                    // rows around it out of line.
                    .when(checked, |this| {
                        this.child(
                            gpui::svg()
                                .path(icons::path("check"))
                                .size(px(15.))
                                .flex_none()
                                .text_color(accent),
                        )
                    })
                    .when_some(shortcut, |this, keys| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(tiny)
                                .text_color(faint)
                                .child(keys),
                        )
                    })
                    .when(!disabled, |this| {
                        this.on_click(move |_, window, cx| {
                            on_select(window, cx);
                            if let Some(dismiss) = dismiss.as_ref() {
                                dismiss(window, cx);
                            }
                        })
                    })
                    .into_any_element()
            }
        });

        // Deferred so the menu paints over everything, and anchored so it flips
        // rather than running off the bottom of the window.
        deferred(
            div()
                .id(self.id)
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                // A click anywhere else closes the menu.
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    if let Some(dismiss) = backdrop_dismiss.as_ref() {
                        dismiss(window, cx);
                    }
                    cx.stop_propagation();
                })
                .child(
                    anchored().position(self.at).child(
                        div()
                            .w(width)
                            .py(px(4.))
                            .rounded(radius)
                            .bg(overlay)
                            .border_1()
                            .border_color(border)
                            .shadow_lg()
                            .text_color(muted)
                            .occlude()
                            .children(items),
                    ),
                ),
        )
        .with_priority(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_only_touch_items() {
        // A shortcut on a divider is a no-op rather than a panic.
        assert!(matches!(
            MenuEntry::Divider.shortcut("Ctrl+X"),
            MenuEntry::Divider
        ));
        assert!(matches!(
            MenuEntry::Group("Add to playlist".into()).danger(),
            MenuEntry::Group(_)
        ));
    }

    #[test]
    fn an_item_keeps_what_it_was_given() {
        let entry = MenuEntry::item("play", "Play", |_, _| {})
            .icon("play")
            .shortcut("Enter")
            .disabled(true);
        match entry {
            MenuEntry::Item {
                label,
                icon,
                shortcut,
                disabled,
                danger,
                ..
            } => {
                assert_eq!(label, "Play");
                assert_eq!(icon.as_deref(), Some("icon/play"));
                assert_eq!(shortcut.as_deref(), Some("Enter"));
                assert!(disabled);
                assert!(!danger);
            }
            _ => panic!("expected an item"),
        }
    }

    #[test]
    fn every_icon_the_track_menu_uses_exists() {
        for name in [
            "play", "queue", "playlist", "folder", "external", "edit", "heart", "star", "trash",
        ] {
            assert!(icons::source(name).is_some(), "{name} is missing");
        }
    }
}
