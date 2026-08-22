mod session;

use std::fs;

use directories::ProjectDirs;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::kbd::Kbd;
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{
    ActiveTheme, Disableable, IconName, Root, Theme, ThemeMode, WindowExt, h_flex, v_flex,
};
use gpui_component_assets::Assets;
use opendb::{AddResult, Client, Connection, ConnectionList, ConnectionString, Engine, FileStore};
use session::SessionView;

actions!(
    connection_list,
    [SelectPrev, SelectNext, OpenSelected, FocusFilter]
);

pub struct ConnectionListView {
    focus_handle: FocusHandle,
    client: Entity<Client>,
    store: FileStore,
    list: ConnectionList,
    filter: Entity<InputState>,
    paste: Entity<InputState>,
    selected: Option<usize>,
    status: SharedString,
}

impl ConnectionListView {
    fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = FileStore::new(store_path());
        let list = store.load().unwrap_or_else(|_| ConnectionList::new());
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter Connections")
        });
        let paste =
            cx.new(|cx| InputState::new(window, cx).placeholder("Paste a Connection String"));
        cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.ensure_selection_visible(cx);
                cx.notify();
            }
        })
        .detach();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let mut view = Self {
            focus_handle,
            client,
            store,
            list,
            filter,
            paste,
            selected: None,
            status: SharedString::default(),
        };
        if !view.list.connections().is_empty() {
            view.selected = Some(0);
        }
        view
    }

    fn filtered_indices(&self, cx: &App) -> Vec<usize> {
        let query = self.filter.read(cx).value().trim().to_lowercase();
        self.list
            .connections()
            .iter()
            .enumerate()
            .filter(|(_, connection)| {
                if query.is_empty() {
                    return true;
                }
                let name = connection.name().as_str().to_lowercase();
                let subtitle = connection
                    .connection_string()
                    .list_subtitle()
                    .to_lowercase();
                name.contains(&query) || subtitle.contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn ensure_selection_visible(&mut self, cx: &App) {
        let filtered = self.filtered_indices(cx);
        if filtered.is_empty() {
            self.selected = None;
            return;
        }
        if let Some(selected) = self.selected {
            if filtered.contains(&selected) {
                return;
            }
        }
        self.selected = filtered.first().copied();
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let filtered = self.filtered_indices(cx);
        if filtered.is_empty() {
            return;
        }
        let position = self
            .selected
            .and_then(|selected| filtered.iter().position(|&index| index == selected))
            .unwrap_or(0);
        let next = if position == 0 {
            filtered.len() - 1
        } else {
            position - 1
        };
        self.selected = Some(filtered[next]);
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let filtered = self.filtered_indices(cx);
        if filtered.is_empty() {
            return;
        }
        let position = self
            .selected
            .and_then(|selected| filtered.iter().position(|&index| index == selected))
            .unwrap_or(usize::MAX);
        let next = if position == usize::MAX || position + 1 >= filtered.len() {
            0
        } else {
            position + 1
        };
        self.selected = Some(filtered[next]);
        cx.notify();
    }

    fn open_selected_action(
        &mut self,
        _: &OpenSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_selected(cx);
    }

    fn focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |input, cx| input.focus(window, cx));
    }

    fn open_add_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paste = self.paste.clone();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Add Connection")
                .child(
                    v_flex()
                        .gap_2()
                        .w(px(420.))
                        .child("Paste a Connection String. The secret stays on this Connection.")
                        .child(Input::new(&paste)),
                )
                .footer(DialogFooter::new().child(
                    Button::new("save-connection").primary().label("Add").on_click({
                        let view = view.clone();
                        move |_, window, cx| {
                            view.update(cx, |this, cx| {
                                this.add(window, cx);
                                if this.status.as_ref() == "Saved." {
                                    window.close_dialog(cx);
                                }
                            })
                            .ok();
                        }
                    }),
                ).child(DialogClose::new().child(Button::new("cancel-add").label("Cancel"))))
        });
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.paste.read(cx).value().to_string();
        match ConnectionString::parse(&raw) {
            Ok(string) => match self.list.add(Connection::from_string(string)) {
                AddResult::Added => {
                    if let Err(err) = self.store.save(&self.list) {
                        self.status = format!("{err}").into();
                    } else {
                        self.status = "Saved.".into();
                        self.selected = Some(self.list.connections().len() - 1);
                        self.paste.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                        });
                    }
                }
                AddResult::Duplicate => {
                    self.status = "That Connection String is already on the list.".into();
                }
            },
            Err(err) => {
                self.status = format!("{err}").into();
            }
        }
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("connection-list.json")
            .save_file()
        else {
            return;
        };
        match self.list.to_bundle_json() {
            Ok(json) => {
                if let Err(err) = fs::write(&path, json) {
                    self.status = format!("{err}").into();
                } else {
                    self.status = "Exported.".into();
                }
            }
            Err(err) => self.status = format!("{err}").into(),
        }
        cx.notify();
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        match fs::read_to_string(&path) {
            Ok(json) => match ConnectionList::from_bundle_json(&json) {
                Ok(incoming) => {
                    self.list.merge(incoming);
                    if let Err(err) = self.store.save(&self.list) {
                        self.status = format!("{err}").into();
                    } else {
                        self.status = "Imported.".into();
                    }
                }
                Err(err) => self.status = format!("{err}").into(),
            },
            Err(err) => self.status = format!("{err}").into(),
        }
        cx.notify();
    }

    fn copy_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(connection) = self.list.connections().get(index) else {
            self.status = "Pick a Connection first.".into();
            cx.notify();
            return;
        };
        let raw = connection.connection_string().as_str().to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(raw));
        self.status = "Connection String copied.".into();
        cx.notify();
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            self.status = "Pick a Connection first.".into();
            cx.notify();
            return;
        };
        let connection = self.list.connections()[index].clone();
        let opened = self
            .client
            .update(cx, |client, _cx| client.open_session(&connection));
        match opened {
            Ok(session_id) => {
                let client = self.client.clone();
                let name = connection.name().as_str().to_string();
                let engine = connection.connection_string().engine();
                let options = session_window_options(&name, engine, cx);
                if let Err(err) = cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| SessionView::new(client, session_id, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                }) {
                    self.status = format!("{err}").into();
                    cx.notify();
                }
            }
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
            }
        }
    }

    fn connection_count_label(&self) -> SharedString {
        let count = self.list.connections().len();
        let noun = if count == 1 {
            "Connection"
        } else {
            "Connections"
        };
        format!("{count} {noun} · Enter opens a Session.").into()
    }
}

