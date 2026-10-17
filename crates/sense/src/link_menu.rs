//! The menu a right click on a link opens: open the link, or copy it.
//!
//! The text view reports clicks on links, on any button, when the button is
//! released; a right click it would otherwise drop. [`on_link_click`] keeps
//! a left or middle click opening the link and leaves a right click (or a
//! long press) here, in [`PendingLinkMenu`]; the window watches that and
//! shows [`LinkMenu`] where the click was.

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;

/// A link right-clicked, with where, until the window shows its menu.
#[derive(Default)]
pub struct PendingLinkMenu(pub Option<(SharedString, Point<Pixels>)>);

impl Global for PendingLinkMenu {}

/// What a click on a link in rendered markdown does: a left or middle click
/// (or Enter, or a tap) opens it, as the text view does by default; a right
/// click or a long press asks for its menu.
pub fn on_link_click(url: &SharedString, event: &ClickEvent, _: &mut Window, cx: &mut App) {
    match menu_position(event) {
        Some(Some(at)) => cx.set_global(PendingLinkMenu(Some((url.clone(), at)))),
        Some(None) => cx.open_url(url),
        None => {}
    }
}

/// Where a click asks for the link's menu: `Some(Some(at))` for the menu,
/// `Some(None)` to open the link, `None` to do nothing (the back and
/// forward buttons).
fn menu_position(event: &ClickEvent) -> Option<Option<Point<Pixels>>> {
    match event {
        ClickEvent::Mouse(click) => match click.up.button {
            MouseButton::Right => Some(Some(click.up.position)),
            MouseButton::Left | MouseButton::Middle => Some(None),
            _ => None,
        },
        ClickEvent::Keyboard(_) => Some(None),
        ClickEvent::Touch(touch) if touch.long_press => Some(Some(touch.position)),
        ClickEvent::Touch(_) => Some(None),
    }
}

/// An open link menu: the menu, and where it was asked for.
pub struct LinkMenu {
    menu: Entity<PopupMenu>,
    at: Point<Pixels>,
    _dismiss: Subscription,
}

impl LinkMenu {
    /// Builds the menu for `url` and gives it focus, so Escape and the arrow
    /// keys reach it. `on_dismiss` runs when it closes: an item was picked,
    /// Escape was pressed, or the pointer went down outside it.
    pub fn open<V: 'static>(
        url: SharedString,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<V>,
        on_dismiss: impl Fn(&mut V, &mut Context<V>) + 'static,
    ) -> Self {
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let open = url.clone();
            let copy = url.to_string();
            menu.item(PopupMenuItem::new("Open Link").on_click(move |_, _, cx| cx.open_url(&open)))
                .item(PopupMenuItem::new("Copy Link").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                }))
        });
        menu.focus_handle(cx).focus(window, cx);
        let _dismiss = cx.subscribe_in(&menu, window, move |this, _, _: &DismissEvent, _, cx| {
            on_dismiss(this, cx);
            cx.notify();
        });
        Self { menu, at, _dismiss }
    }

    /// The menu over everything else, at the click and kept inside the
    /// window, as the app's other context menus show.
    pub fn render(&self, window: &Window) -> AnyElement {
        deferred(
            anchored().child(
                div()
                    .w(window.bounds().size.width)
                    .h(window.bounds().size.height)
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .child(
                        anchored()
                            .position(self.at)
                            .snap_to_window_with_margin(px(8.))
                            .child(self.menu.clone()),
                    ),
            ),
        )
        .with_priority(gpui_kit::base::POPUP_PRIORITY)
        .into_any_element()
    }

    /// Where the menu was asked for, for automated checks.
    #[cfg(feature = "automation")]
    pub fn position(&self) -> Point<Pixels> {
        self.at
    }
}

#[cfg(test)]
mod tests {
    use super::menu_position;
    use gpui_kit::{
        ClickEvent, Modifiers, MouseButton, MouseClickEvent, MouseDownEvent, MouseUpEvent,
        NavigationDirection, point, px,
    };

    fn mouse(button: MouseButton) -> ClickEvent {
        let at = point(px(10.), px(20.));
        ClickEvent::Mouse(MouseClickEvent {
            down: MouseDownEvent {
                button,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            },
            up: MouseUpEvent {
                button,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
            },
        })
    }

    #[test]
    fn a_right_click_asks_for_the_menu_and_other_clicks_open_the_link() {
        assert_eq!(
            menu_position(&mouse(MouseButton::Right)),
            Some(Some(point(px(10.), px(20.))))
        );
        assert_eq!(menu_position(&mouse(MouseButton::Left)), Some(None));
        assert_eq!(menu_position(&mouse(MouseButton::Middle)), Some(None));
        assert_eq!(
            menu_position(&mouse(MouseButton::Navigate(NavigationDirection::Back))),
            None
        );
    }
}
