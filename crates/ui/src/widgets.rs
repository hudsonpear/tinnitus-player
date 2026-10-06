//! The small pieces: buttons, icons, labels, separators, tabs, empty states.

use gpui::prelude::*;
use gpui::{
    AnyView, App, ClickEvent, Context, Div, ElementId, Hsla, Interactivity, MouseButton,
    SharedString, Stateful, StyleRefinement, Window, div, px, svg,
};

use crate::metrics::Text;
use crate::theme::ActiveTheme as _;

type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type SelectTab = std::rc::Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// A bordered button on a surface. The default.
    Secondary,
    /// No chrome until hovered. Toolbars and rows.
    Ghost,
    /// The accent. One per screen at most.
    Primary,
    /// Destructive.
    Danger,
}

#[derive(IntoElement)]
pub struct Button {
    base: Stateful<Div>,
    label: Option<SharedString>,
    icon: Option<SharedString>,
    variant: Variant,
    small: bool,
    /// Overrides the glyph size the height would imply. For the handful of
    /// buttons that are all glyph and have to be seen from across the window.
    icon_size: Option<gpui::Pixels>,
    disabled: bool,
    selected: bool,
    tint: Option<Hsla>,
    /// What the button does, in words. An icon-only button says nothing to
    /// somebody who does not already recognise the glyph.
    tooltip: Option<SharedString>,
    on_click: Option<Click>,
}

impl Button {
    #[track_caller]
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id),
            label: None,
            icon: None,
            variant: Variant::Secondary,
            small: false,
            icon_size: None,
            disabled: false,
            selected: false,
            tint: None,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// An icon name from the `icons` crate, e.g. `"play"`.
    pub fn icon(mut self, name: impl AsRef<str>) -> Self {
        self.icon = Some(icons::path(name));
        self
    }

    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    pub fn danger(mut self) -> Self {
        self.variant = Variant::Danger;
        self
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// A bigger glyph than the button's height would normally give it.
    pub fn icon_size(mut self, size: gpui::Pixels) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn tint(mut self, tint: Hsla) -> Self {
        self.tint = Some(tint);
        self
    }

    /// What this button does, shown on hover. Every icon-only button wants one.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

/// A label that appears beside whatever the pointer is resting on.
///
/// GPUI wants a whole view rather than a string, so this is the one that says a
/// line of text. It is deliberately plain: a tooltip that draws attention to
/// itself is worse than none.
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    /// The builder `InteractiveElement::tooltip` expects.
    pub fn text(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
        let text: SharedString = text.into();
        move |_window, cx| {
            let text = text.clone();
            cx.new(|_| Self { text }).into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(theme.radius)
            .bg(theme.overlay)
            .border_1()
            .border_color(theme.border_strong)
            .shadow_md()
            .text_size(theme.text(Text::Small))
            .text_color(theme.foreground)
            .child(self.text.clone())
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Button {}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            mut base,
            label,
            icon,
            variant,
            small,
            icon_size,
            disabled,
            selected,
            tint,
            tooltip,
            on_click,
        } = self;

        let theme = cx.theme();
        // A selected Secondary button — a filter chip, a settings choice — used
        // to look exactly like an unselected one: it carries a background of its
        // own, and only the Ghost variant had a selected treatment. It now takes
        // the accent for its border and text, which says "this is the one"
        // without turning a whole row into Primary buttons.
        let (background, base_foreground, border) = match (variant, selected) {
            (Variant::Secondary, false) => {
                (Some(theme.raised), theme.foreground, Some(theme.border))
            }
            (Variant::Secondary, true) => (Some(theme.selected), theme.accent, Some(theme.accent)),
            (Variant::Ghost, _) => (None, theme.muted_foreground, None),
            (Variant::Primary, _) => (Some(theme.accent), theme.accent_foreground, None),
            (Variant::Danger, _) => (Some(theme.danger), theme.danger_foreground, None),
        };

        let foreground = match (disabled, tint) {
            (true, _) => theme.faint_foreground,
            (false, Some(tint)) => tint,
            (false, None) => match selected && variant == Variant::Ghost {
                true => theme.foreground,
                false => base_foreground,
            },
        };
        let hover = match disabled {
            true => None,
            false => Some(match variant {
                Variant::Primary => theme.accent_hover,
                Variant::Ghost => theme.hover,
                _ => theme.active,
            }),
        };

        let (height, padding, gap) = match small {
            true => (
                theme.metrics.control_small,
                theme.metrics.pad * 0.75,
                px(4.),
            ),
            false => (theme.metrics.control, theme.metrics.pad * 1.25, px(6.)),
        };
        let icon_size = icon_size.unwrap_or(match small {
            true => px(14.),
            false => px(16.),
        });
        let radius = theme.radius;
        let selected_background = theme.active;
        let text_size = theme.text(match small {
            true => Text::Small,
            false => Text::Body,
        });
        let interactive = !disabled;
        let overrides = std::mem::take(base.style());

        let mut button = base
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(gap)
            .h(height)
            .px(padding)
            .rounded(radius)
            .text_size(text_size)
            .text_color(foreground)
            .when_some(background, |this, background| this.bg(background))
            .when(selected && background.is_none(), |this| {
                this.bg(selected_background)
            })
            .when_some(border, |this, border| this.border_1().border_color(border))
            .when(interactive, |this| this.cursor_pointer())
            // On a disabled button too: "why can I not press this" is exactly
            // when the label is worth reading.
            .when_some(tooltip, |this, text| this.tooltip(Tooltip::text(text)))
            .when_some(hover, |this, hover| {
                this.hover(move |style| style.bg(hover))
            })
            .when_some(icon, |this, path| {
                this.child(
                    svg()
                        .path(path)
                        .size(icon_size)
                        .flex_none()
                        .text_color(foreground),
                )
            })
            .when_some(label, |this, label| {
                this.child(div().min_w_0().truncate().child(label))
            })
            .when(interactive, |this| {
                this.when_some(on_click, |this, handler| {
                    // Stop the press from reaching a row underneath the button.
                    this.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |event, window, cx| handler(event, window, cx))
                })
            });

        button.style().refine(&overrides);
        button
    }
}

