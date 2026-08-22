//! Session chrome layout policy for the desktop Client.
//!
//! Keeps density and inspector rules out of GPUI widgets so they stay unit-testable
//! under `cargo test -p opendb`.

/// Collapsed Staged Changes inspector width when the bag is empty.
pub const STAGED_INSPECTOR_COLLAPSED_PX: f32 = 28.0;

/// Expanded Staged Changes inspector width when there is at least one change.
pub const STAGED_INSPECTOR_EXPANDED_PX: f32 = 240.0;

/// Table / Result grid row height.
pub const GRID_ROW_HEIGHT_PX: f32 = 22.0;

/// Tab bar target height.
pub const TAB_BAR_HEIGHT_PX: f32 = 28.0;

/// DataTable `stripe` fills unused vertical space with empty zebra rows.
/// The Client keeps stripe off so the grid only paints real rows.
pub const GRID_STRIPE: bool = false;

/// Width of the Staged Changes inspector for a given bag size.
pub fn staged_inspector_width_px(staged_count: usize) -> f32 {
    if staged_count == 0 {
        STAGED_INSPECTOR_COLLAPSED_PX
    } else {
        STAGED_INSPECTOR_EXPANDED_PX
    }
}

/// Apply / Discard only belong on the expanded inspector.
pub fn staged_inspector_shows_actions(staged_count: usize) -> bool {
    staged_count > 0
}

/// Filter chips get their own row only after the first chip exists.
/// Until then `+ Filter` lives on the Tab bar.
pub fn filter_chips_own_row(filter_count: usize) -> bool {
    filter_count > 0
}

/// Namespace footer is one picker control (label + menu), never a chip list plus `+`.
pub fn namespace_footer_is_single_control() -> bool {
    true
}

/// gpui-component DataTable emits filler zebra rows only when stripe is enabled.
pub fn grid_emits_phantom_rows(stripe: bool) -> bool {
    stripe
}

/// Column width from header and cell text lengths (character heuristic).
pub fn column_width_px(header: &str, cell_texts: impl IntoIterator<Item = impl AsRef<str>>) -> f32 {
    const CHAR_PX: f32 = 7.5;
    const PAD_PX: f32 = 24.0;
    const MIN_PX: f32 = 56.0;
    const MAX_PX: f32 = 320.0;
    const SAMPLE_CAP: usize = 48;

    let mut longest = header.chars().count().min(SAMPLE_CAP);
    for text in cell_texts {
        longest = longest.max(text.as_ref().chars().count().min(SAMPLE_CAP));
    }
    (PAD_PX + longest as f32 * CHAR_PX).clamp(MIN_PX, MAX_PX)
}

/// Session window title string for the OS window metadata (task switcher), not a visible titlebar.
pub fn session_window_title(name: &str, engine_label: &str) -> String {
    format!("{name} · {engine_label} · Session")
}

/// Connection List window title string for OS window metadata.
pub fn connection_list_window_title() -> &'static str {
    "OpenDB"
}

/// Thin top drag strip height. Enough for GPUI window-move hit testing — not a titleband.
pub const WINDOW_DRAG_STRIP_HEIGHT_PX: f32 = 6.0;

/// Native OS / GPUI titlebar is hidden; Client chrome owns the top of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientWindowChromePolicy {
    /// Hide the default system titlebar (macOS / Windows).
    pub appears_transparent: bool,
    /// Prefer client-side decorations on Linux.
    pub client_decorations: bool,
    /// App owns titlebar drag regions instead of the system (macOS).
    pub app_owns_titlebar_drag: bool,
}

/// Window chrome policy for Session and Connection List windows.
pub fn client_window_chrome_policy() -> ClientWindowChromePolicy {
    ClientWindowChromePolicy {
        appears_transparent: true,
        client_decorations: true,
        app_owns_titlebar_drag: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_inspector_collapses_when_empty() {
        assert_eq!(staged_inspector_width_px(0), STAGED_INSPECTOR_COLLAPSED_PX);
        assert!(!staged_inspector_shows_actions(0));
        assert_eq!(staged_inspector_width_px(1), STAGED_INSPECTOR_EXPANDED_PX);
        assert_eq!(staged_inspector_width_px(3), STAGED_INSPECTOR_EXPANDED_PX);
        assert!(staged_inspector_shows_actions(2));
    }

    #[test]
    fn filter_row_absent_until_chips_exist() {
        assert!(!filter_chips_own_row(0));
        assert!(filter_chips_own_row(1));
        assert!(filter_chips_own_row(4));
    }

    #[test]
    fn schema_picker_is_a_single_control() {
        assert!(namespace_footer_is_single_control());
    }

    #[test]
    fn grid_does_not_emit_phantom_rows() {
        assert!(!grid_emits_phantom_rows(GRID_STRIPE));
        assert!(grid_emits_phantom_rows(true));
        assert_eq!(GRID_ROW_HEIGHT_PX, 22.0);
    }

    #[test]
    fn column_width_grows_with_header_and_content() {
        let narrow = column_width_px("id", ["1", "2"]);
        let wide = column_width_px("meta", [r#"{"role":"user","n":1}"#]);
        assert!(wide > narrow);
        assert!(narrow >= 56.0);
        assert!(wide <= 320.0);
    }

    #[test]
    fn session_title_uses_name_engine_session() {
        assert_eq!(
            session_window_title("opendb_pr14", "Postgres"),
            "opendb_pr14 · Postgres · Session"
        );
    }

    #[test]
    fn native_titlebar_is_hidden_for_client_chrome() {
        let policy = client_window_chrome_policy();
        assert!(policy.appears_transparent);
        assert!(policy.client_decorations);
        assert!(policy.app_owns_titlebar_drag);
        assert_eq!(WINDOW_DRAG_STRIP_HEIGHT_PX, 6.0);
        assert_eq!(connection_list_window_title(), "OpenDB");
    }
}
