use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme, Disableable, h_flex, v_flex};
use opendb::{
    Cell, Client, Filter, Page, SessionId, SystemCatalogPreference, TableName, TablePage,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DraftKind {
    Equals,
    Contains,
    IsNull,
}

struct PageGrid {
    columns: Vec<Column>,
    rows: Vec<Vec<SharedString>>,
}

impl PageGrid {
    fn from_page(page: &TablePage) -> Self {
        Self {
            columns: page
                .columns()
                .iter()
                .map(|column| {
                    let name = column.as_str().to_string();
                    Column::new(name.clone(), name)
                })
                .collect(),
            rows: page
                .rows()
                .iter()
                .map(|row| row.iter().map(cell_label).collect())
                .collect(),
        }
    }
}

impl TableDelegate for PageGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = self
            .rows
            .get(row_ix)
            .and_then(|row| row.get(col_ix))
            .cloned()
            .unwrap_or_default();
        div().size_full().px_2().child(text)
    }

    fn has_more(&self, _: &App) -> bool {
        false
    }
}

pub struct SessionView {
    client: Entity<Client>,
    session_id: SessionId,
    selected: Option<TableName>,
    filters: Vec<Filter>,
    page: Page,
    has_next: bool,
    grid: Option<Entity<TableState<PageGrid>>>,
    column_input: Entity<InputState>,
    value_input: Entity<InputState>,
    draft_kind: DraftKind,
    status: SharedString,
}

impl SessionView {
    pub fn new(
        client: Entity<Client>,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&client, |_, _, cx| cx.notify()).detach();
        cx.on_release(|this, cx| {
            this.client.update(cx, |client, _cx| {
                client.close_session(this.session_id);
            });
        })
        .detach();
        let column_input = cx.new(|cx| InputState::new(window, cx).placeholder("Column"));
        let value_input = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));
        Self {
            client,
            session_id,
            selected: None,
            filters: Vec::new(),
            page: Page::first(),
            has_next: false,
            grid: None,
            column_input,
            value_input,
            draft_kind: DraftKind::Equals,
            status: SharedString::default(),
        }
    }

    fn toggle_system_catalogs(&mut self, cx: &mut Context<Self>) {
        let result = self.client.update(cx, |client, cx| {
            let next = client.show_system_catalogs().toggled();
            let result = client.set_show_system_catalogs(next);
            cx.notify();
            result
        });
        if let Err(err) = result {
            self.status = format!("{err}").into();
        }
        cx.notify();
    }

    fn open_table(&mut self, table: TableName, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(table);
        self.filters.clear();
        self.page = Page::first();
        self.reload(window, cx);
    }

    fn add_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            self.status = "Pick a Table first.".into();
            cx.notify();
            return;
        }
        let column = self.column_input.read(cx).value().trim().to_string();
        if column.is_empty() {
            self.status = "Column is required.".into();
            cx.notify();
            return;
        }
        let filter = match self.draft_kind {
            DraftKind::Equals => Filter::equals(
                column,
                Cell::Text(self.value_input.read(cx).value().to_string()),
            ),
            DraftKind::Contains => {
                Filter::contains(column, self.value_input.read(cx).value().to_string())
            }
            DraftKind::IsNull => Filter::is_null(column),
        };
        self.filters.push(filter);
        self.page = Page::first();
        self.reload(window, cx);
    }

    fn remove_filter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.filters.len() {
            self.filters.remove(index);
            self.page = Page::first();
            self.reload(window, cx);
        }
    }

    fn next_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_next {
            return;
        }
        self.page = self.page.next();
        self.reload(window, cx);
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.selected.clone() else {
            return;
        };
        match self
            .client
            .read(cx)
            .table_page(self.session_id, &table, &self.filters, self.page)
        {
            Ok(page) => {
                self.has_next = page.has_next();
                let delegate = PageGrid::from_page(&page);
                match &self.grid {
                    Some(state) => {
                        state.update(cx, |state, cx| {
                            *state.delegate_mut() = delegate;
                            state.refresh(cx);
                            cx.notify();
                        });
                    }
                    None => {
                        self.grid = Some(
                            cx.new(|cx| TableState::new(delegate, window, cx).sortable(false)),
                        );
                    }
                }
                self.status = SharedString::default();
            }
            Err(err) => {
                self.status = format!("{err}").into();
            }
        }
        cx.notify();
    }
}