/// A bare icon, for places that are not buttons.
#[derive(IntoElement)]
pub struct Icon {
    name: SharedString,
    size: gpui::Pixels,
    color: Option<Hsla>,
}

impl Icon {
    pub fn new(name: impl AsRef<str>) -> Self {
        Self {
            name: icons::path(name),
            size: px(16.),
            color: None,
        }
    }

    pub fn size(mut self, size: gpui::Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.theme().muted_foreground);
        svg()
            .path(self.name)
            .size(self.size)
            .flex_none()
            .text_color(color)
    }
}

/// A one-pixel rule in the theme's border colour.
#[derive(IntoElement)]
pub struct Separator {
    vertical: bool,
}

impl Separator {
    pub fn horizontal() -> Self {
        Self { vertical: false }
    }

    pub fn vertical() -> Self {
        Self { vertical: true }
    }
}

impl RenderOnce for Separator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = cx.theme().border;
        match self.vertical {
            true => div().w(px(1.)).h_full().flex_none().bg(color),
            false => div().h(px(1.)).w_full().flex_none().bg(color),
        }
    }
}

/// A section heading in the sidebar and the settings screen.
pub fn eyebrow(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .text_size(theme.text(Text::Tiny))
        .text_color(theme.faint_foreground)
        .child(SharedString::from(text.into().to_uppercase()))
}

/// Secondary text: artist under a title, counts, hints.
pub fn faint(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .text_size(theme.text(Text::Small))
        .text_color(theme.muted_foreground)
        .truncate()
        .child(text.into())
}

/// A screen heading.
pub fn heading(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .text_size(theme.text(Text::Title))
        .text_color(theme.foreground)
        .child(text.into())
}

