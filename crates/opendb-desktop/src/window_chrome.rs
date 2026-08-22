use gpui::*;
use gpui_component::ActiveTheme;
use opendb::{
    Engine, WINDOW_DRAG_STRIP_HEIGHT_PX, client_window_chrome_policy, connection_list_window_title,
    session_window_title,
};

/// Thin top strip for GPUI window move when the native titlebar is hidden.
pub(crate) fn window_drag_strip(cx: &App) -> impl IntoElement {
    div()
        .id("window-drag-strip")
        .w_full()
        .h(px(WINDOW_DRAG_STRIP_HEIGHT_PX))
        .flex_shrink_0()
        .bg(cx.theme().background)
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.start_window_move();
        })
}

fn client_chrome_window_options(
    title: impl Into<SharedString>,
    bounds: Size<Pixels>,
    cx: &App,
) -> WindowOptions {
    let policy = client_window_chrome_policy();
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(bounds, cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: policy.appears_transparent,
            ..Default::default()
        }),
        window_decorations: policy
            .client_decorations
            .then_some(WindowDecorations::Client),
        app_owns_titlebar_drag: policy.app_owns_titlebar_drag,
        ..Default::default()
    }
}

pub(crate) fn session_window_options(name: &str, engine: Engine, cx: &App) -> WindowOptions {
    let title = session_window_title(name, engine.label());
    client_chrome_window_options(title, size(px(1100.), px(720.)), cx)
}

pub(crate) fn connection_list_window_options(cx: &App) -> WindowOptions {
    client_chrome_window_options(
        connection_list_window_title(),
        size(px(520.), px(640.)),
        cx,
    )
}