impl Focusable for ConnectionListView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectionListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filtered = self.filtered_indices(cx);
        let selected = self.selected;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let selected_bg = cx.theme().accent;
        let secondary = cx.theme().secondary;
        let foreground = cx.theme().foreground;
        let empty_list = self.list.connections().is_empty();

        let mut rows = v_flex().id("connection-rows").flex_1().w_full().overflow_y_scroll();
        if filtered.is_empty() {
            rows = rows.child(
                div()
                    .p_4()
                    .text_color(muted)
                    .child(if empty_list {
                        "No Connections yet. Add one with a Connection String."
                    } else {
                        "No Connections match this filter."
                    }),
            );
        } else {
            for index in filtered {
                let connection = &self.list.connections()[index];
                let engine = connection.connection_string().engine();
                let name = connection.name().as_str().to_string();
                let subtitle = connection.connection_string().list_subtitle();
                let is_selected = selected == Some(index);
                let view = cx.entity().downgrade();

                rows = rows.child(
                    div()
                        .id(("connection-row", index as u64))
                        .w_full()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(border)
                        .when(is_selected, |row| row.bg(selected_bg))
                        .when(!is_selected, |row| {
                            row.hover(move |style| style.bg(secondary))
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.selected = Some(index);
                            if event.click_count() >= 2 {
                                this.open_selected(cx);
                            } else {
                                cx.notify();
                            }
                        }))
                        .context_menu({
                            let view = view.clone();
                            move |menu, _, _| {
                                menu.item(
                                    PopupMenuItem::new("Copy Connection String (secret)").on_click({
                                        let view = view.clone();
                                        move |_, _, cx| {
                                            view.update(cx, |this, cx| {
                                                this.copy_at(index, cx);
                                            })
                                            .ok();
                                        }
                                    }),
                                )
                            }
                        })
                        .child(
                            h_flex()
                                .gap_3()
                                .items_center()
                                .child(engine_badge(engine))
                                .child(
                                    v_flex()
                                        .gap_0()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(foreground)
                                                .child(name),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(subtitle),
                                        ),
                                ),
                        ),
                );
            }
        }

        let open_enabled = selected.is_some();
        let view = cx.entity().downgrade();

        div()
            .id("connection-list")
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("ConnectionList")
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_selected_action))
            .on_action(cx.listener(Self::focus_filter))
            .bg(cx.theme().background)
            .text_color(foreground)
            .child(
                v_flex().size_full().child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            Input::new(&self.filter)
                                .prefix(IconName::Search)
                                .suffix(
                                    Kbd::new(Keystroke::parse("cmd-f").unwrap_or_else(|_| {
                                        Keystroke::parse("ctrl-f").unwrap()
                                    }))
                                    .appearance(false),
                                ),
                        ),
                )
                .child(rows)
                .child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            Button::new("open-session")
                                .primary()
                                .icon(IconName::SquareTerminal)
                                .label("Open Session")
                                .disabled(!open_enabled)
                                .on_click(cx.listener(|this, _, _, cx| this.open_selected(cx))),
                        )
                        .child(
                            Button::new("add")
                                .icon(IconName::Plus)
                                .label("Add")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_add_sheet(window, cx);
                                })),
                        )
                        .child(
                            Button::new("more")
                                .label("More")
                                .icon(IconName::Ellipsis)
                                .dropdown_caret(true)
                                .dropdown_menu({
                                    let view = view.clone();
                                    move |menu, _, _| {
                                        menu.item(PopupMenuItem::new("Export").on_click({
                                            let view = view.clone();
                                            move |_, _, cx| {
                                                view.update(cx, |this, cx| this.export(cx))
                                                    .ok();
                                            }
                                        }))
                                        .item(PopupMenuItem::new("Import").on_click({
                                            let view = view.clone();
                                            move |_, _, cx| {
                                                view.update(cx, |this, cx| this.import(cx))
                                                    .ok();
                                            }
                                        }))
                                    }
                                }),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(if self.status.is_empty() {
                                    self.connection_count_label()
                                } else {
                                    self.status.clone()
                                }),
                        ),
                ),
            )
    }
}