/// What a list shows when it has nothing in it. An empty library should explain
/// itself rather than look broken.
#[derive(IntoElement)]
pub struct Vacancy {
    icon: SharedString,
    title: SharedString,
    detail: Option<SharedString>,
    action: Option<gpui::AnyElement>,
}

impl Vacancy {
    pub fn new(icon: impl AsRef<str>, title: impl Into<SharedString>) -> Self {
        Self {
            icon: icons::path(icon),
            title: title.into(),
            detail: None,
            action: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The one thing to do about the emptiness. Rendered inside the centred
    /// column, so it sits under the message rather than being pushed to the
    /// bottom of the screen by this element's own `flex_1`.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for Vacancy {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(theme.metrics.pad)
            .p(theme.metrics.inset)
            .child(
                svg()
                    .path(self.icon)
                    .size(px(36.))
                    .text_color(theme.faint_foreground),
            )
            .child(
                div()
                    .text_size(theme.text(Text::Large))
                    .text_color(theme.muted_foreground)
                    .child(self.title),
            )
            .when_some(self.detail, |this, detail| {
                this.child(
                    div()
                        .max_w(px(420.))
                        .text_center()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.faint_foreground)
                        .child(detail),
                )
            })
            .when_some(self.action, |this, action| {
                this.child(div().mt(theme.metrics.pad).child(action))
            })
    }
}

/// A row of tabs, as used by the playlist bar.
#[derive(IntoElement)]
pub struct Tabs {
    id: ElementId,
    tabs: Vec<(ElementId, SharedString)>,
    selected: usize,
    on_select: Option<SelectTab>,
    trailing: Option<gpui::AnyElement>,
}

impl Tabs {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            tabs: vec![],
            selected: 0,
            on_select: None,
            trailing: None,
        }
    }

    pub fn tab(mut self, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        self.tabs.push((id.into(), label.into()));
        self
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(std::rc::Rc::new(handler));
        self
    }

    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }
}

impl RenderOnce for Tabs {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self.selected;
        let on_select = self.on_select;
        let (foreground, muted, active, hover, radius, pad, control, small) = (
            theme.foreground,
            theme.muted_foreground,
            theme.active,
            theme.hover,
            theme.radius,
            theme.metrics.pad,
            theme.metrics.control,
            theme.text(Text::Small),
        );

        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(px(2.))
            .h(control)
            .children(
                self.tabs
                    .into_iter()
                    .enumerate()
                    .map(move |(index, (id, label))| {
                        let is_active = index == selected;
                        let handler = on_select.clone();
                        div()
                            .id(id)
                            .flex()
                            .flex_none()
                            .items_center()
                            .h_full()
                            .px(pad)
                            .rounded(radius)
                            .cursor_pointer()
                            .text_size(small)
                            .text_color(match is_active {
                                true => foreground,
                                false => muted,
                            })
                            .when(is_active, |this| this.bg(active))
                            .hover(move |style| style.bg(hover))
                            .child(label)
                            .on_click(move |_, window, cx| {
                                if let Some(handler) = handler.as_ref() {
                                    handler(index, window, cx);
                                }
                            })
                    }),
            )
            .when_some(self.trailing, |this, trailing| this.child(trailing))
    }
}

#[cfg(test)]
mod tests {
    use crate::theme::{Theme, ThemeOverrides};

    #[test]
    fn every_icon_a_widget_names_exists() {
        // A typo in an icon name would otherwise be a silently blank square.
        for name in [
            "play",
            "pause",
            "next",
            "previous",
            "shuffle",
            "repeat",
            "repeat-one",
            "volume",
            "volume-mute",
            "search",
            "settings",
            "close",
            "more",
        ] {
            assert!(icons::source(name).is_some(), "{name} is missing");
        }
    }

    #[test]
    fn a_default_theme_has_readable_contrast() {
        let theme = Theme::dark(&ThemeOverrides::default());
        assert!((theme.foreground.l - theme.background.l).abs() > 0.5);
        assert!(theme.muted_foreground.l > theme.background.l);
    }
}
