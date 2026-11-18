//! Initialize GPUI Kit with its default dark theme.

use gpui::{App, hsla};
use gpui_component::{Theme, ThemeMode};

pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.colors.overlay = hsla(0.0, 0.0, 0.0, 0.58);
    });
}