impl Render for SessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, title, table_names) = {
            let client = self.client.read(cx);
            let shown = client.show_system_catalogs() == SystemCatalogPreference::Shown;
            let title = client
                .session_name(self.session_id)
                .map(|name| name.as_str().to_string())
                .unwrap_or_else(|_| "Session".into());
            let table_names = match client.tables(self.session_id) {
                Ok(catalog) => Ok(catalog
                    .tables()
                    .iter()
                    .map(|table| table.name().clone())
                    .collect::<Vec<_>>()),
                Err(err) => Err(format!("{err}")),
            };
            (shown, title, table_names)
        };

        let mut tables = v_flex().gap_1().w(px(220.));
        match table_names {
            Ok(names) if names.is_empty() => {
                tables = tables.child("No Tables.");
            }
            Ok(names) => {
                for (index, name) in names.into_iter().enumerate() {
                    let selected = self.selected.as_ref() == Some(&name);
                    let label = name.as_str().to_string();
                    tables = tables.child(
                        Button::new(("table", index as u64))
                            .label(label)
                            .when(selected, |button| button.primary())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_table(name.clone(), window, cx);
                            })),
                    );
                }
            }
            Err(err) => {
                tables = tables.child(err);
            }
        }

        let mut filter_rows = v_flex().gap_1();
        for (index, filter) in self.filters.iter().enumerate() {
            filter_rows = filter_rows.child(
                h_flex().gap_2().child(filter_label(filter)).child(
                    Button::new(("remove-filter", index as u64))
                        .label("Remove")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove_filter(index, window, cx);
                        })),
                ),
            );
        }

        let grid = match &self.grid {
            Some(state) => v_flex()
                .flex_1()
                .min_h(px(240.))
                .child(DataTable::new(state).stripe(true)),
            None => v_flex().flex_1().child("Pick a Table."),
        };

        v_flex()
            .size_full()
            .p_5()
            .gap_3()
            .bg(cx.theme().background)
            .child(title)
            .child(
                Button::new("system-catalogs")
                    .label(if shown {
                        "Hide System Catalogs"
                    } else {
                        "Show System Catalogs"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_system_catalogs(cx))),
            )
            .child(
                h_flex().size_full().gap_4().child(tables).child(
                    v_flex()
                        .flex_1()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Input::new(&self.column_input))
                                .child(
                                    Button::new("kind-equals")
                                        .label("Equals")
                                        .when(self.draft_kind == DraftKind::Equals, |button| {
                                            button.primary()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.draft_kind = DraftKind::Equals;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("kind-contains")
                                        .label("Contains")
                                        .when(self.draft_kind == DraftKind::Contains, |button| {
                                            button.primary()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.draft_kind = DraftKind::Contains;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("kind-null")
                                        .label("Null")
                                        .when(self.draft_kind == DraftKind::IsNull, |button| {
                                            button.primary()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.draft_kind = DraftKind::IsNull;
                                            cx.notify();
                                        })),
                                )
                                .child(Input::new(&self.value_input))
                                .child(Button::new("add-filter").label("Add Filter").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.add_filter(window, cx);
                                    }),
                                )),
                        )
                        .child(filter_rows)
                        .child(grid)
                        .child(
                            Button::new("next-page")
                                .label("Next page")
                                .disabled(!self.has_next)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.next_page(window, cx);
                                })),
                        ),
                ),
            )
            .child(self.status.clone())
    }
}

fn cell_label(cell: &Cell) -> SharedString {
    match cell {
        Cell::Null => SharedString::default(),
        Cell::Integer(value) => value.to_string().into(),
        Cell::Real(value) => value.to_string().into(),
        Cell::Text(value) => value.clone().into(),
        Cell::Blob(value) => format!("<{} bytes>", value.len()).into(),
    }
}

fn filter_label(filter: &Filter) -> String {
    match filter {
        Filter::Equals { column, value } => {
            format!("{} = {}", column.as_str(), cell_label(value))
        }
        Filter::Contains { column, value } => {
            format!("{} contains {}", column.as_str(), value)
        }
        Filter::IsNull { column } => format!("{} is null", column.as_str()),
    }
}
