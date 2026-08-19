use std::fs;

use directories::ProjectDirs;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Root, h_flex, v_flex};
use gpui_component_assets::Assets;
use opendb::{AddResult, Connection, ConnectionList, ConnectionString, FileStore};

pub struct ConnectionListView {
    store: FileStore,
    list: ConnectionList,
    paste: Entity<InputState>,
    selected: Option<usize>,
    status: SharedString,
}

impl ConnectionListView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = FileStore::new(store_path());
        let list = store.load().unwrap_or_else(|_| ConnectionList::new());
        let paste =
            cx.new(|cx| InputState::new(window, cx).placeholder("Paste a Connection String"));
        Self {
            store,
            list,
            paste,
            selected: None,
            status: SharedString::default(),
        }
    }

    fn add(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.paste.read(cx).value().to_string();
        match ConnectionString::parse(&raw) {
            Ok(string) => match self.list.add(Connection::from_string(string)) {
                AddResult::Added => {
                    if let Err(err) = self.store.save(&self.list) {
                        self.status = format!("{err}").into();
                    } else {
                        self.status = "Saved.".into();
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

    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            self.status = "Pick a Connection first.".into();
            cx.notify();
            return;
        };
        let raw = self.list.connections()[index]
            .connection_string()
            .as_str()
            .to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(raw));
        self.status = "Connection String copied.".into();
        cx.notify();
    }
}

impl Render for ConnectionListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = v_flex().gap_1();
        for (index, connection) in self.list.connections().iter().enumerate() {
            let selected = self.selected == Some(index);
            rows = rows.child(
                Button::new(("connection", index as u64))
                    .label(connection.name().as_str().to_string())
                    .when(selected, |b| b.primary())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(index);
                        cx.notify();
                    })),
            );
        }

        v_flex()
            .size_full()
            .p_5()
            .gap_3()
            .bg(cx.theme().background)
            .child("OpenDB")
            .child(Input::new(&self.paste))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("add")
                            .primary()
                            .label("Add")
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    )
                    .child(
                        Button::new("export")
                            .label("Export")
                            .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
                    )
                    .child(
                        Button::new("import")
                            .label("Import")
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    )
                    .child(
                        Button::new("copy")
                            .label("Copy Connection String")
                            .on_click(cx.listener(|this, _, _, cx| this.copy_selected(cx))),
                    ),
            )
            .child(rows)
            .child(self.status.clone())
    }
}

fn store_path() -> std::path::PathBuf {
    let dirs = ProjectDirs::from("dev", "OpenDB", "opendb").expect("app data directory");
    dirs.data_dir().join("connection-list.json")
}

fn main() {
    let app = gpui_platform::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(720.), px(520.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("OpenDB".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                let view = cx.new(|cx| ConnectionListView::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