fn engine_badge(engine: Engine) -> impl IntoElement {
    let (bg, label) = match engine {
        Engine::Postgres => (rgb(0x166534), "PG"),
        Engine::Sqlite => (rgb(0x52525b), "SQL"),
        Engine::Mysql => (rgb(0xb45309), "MY"),
    };
    div()
        .flex_shrink_0()
        .w(px(34.))
        .h(px(24.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(rgb(0xffffff))
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(label)
}

fn app_data_dir() -> std::path::PathBuf {
    let dirs = ProjectDirs::from("dev", "OpenDB", "opendb").expect("app data directory");
    dirs.data_dir().to_path_buf()
}

fn store_path() -> std::path::PathBuf {
    app_data_dir().join("connection-list.json")
}

fn preferences_path() -> std::path::PathBuf {
    app_data_dir().join("preferences.json")
}

fn session_window_options(name: &str, engine: Engine, cx: &App) -> WindowOptions {
    let title = format!("{name} · {} · Session", engine.label());
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1100.), px(720.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.bind_keys([
            KeyBinding::new("up", SelectPrev, Some("ConnectionList")),
            KeyBinding::new("down", SelectNext, Some("ConnectionList")),
            KeyBinding::new("enter", OpenSelected, Some("ConnectionList")),
            KeyBinding::new("cmd-f", FocusFilter, Some("ConnectionList")),
            KeyBinding::new("ctrl-f", FocusFilter, Some("ConnectionList")),
        ]);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(520.), px(640.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("OpenDB".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                let client =
                    cx.new(|_cx| Client::open(preferences_path()).expect("Client preferences"));
                let view = cx.new(|cx| ConnectionListView::new(client, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
