use std::collections::HashSet;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable, WindowExt, h_flex, v_flex};
use opendb::{
    Cell, Client, ColumnDefinition, ColumnName, Filter, Namespace, Page, ResultStaging, RowIdentity,
    SchemaChange, SessionId, SqlKind, StagedChange, StagedChangeId, SystemCatalogPreference, Table,
    TableName, TablePage, TABLE_PAGE_SIZE,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    Equals,
    Contains,
    IsNull,
}

#[derive(Clone, PartialEq)]
enum TabKind {
    Table {
        table: Table,
        filters: Vec<Filter>,
        page: Page,
        has_next: bool,
        grid: Option<Entity<TableState<PageGrid>>>,
    },
    Query {
        editor: Entity<EditorState>,
        sql: String,
        staging: ResultStaging,
        page: Page,
        has_next: bool,
        grid: Option<Entity<TableState<PageGrid>>>,
    },
    Structure {
        table: Table,
    },
}

struct SessionTab {
    id: u64,
    kind: TabKind,
}

struct PageGrid {
    columns: Vec<Column>,
    column_names: Vec<ColumnName>,
    rows: Vec<Vec<Cell>>,
    selected_row: Option<usize>,
    editing: Option<(usize, usize)>,
    pending_insert: Option<Vec<Cell>>,
    edit_input: Entity<InputState>,
    editable: bool,
}

impl PageGrid {
    fn from_page(page: &TablePage, edit_input: Entity<InputState>, editable: bool) -> Self {
        Self {
            columns: page
                .columns()
                .iter()
                .map(|column| {
                    let name = column.as_str().to_string();
                    Column::new(name.clone(), name)
                })
                .collect(),
            column_names: page.columns().to_vec(),
            rows: page.rows().to_vec(),
            selected_row: None,
            editing: None,
            pending_insert: None,
            edit_input,
            editable,
        }
    }

    fn row_count(&self) -> usize {
        self.rows.len() + usize::from(self.pending_insert.is_some())
    }

    fn row_cells(&self, row_ix: usize) -> Option<&[Cell]> {
        if row_ix < self.rows.len() {
            self.rows.get(row_ix).map(|row| row.as_slice())
        } else {
            self.pending_insert.as_deref()
        }
    }

    fn row_cells_mut(&mut self, row_ix: usize) -> Option<&mut Vec<Cell>> {
        if row_ix < self.rows.len() {
            self.rows.get_mut(row_ix)
        } else {
            self.pending_insert.as_mut()
        }
    }

    fn named_row(&self, row_ix: usize) -> Option<Vec<(ColumnName, Cell)>> {
        let row = self.row_cells(row_ix)?;
        Some(
            self.column_names
                .iter()
                .cloned()
                .zip(row.iter().cloned())
                .collect(),
        )
    }

    fn begin_edit(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(cell) = self
            .row_cells(row_ix)
            .and_then(|row| row.get(col_ix))
            .cloned()
        else {
            return;
        };
        if matches!(cell, Cell::Blob(_)) || !self.editable {
            self.selected_row = Some(row_ix);
            return;
        }
        self.editing = Some((row_ix, col_ix));
        self.selected_row = Some(row_ix);
        let text = cell_edit_text(&cell);
        self.edit_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
    }

    fn start_pending_insert(&mut self) {
        let width = self.column_names.len();
        self.pending_insert = Some(vec![Cell::Null; width]);
        self.selected_row = Some(self.rows.len());
        self.editing = None;
    }

    fn take_insert_values(&mut self) -> Vec<(ColumnName, Cell)> {
        let Some(draft) = self.pending_insert.take() else {
            return Vec::new();
        };
        self.column_names
            .iter()
            .cloned()
            .zip(draft)
            .filter(|(_, cell)| !matches!(cell, Cell::Null))
            .collect()
    }
}

impl TableDelegate for PageGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.row_count()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if self.editing == Some((row_ix, col_ix)) {
            return div()
                .size_full()
                .child(Input::new(&self.edit_input))
                .into_any_element();
        }
        let text = self
            .row_cells(row_ix)
            .and_then(|row| row.get(col_ix))
            .map(cell_label)
            .unwrap_or_default();
        div()
            .id(("cell", (row_ix as u64) << 32 | col_ix as u64))
            .size_full()
            .px_2()
            .on_click(cx.listener(move |table, _, window, cx| {
                table.delegate_mut().begin_edit(row_ix, col_ix, window, cx);
                cx.notify();
            }))
            .child(text)
            .into_any_element()
    }

    fn has_more(&self, _: &App) -> bool {
        false
    }
}

pub struct SessionView {
    client: Entity<Client>,
    session_id: SessionId,
    tabs: Vec<SessionTab>,
    active_tab: usize,
    next_tab_id: u64,
    table_search: Entity<InputState>,
    collapsed_namespaces: HashSet<String>,
    column_input: Entity<InputState>,
    value_input: Entity<InputState>,
    edit_input: Entity<InputState>,
    schema_table_input: Entity<InputState>,
    schema_columns_input: Entity<InputState>,
    schema_column_name_input: Entity<InputState>,
    schema_column_type_input: Entity<InputState>,
    schema_rename_to_input: Entity<InputState>,
    schema_index_name_input: Entity<InputState>,
    schema_index_columns_input: Entity<InputState>,
    schema_unique_index: bool,
    filter_kind: FilterKind,
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
        let edit_input = cx.new(|cx| InputState::new(window, cx));
        let table_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search Tables"));
        let schema_table_input = cx.new(|cx| InputState::new(window, cx).placeholder("Table name"));
        let schema_columns_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("id INTEGER PRIMARY KEY\nname TEXT")
        });
        let schema_column_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Column name"));
        let schema_column_type_input = cx.new(|cx| InputState::new(window, cx).placeholder("Type"));
        let schema_rename_to_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("New column name"));
        let schema_index_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Index name"));
        let schema_index_columns_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Columns (comma-separated)"));
        cx.subscribe(&edit_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_cell(cx);
            }
        })
        .detach();
        cx.subscribe(&table_search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            entity
                .update(cx, |this, cx| this.on_should_close(window, cx))
                .unwrap_or(true)
        });
        Self {
            client,
            session_id,
            tabs: Vec::new(),
            active_tab: 0,
            next_tab_id: 1,
            table_search,
            collapsed_namespaces: HashSet::new(),
            column_input,
            value_input,
            edit_input,
            schema_table_input,
            schema_columns_input,
            schema_column_name_input,
            schema_column_type_input,
            schema_rename_to_input,
            schema_index_name_input,
            schema_index_columns_input,
            schema_unique_index: false,
            filter_kind: FilterKind::Equals,
            status: SharedString::default(),
        }
    }

    fn on_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dirty = self
            .client
            .read(cx)
            .staged_changes(self.session_id)
            .map(|changes| !changes.is_empty())
            .unwrap_or(false);
        if !dirty {
            return true;
        }
        if window.has_active_dialog(cx) {
            return false;
        }
        self.open_close_prompt(window, cx);
        false
    }

    fn open_close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Staged Changes")
                .child("Apply them, discard them, or keep this Session open.")
                .overlay_closable(false)
                .footer(
                    DialogFooter::new()
                        .child(Button::new("apply").primary().label("Apply").on_click({
                            let session = session.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        if this.apply_changes(window, cx) {
                                            window.close_dialog(cx);
                                            window.remove_window();
                                        }
                                    })
                                    .ok();
                            }
                        }))
                        .child(Button::new("discard").label("Discard").on_click({
                            let session = session.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        this.discard_all(cx);
                                        window.close_dialog(cx);
                                        window.remove_window();
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel").label("Cancel"))),
                )
        });
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

    fn add_table_tab(&mut self, table: Table, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(SessionTab {
            id: self.next_tab_id,
            kind: TabKind::Table {
                table,
                filters: Vec::new(),
                page: Page::first(),
                has_next: false,
                grid: None,
            },
        });
        self.next_tab_id += 1;
        self.active_tab = self.tabs.len() - 1;
        self.reload(window, cx);
    }

    fn add_structure_tab(
        &mut self,
        table: Table,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tabs.push(SessionTab {
            id: self.next_tab_id,
            kind: TabKind::Structure { table },
        });
        self.next_tab_id += 1;
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
    }

    fn add_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| {
            // gpui-component pin lacks tree-sitter-sql (conflicts with gpui's cc pin); enable that feature for real highlighting.
            EditorState::new(window, cx)
                .placeholder("SQL")
                .language("sql")
        });
        self.tabs.push(SessionTab {
            id: self.next_tab_id,
            kind: TabKind::Query {
                editor,
                sql: String::new(),
                staging: ResultStaging::ReadOnly,
                page: Page::first(),
                has_next: false,
                grid: None,
            },
        });
        self.next_tab_id += 1;
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab = index;
            cx.notify();
        }
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if self.active_tab > index {
            self.active_tab -= 1;
        }
        cx.notify();
    }

    fn tab_label(&self, tab: &SessionTab, cx: &App) -> String {
        match &tab.kind {
            TabKind::Table { table, .. } => table_label(table),
            TabKind::Query { editor, .. } => {
                let sql = editor.read(cx).value();
                let trimmed = sql.trim();
                if trimmed.is_empty() {
                    "Query".into()
                } else {
                    let line = trimmed.lines().next().unwrap_or(trimmed);
                    truncate_label(line, 40)
                }
            }
            TabKind::Structure { table } => format!("{} (structure)", table_label(table)),
        }
    }

    fn add_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabKind::Table { filters, .. }) = self
            .tabs
            .get_mut(self.active_tab)
            .map(|tab| &mut tab.kind)
        else {
            self.status = "Filters apply to a Table Tab.".into();
            cx.notify();
            return;
        };
        let column = self.column_input.read(cx).value().trim().to_string();
        if column.is_empty() {
            self.status = "Column is required.".into();
            cx.notify();
            return;
        }
        let filter = match self.filter_kind {
            FilterKind::Equals => Filter::equals(
                column,
                Cell::Text(self.value_input.read(cx).value().to_string()),
            ),
            FilterKind::Contains => {
                Filter::contains(column, self.value_input.read(cx).value().to_string())
            }
            FilterKind::IsNull => Filter::is_null(column),
        };
        filters.push(filter);
        if let TabKind::Table { page, .. } = &mut self.tabs[self.active_tab].kind {
            *page = Page::first();
        }
        self.reload(window, cx);
    }

    fn remove_filter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabKind::Table { filters, page, .. }) =
            self.tabs.get_mut(self.active_tab).map(|tab| &mut tab.kind)
        else {
            return;
        };
        if index < filters.len() {
            filters.remove(index);
            *page = Page::first();
            self.reload(window, cx);
        }
    }

    fn next_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_next = match &self.tabs.get(self.active_tab).map(|tab| &tab.kind) {
            Some(TabKind::Table { has_next, .. }) | Some(TabKind::Query { has_next, .. }) => {
                *has_next
            }
            _ => false,
        };
        if !has_next {
            return;
        }
        match &mut self.tabs[self.active_tab].kind {
            TabKind::Table { page, .. } | TabKind::Query { page, .. } => {
                *page = page.next();
            }
            TabKind::Structure { .. } => {}
        }
        self.reload(window, cx);
    }

    fn prev_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = match &self.tabs.get(self.active_tab).map(|tab| &tab.kind) {
            Some(TabKind::Table { page, .. }) | Some(TabKind::Query { page, .. }) => *page,
            _ => return,
        };
        let Some(prev) = page.prev() else {
            return;
        };
        match &mut self.tabs[self.active_tab].kind {
            TabKind::Table { page, .. } | TabKind::Query { page, .. } => {
                *page = prev;
            }
            TabKind::Structure { .. } => {}
        }
        self.reload(window, cx);
    }

    fn open_filter_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.tabs.get(self.active_tab).map(|tab| &tab.kind),
            Some(TabKind::Table { .. })
        ) {
            self.status = "Filters apply to a Table Tab.".into();
            cx.notify();
            return;
        }
        let session = cx.entity().downgrade();
        let column_input = self.column_input.clone();
        let value_input = self.value_input.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let kind = session
                .upgrade()
                .map(|entity| entity.read(cx).filter_kind)
                .unwrap_or(FilterKind::Equals);
            dialog
                .title("Add Filter")
                .child(
                    v_flex()
                        .gap_2()
                        .w(px(360.))
                        .child(Input::new(&column_input))
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("kind-equals")
                                        .label("Equals")
                                        .when(kind == FilterKind::Equals, |button| button.primary())
                                        .on_click({
                                            let session = session.clone();
                                            move |_, _, cx| {
                                                session
                                                    .update(cx, |this, cx| {
                                                        this.filter_kind = FilterKind::Equals;
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("kind-contains")
                                        .label("Contains")
                                        .when(kind == FilterKind::Contains, |button| {
                                            button.primary()
                                        })
                                        .on_click({
                                            let session = session.clone();
                                            move |_, _, cx| {
                                                session
                                                    .update(cx, |this, cx| {
                                                        this.filter_kind = FilterKind::Contains;
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("kind-null")
                                        .label("Null")
                                        .when(kind == FilterKind::IsNull, |button| button.primary())
                                        .on_click({
                                            let session = session.clone();
                                            move |_, _, cx| {
                                                session
                                                    .update(cx, |this, cx| {
                                                        this.filter_kind = FilterKind::IsNull;
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }
                                        }),
                                ),
                        )
                        .child(Input::new(&value_input)),
                )
                .footer(
                    DialogFooter::new()
                        .child(Button::new("add-filter").primary().label("Add Filter").on_click({
                            let session = session.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        this.add_filter(window, cx);
                                        window.close_dialog(cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel-filter").label("Cancel"))),
                )
        });
    }

    fn toggle_namespace(&mut self, namespace: &str, cx: &mut Context<Self>) {
        if self.collapsed_namespaces.contains(namespace) {
            self.collapsed_namespaces.remove(namespace);
        } else {
            self.collapsed_namespaces.insert(namespace.to_string());
        }
        cx.notify();
    }

    fn table_matches_search(&self, table: &Table, cx: &App) -> bool {
        let query = self.table_search.read(cx).value().trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let name = table.name().as_str().to_lowercase();
        let full = table_label(table).to_lowercase();
        name.contains(&query) || full.contains(&query)
    }

    fn active_table(&self) -> Option<Table> {
        match self.tabs.get(self.active_tab).map(|tab| &tab.kind) {
            Some(TabKind::Table { table, .. }) => Some(table.clone()),
            Some(TabKind::Query {
                staging: ResultStaging::Staged { table },
                ..
            }) => Some(table.clone()),
            _ => None,
        }
    }

    fn active_grid(&self) -> Option<Entity<TableState<PageGrid>>> {
        match self.tabs.get(self.active_tab).map(|tab| &tab.kind) {
            Some(TabKind::Table { grid, .. }) | Some(TabKind::Query { grid, .. }) => {
                grid.clone()
            }
            _ => None,
        }
    }

    fn run_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabKind::Query { editor, .. }) =
            self.tabs.get(self.active_tab).map(|tab| &tab.kind)
        else {
            self.status = "Open a Query Tab first.".into();
            cx.notify();
            return;
        };
        let sql = editor.read(cx).value().to_string();
        if sql.trim().is_empty() {
            self.status = "SQL is required.".into();
            cx.notify();
            return;
        }
        let kind = self.client.read(cx).sql_kind(self.session_id, &sql);
        match kind {
            Ok(SqlKind::Read) => self.run_query(sql, window, cx),
            Ok(SqlKind::Mutating) => self.confirm_mutating(sql, window, cx),
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
            }
        }
    }

    fn run_query(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        if let TabKind::Query {
            sql: stored,
            staging,
            page,
            ..
        } = &mut self.tabs[self.active_tab].kind
        {
            *stored = sql;
            *staging = ResultStaging::ReadOnly;
            *page = Page::first();
        }
        self.reload(window, cx);
    }

    fn confirm_mutating(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Run SQL")
                .child("This SQL will change the Database.")
                .footer(
                    DialogFooter::new()
                        .child(Button::new("run").primary().label("Run").on_click({
                            let session = session.clone();
                            let sql = sql.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        this.execute_mutating(&sql, window, cx);
                                        window.close_dialog(cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel").label("Cancel"))),
                )
        });
    }

    fn execute_mutating(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.client.update(cx, |client, cx| {
            let result = client.execute_mutating(self.session_id, sql);
            cx.notify();
            result
        });
        match result {
            Ok(()) => {
                self.status = SharedString::default();
                self.reload_all_grids(window, cx);
            }
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
            }
        }
    }

    fn reload_all_grids(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        for index in 0..count {
            let previous = self.active_tab;
            self.active_tab = index;
            if matches!(
                self.tabs[index].kind,
                TabKind::Table { .. } | TabKind::Query { .. }
            ) {
                self.reload(window, cx);
            }
            self.active_tab = previous;
        }
        cx.notify();
    }

    fn commit_cell(&mut self, cx: &mut Context<Self>) {
        let Some(grid) = self.active_grid() else {
            return;
        };
        let Some(table) = self.active_table() else {
            return;
        };
        let Some((row_ix, col_ix)) = grid.read(cx).delegate().editing else {
            return;
        };
        let Some(original) = grid
            .read(cx)
            .delegate()
            .row_cells(row_ix)
            .and_then(|row| row.get(col_ix))
            .cloned()
        else {
            return;
        };
        let Some(column) = grid.read(cx).delegate().column_names.get(col_ix).cloned() else {
            return;
        };
        let text = self.edit_input.read(cx).value().to_string();
        let new_cell = cell_from_input(&original, &text);
        let is_draft = row_ix >= grid.read(cx).delegate().rows.len();
        if is_draft {
            grid.update(cx, |state, cx| {
                if let Some(row) = state.delegate_mut().row_cells_mut(row_ix) {
                    if let Some(cell) = row.get_mut(col_ix) {
                        *cell = new_cell;
                    }
                }
                state.delegate_mut().editing = None;
                state.refresh(cx);
            });
            cx.notify();
            return;
        }
        if new_cell == original {
            grid.update(cx, |state, cx| {
                state.delegate_mut().editing = None;
                state.refresh(cx);
            });
            cx.notify();
            return;
        }
        let Some(last_seen) = grid.read(cx).delegate().named_row(row_ix) else {
            return;
        };
        let result = self.client.update(cx, |client, cx| {
            let result =
                client.stage_update(self.session_id, table, last_seen, vec![(column, new_cell)]);
            cx.notify();
            result
        });
        grid.update(cx, |state, cx| {
            state.delegate_mut().editing = None;
            state.refresh(cx);
        });
        match result {
            Ok(_) => self.status = SharedString::default(),
            Err(err) => self.status = format!("{err}").into(),
        }
        cx.notify();
    }

    fn stage_insert_row(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.active_table() else {
            self.status = active_tab_stage_error(&self.tabs, self.active_tab);
            cx.notify();
            return;
        };
        let Some(grid) = self.active_grid() else {
            return;
        };
        let values = grid.update(cx, |state, cx| {
            let values = state.delegate_mut().take_insert_values();
            state.refresh(cx);
            values
        });
        if values.is_empty() {
            self.status = "Add values to the insert row first.".into();
            cx.notify();
            return;
        }
        let result = self.client.update(cx, |client, cx| {
            let result = client.stage_insert(self.session_id, table, values);
            cx.notify();
            result
        });
        match result {
            Ok(_) => self.status = SharedString::default(),
            Err(err) => self.status = format!("{err}").into(),
        }
        cx.notify();
    }

    fn start_insert_row(&mut self, cx: &mut Context<Self>) {
        if self.active_table().is_none() {
            self.status = active_tab_stage_error(&self.tabs, self.active_tab);
            cx.notify();
            return;
        }
        let Some(grid) = self.active_grid() else {
            self.status = "Open a Table or staged Result Tab first.".into();
            cx.notify();
            return;
        };
        grid.update(cx, |state, cx| {
            state.delegate_mut().start_pending_insert();
            state.refresh(cx);
        });
        cx.notify();
    }

    fn stage_delete_row(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.active_table() else {
            self.status = active_tab_stage_error(&self.tabs, self.active_tab);
            cx.notify();
            return;
        };
        let Some(grid) = self.active_grid() else {
            return;
        };
        let Some(row_ix) = grid.read(cx).delegate().selected_row else {
            self.status = "Pick a row first.".into();
            cx.notify();
            return;
        };
        if row_ix >= grid.read(cx).delegate().rows.len() {
            grid.update(cx, |state, cx| {
                state.delegate_mut().pending_insert = None;
                state.delegate_mut().selected_row = None;
                state.refresh(cx);
            });
            cx.notify();
            return;
        }
        let Some(last_seen) = grid.read(cx).delegate().named_row(row_ix) else {
            return;
        };
        let result = self.client.update(cx, |client, cx| {
            let result = client.stage_delete(self.session_id, table, last_seen);
            cx.notify();
            result
        });
        match result {
            Ok(_) => self.status = SharedString::default(),
            Err(err) => self.status = format!("{err}").into(),
        }
        cx.notify();
    }

    fn unstage_one(&mut self, change: StagedChangeId, cx: &mut Context<Self>) {
        let result = self.client.update(cx, |client, cx| {
            let result = client.unstage(self.session_id, change);
            cx.notify();
            result
        });
        if let Err(err) = result {
            self.status = format!("{err}").into();
        }
        cx.notify();
    }

    fn discard_all(&mut self, cx: &mut Context<Self>) {
        let result = self.client.update(cx, |client, cx| {
            let result = client.discard_staged_changes(self.session_id);
            cx.notify();
            result
        });
        if let Err(err) = result {
            self.status = format!("{err}").into();
        }
        cx.notify();
    }

    fn apply_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let result = self.client.update(cx, |client, cx| {
            let result = client.apply(self.session_id);
            cx.notify();
            result
        });
        match result {
            Ok(()) => {
                self.status = SharedString::default();
                self.reload_all_grids(window, cx);
                true
            }
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
                false
            }
        }
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_tab;
        let Some(kind) = self.tabs.get(active).map(|tab| tab.kind.clone()) else {
            return;
        };

        let loaded = match kind {
            TabKind::Table {
                table,
                filters,
                page,
                ..
            } => self
                .client
                .read(cx)
                .table_page(self.session_id, &table, &filters, page)
                .map(|page_data| (page_data, true, None))
                .map_err(|error| format!("{error}")),
            TabKind::Query { sql, page, .. } => self
                .client
                .read(cx)
                .query(self.session_id, &sql, page)
                .map(|result| {
                    let editable = matches!(result.staging(), ResultStaging::Staged { .. });
                    (
                        result.page().clone(),
                        editable,
                        Some(result.staging().clone()),
                    )
                })
                .map_err(|error| format!("{error}")),
            TabKind::Structure { .. } => {
                cx.notify();
                return;
            }
        };

        match loaded {
            Err(err) => self.status = err.into(),
            Ok((page_data, editable, staging)) => {
                let mut grid = match self.tabs.get_mut(active).map(|tab| &mut tab.kind) {
                    Some(TabKind::Table { has_next, grid, .. }) => {
                        *has_next = page_data.has_next();
                        grid.take()
                    }
                    Some(TabKind::Query {
                        has_next,
                        grid,
                        staging: tab_staging,
                        ..
                    }) => {
                        *has_next = page_data.has_next();
                        if let Some(staging) = staging {
                            *tab_staging = staging;
                        }
                        grid.take()
                    }
                    _ => None,
                };
                self.show_page(page_data, editable, &mut grid, window, cx);
                if let Some(tab) = self.tabs.get_mut(active) {
                    match &mut tab.kind {
                        TabKind::Table { grid: slot, .. } | TabKind::Query { grid: slot, .. } => {
                            *slot = grid;
                        }
                        TabKind::Structure { .. } => {}
                    }
                }
            }
        }
        cx.notify();
    }

    fn show_page(
        &mut self,
        page: TablePage,
        editable: bool,
        grid: &mut Option<Entity<TableState<PageGrid>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delegate = PageGrid::from_page(&page, self.edit_input.clone(), editable);
        match grid {
            Some(state) => {
                state.update(cx, |state, cx| {
                    *state.delegate_mut() = delegate;
                    state.refresh(cx);
                    cx.notify();
                });
            }
            None => {
                *grid = Some(cx.new(|cx| TableState::new(delegate, window, cx).sortable(false)));
            }
        }
        self.status = SharedString::default();
    }

    fn confirm_schema_change(
        &mut self,
        change: SchemaChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ddl = match self.client.read(cx).schema_change_ddl(self.session_id, &change) {
            Ok(ddl) => ddl,
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
                return;
            }
        };
        let session = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Schema Change")
                .child(ddl.clone())
                .footer(
                    DialogFooter::new()
                        .child(Button::new("run-schema").primary().label("Run").on_click({
                            let session = session.clone();
                            let change = change.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        this.run_schema_change(change.clone(), window, cx);
                                        window.close_dialog(cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel-schema").label("Cancel"))),
                )
        });
    }

    fn run_schema_change(
        &mut self,
        change: SchemaChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.client.update(cx, |client, cx| {
            let result = client.execute_schema_change(self.session_id, &change);
            cx.notify();
            result
        });
        match result {
            Ok(()) => {
                self.status = SharedString::default();
                self.reload_all_grids(window, cx);
                cx.notify();
            }
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
            }
        }
    }

    fn open_create_table_form(
        &mut self,
        namespace: Namespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        let table_input = self.schema_table_input.clone();
        let columns_input = self.schema_columns_input.clone();
        let namespace_label = namespace.as_str().to_string();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Create Table")
                .child(format!(
                    "Table lands in the {namespace_label} Namespace."
                ))
                .child(Input::new(&table_input))
                .child("One column per line: name TYPE")
                .child(Input::new(&columns_input))
                .footer(
                    DialogFooter::new()
                        .child(Button::new("preview-create").primary().label("Preview DDL").on_click({
                            let session = session.clone();
                            let table_input = table_input.clone();
                            let columns_input = columns_input.clone();
                            let namespace = namespace.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        let name = table_input.read(cx).value().trim().to_string();
                                        let columns =
                                            parse_column_lines(&columns_input.read(cx).value());
                                        if name.is_empty() || columns.is_empty() {
                                            this.status =
                                                "Table name and at least one column are required."
                                                    .into();
                                            cx.notify();
                                            return;
                                        }
                                        let change = SchemaChange::CreateTable {
                                            namespace: namespace.clone(),
                                            table: TableName::new(name),
                                            columns,
                                        };
                                        window.close_dialog(cx);
                                        this.confirm_schema_change(change, window, cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel-create").label("Cancel"))),
                )
        });
    }

    fn open_add_column_form(
        &mut self,
        table: Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        let name_input = self.schema_column_name_input.clone();
        let type_input = self.schema_column_type_input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Add column to {}", table_label(&table)))
                .child(Input::new(&name_input))
                .child(Input::new(&type_input))
                .footer(
                    DialogFooter::new()
                        .child(Button::new("preview-add-column").primary().label("Preview DDL").on_click({
                            let session = session.clone();
                            let table = table.clone();
                            let name_input = name_input.clone();
                            let type_input = type_input.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        let name = name_input.read(cx).value().trim().to_string();
                                        let type_sql = type_input.read(cx).value().trim().to_string();
                                        if name.is_empty() || type_sql.is_empty() {
                                            this.status =
                                                "Column name and type are required.".into();
                                            cx.notify();
                                            return;
                                        }
                                        let change = SchemaChange::AddColumn {
                                            table: table.clone(),
                                            column: ColumnDefinition::new(name, type_sql),
                                        };
                                        window.close_dialog(cx);
                                        this.confirm_schema_change(change, window, cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel-add-column").label("Cancel"))),
                )
        });
    }

    fn open_rename_column_form(
        &mut self,
        table: Table,
        from: ColumnName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.schema_rename_to_input.update(cx, |input, cx| {
            input.set_value(String::new(), window, cx);
        });
        let session = cx.entity().downgrade();
        let rename_input = self.schema_rename_to_input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!(
                    "Rename column {} on {}",
                    from.as_str(),
                    table_label(&table)
                ))
                .child(Input::new(&rename_input))
                .footer(
                    DialogFooter::new()
                        .child(Button::new("preview-rename").primary().label("Preview DDL").on_click({
                            let session = session.clone();
                            let table = table.clone();
                            let from = from.clone();
                            let rename_input = rename_input.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        let to = rename_input.read(cx).value().trim().to_string();
                                        if to.is_empty() {
                                            this.status = "New column name is required.".into();
                                            cx.notify();
                                            return;
                                        }
                                        let change = SchemaChange::RenameColumn {
                                            table: table.clone(),
                                            from: from.clone(),
                                            to: ColumnName::from_name(to),
                                        };
                                        window.close_dialog(cx);
                                        this.confirm_schema_change(change, window, cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel-rename").label("Cancel"))),
                )
        });
    }

    fn open_add_index_form(
        &mut self,
        table: Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        let name_input = self.schema_index_name_input.clone();
        let columns_input = self.schema_index_columns_input.clone();
        let unique = self.schema_unique_index;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Add index on {}", table_label(&table)))
                .child(Input::new(&name_input))
                .child(Input::new(&columns_input))
                .child(
                    Button::new("toggle-unique")
                        .label(if unique { "Unique" } else { "Not unique" })
                        .when(unique, |button| button.primary())
                        .on_click({
                            let session = session.clone();
                            let table = table.clone();
                            move |_, window, cx| {
                                session
                                    .update(cx, |this, cx| {
                                        this.schema_unique_index = !this.schema_unique_index;
                                        window.close_dialog(cx);
                                        this.open_add_index_form(table.clone(), window, cx);
                                    })
                                    .ok();
                            }
                        }),
                )
                .footer(
                    DialogFooter::new()
                        .child(Button::new("preview-add-index").primary().label("Preview DDL").on_click({
                            let session = session.clone();
                            let table = table.clone();
                            let name_input = name_input.clone();
                            let columns_input = columns_input.clone();
                            move |_, window, cx| {
                                let table = table.clone();
                                session
                                    .update(cx, |this, cx| {
                                        let name = name_input.read(cx).value().trim().to_string();
                                        let columns =
                                            parse_column_names(&columns_input.read(cx).value());
                                        if name.is_empty() || columns.is_empty() {
                                            this.status =
                                                "Index name and at least one column are required."
                                                    .into();
                                            cx.notify();
                                            return;
                                        }
                                        let change = SchemaChange::AddIndex {
                                            name,
                                            table,
                                            columns,
                                            unique: this.schema_unique_index,
                                        };
                                        window.close_dialog(cx);
                                        this.confirm_schema_change(change, window, cx);
                                    })
                                    .ok();
                            }
                        }))
                        .child(DialogClose::new().child(Button::new("cancel-add-index").label("Cancel"))),
                )
        });
    }

    fn render_structure(
        &mut self,
        table: &Table,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let table_for_drop = table.clone();
        match self
            .client
            .read(cx)
            .table_structure(self.session_id, table)
        {
            Ok(structure) => {
                let mut columns = v_flex().gap_1().child("Columns");
                for (column_ix, column) in structure.columns().iter().enumerate() {
                    let mut parts = vec![
                        column.name().as_str().to_string(),
                        column.type_name().to_string(),
                    ];
                    if column.not_null() {
                        parts.push("NOT NULL".into());
                    }
                    if column.primary_key() {
                        parts.push("PRIMARY KEY".into());
                    }
                    let column_name = column.name().clone();
                    let table_for_column = table.clone();
                    columns = columns.child(
                        h_flex().gap_2().child(parts.join(" ")).child(
                            Button::new(("drop-column", column_ix as u64))
                                .label("Drop")
                                .on_click(cx.listener({
                                    let table_for_column = table_for_column.clone();
                                    let column_name = column_name.clone();
                                    move |this, _, window, cx| {
                                        this.confirm_schema_change(
                                            SchemaChange::DropColumn {
                                                table: table_for_column.clone(),
                                                column: column_name.clone(),
                                            },
                                            window,
                                            cx,
                                        );
                                    }
                                })),
                        ).child(
                            Button::new(("rename-column", column_ix as u64))
                                .label("Rename")
                                .on_click(cx.listener({
                                    let table_for_column = table_for_column.clone();
                                    let column_name = column_name.clone();
                                    move |this, _, window, cx| {
                                        this.open_rename_column_form(
                                            table_for_column.clone(),
                                            column_name.clone(),
                                            window,
                                            cx,
                                        );
                                    }
                                })),
                        ),
                    );
                }
                let table_for_index = table.clone();
                let mut indexes = v_flex().gap_1().child("Indexes");
                if structure.indexes().is_empty() {
                    indexes = indexes.child("No indexes.");
                } else {
                    for (index_ix, index) in structure.indexes().iter().enumerate() {
                        let unique = if index.unique() { "UNIQUE " } else { "" };
                        let index_name = index.name().to_string();
                        let table_for_drop_index = table_for_index.clone();
                        indexes = indexes.child(
                            h_flex().gap_2().child(format!(
                                "{unique}{} ({})",
                                index.name(),
                                index.columns().join(", ")
                            )).child(
                                Button::new(("drop-index", index_ix as u64))
                                    .label("Drop")
                                    .on_click(cx.listener({
                                        let index_name = index_name.clone();
                                        let table = table_for_drop_index.clone();
                                        move |this, _, window, cx| {
                                            this.confirm_schema_change(
                                                SchemaChange::DropIndex {
                                                    name: index_name.clone(),
                                                    table: table.clone(),
                                                },
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                        );
                    }
                }
                let table_for_add = table.clone();
                let schema_actions = h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("add-column")
                            .label("Add column")
                            .on_click(cx.listener({
                                let table_for_add = table_for_add.clone();
                                move |this, _, window, cx| {
                                    this.open_add_column_form(table_for_add.clone(), window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("add-index")
                            .label("Add index")
                            .on_click(cx.listener({
                                let table_for_add = table_for_add.clone();
                                move |this, _, window, cx| {
                                    this.open_add_index_form(table_for_add.clone(), window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("drop-table")
                            .label("Drop table")
                            .on_click(cx.listener({
                                let table_for_drop = table_for_drop.clone();
                                move |this, _, window, cx| {
                                    this.confirm_schema_change(
                                        SchemaChange::DropTable {
                                            table: table_for_drop.clone(),
                                        },
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    );
                v_flex()
                    .gap_3()
                    .child(schema_actions)
                    .child(columns)
                    .child(indexes)
            }
            Err(err) => v_flex().child(format!("{err}")),
        }
    }

    fn render_active_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return v_flex()
                .flex_1()
                .p_4()
                .text_color(cx.theme().muted_foreground)
                .child("Pick a Table, open Structure, or start a Query.")
                .into_any_element();
        };
        match tab.kind.clone() {
            TabKind::Structure { table } => self
                .render_structure(&table, window, cx)
                .into_any_element(),
            TabKind::Query { editor, .. } => {
                let grid = self.active_grid();
                let (has_next, page, row_count) = match &self.tabs[self.active_tab].kind {
                    TabKind::Query {
                        has_next,
                        page,
                        grid,
                        ..
                    } => (
                        *has_next,
                        *page,
                        grid.as_ref()
                            .map(|state| state.read(cx).delegate().rows.len())
                            .unwrap_or(0),
                    ),
                    _ => (false, Page::first(), 0),
                };
                let can_stage = self.active_table().is_some();
                let grid_element = match grid {
                    Some(state) => v_flex()
                        .flex_1()
                        .min_h(px(160.))
                        .child(DataTable::new(&state).stripe(true)),
                    None => v_flex()
                        .flex_1()
                        .p_3()
                        .text_color(cx.theme().muted_foreground)
                        .child("Run a Query to see Results."),
                };
                v_flex()
                    .flex_1()
                    .gap_2()
                    .p_2()
                    .child(Editor::new(&editor).h(px(140.)))
                    .child(
                        h_flex().gap_2().child(
                            Button::new("run-sql").primary().label("Run").on_click(
                                cx.listener(|this, _, window, cx| this.run_sql(window, cx)),
                            ),
                        ),
                    )
                    .child(grid_element)
                    .child(self.render_pagination_bar(page, has_next, row_count, can_stage, cx))
                    .into_any_element()
            }
            TabKind::Table {
                filters, has_next, page, grid, ..
            } => {
                let can_stage = self.active_table().is_some();
                let row_count = grid
                    .as_ref()
                    .map(|state| state.read(cx).delegate().rows.len())
                    .unwrap_or(0);
                let mut filter_bar = h_flex().gap_2().px_2().py_1().flex_wrap().items_center();
                for (index, filter) in filters.iter().enumerate() {
                    filter_bar = filter_bar.child(
                        h_flex()
                            .id(("filter-chip", index as u64))
                            .gap_1()
                            .px_2()
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(cx.theme().secondary)
                            .border_1()
                            .border_color(cx.theme().border)
                            .items_center()
                            .child(div().text_xs().child(filter_label(filter)))
                            .child(
                                Button::new(("remove-filter", index as u64))
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.remove_filter(index, window, cx);
                                    })),
                            ),
                    );
                }
                filter_bar = filter_bar.child(
                    Button::new("add-filter")
                        .label("+ Filter")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_filter_dialog(window, cx);
                        })),
                );
                let grid_element = match self.active_grid() {
                    Some(state) => v_flex()
                        .flex_1()
                        .min_h(px(160.))
                        .child(DataTable::new(&state).stripe(true)),
                    None => v_flex()
                        .flex_1()
                        .p_3()
                        .text_color(cx.theme().muted_foreground)
                        .child("Loading…"),
                };
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(filter_bar)
                    .child(grid_element)
                    .child(self.render_pagination_bar(page, has_next, row_count, can_stage, cx))
                    .into_any_element()
            }
        }
    }

    fn render_pagination_bar(
        &self,
        page: Page,
        has_next: bool,
        row_count: usize,
        can_stage: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let page_number = page.index() + 1;
        let start = if row_count == 0 {
            0
        } else {
            page.index() * TABLE_PAGE_SIZE + 1
        };
        let end = page.index() * TABLE_PAGE_SIZE + row_count;
        let range_label = if row_count == 0 {
            "No rows".to_string()
        } else if has_next {
            format!("{start} – {end}+")
        } else {
            format!("{start} – {end}")
        };
        let can_prev = page.index() > 0;
        h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("first-page")
                    .label("«")
                    .ghost()
                    .xsmall()
                    .disabled(!can_prev)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(TabKind::Table { page, .. } | TabKind::Query { page, .. }) =
                            this.tabs.get_mut(this.active_tab).map(|tab| &mut tab.kind)
                        {
                            *page = Page::first();
                        }
                        this.reload(window, cx);
                    })),
            )
            .child(
                Button::new("prev-page")
                    .label("‹")
                    .ghost()
                    .xsmall()
                    .disabled(!can_prev)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.prev_page(window, cx);
                    })),
            )
            .child(
                Button::new("next-page")
                    .label("›")
                    .ghost()
                    .xsmall()
                    .disabled(!has_next)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.next_page(window, cx);
                    })),
            )
            .child(
                Button::new("page-label")
                    .label(format!("Page {page_number}"))
                    .ghost()
                    .xsmall()
                    .disabled(true),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{TABLE_PAGE_SIZE} rows")),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(range_label),
            )
            .child(
                {
                    let view = cx.entity().downgrade();
                    Button::new("grid-more")
                        .icon(IconName::Ellipsis)
                        .ghost()
                        .xsmall()
                        .disabled(!can_stage)
                        .dropdown_menu(move |menu, _, _| {
                            menu.item(PopupMenuItem::new("Insert row").on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| this.start_insert_row(cx))
                                        .ok();
                                }
                            }))
                            .item(PopupMenuItem::new("Stage insert").on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| this.stage_insert_row(cx))
                                        .ok();
                                }
                            }))
                            .item(PopupMenuItem::new("Stage delete").on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| this.stage_delete_row(cx))
                                        .ok();
                                }
                            }))
                        })
                },
            )
    }
}

impl Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, catalog, staged, staged_count) = {
            let client = self.client.read(cx);
            let shown = client.show_system_catalogs() == SystemCatalogPreference::Shown;
            let catalog = client
                .tables(self.session_id)
                .map_err(|err| format!("{err}"));
            let staged = client
                .staged_changes(self.session_id)
                .map(|changes| {
                    changes
                        .iter()
                        .map(|change| (change.id(), change_kind_icon(change), change_label(change)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let staged_count = staged.len();
            (shown, catalog, staged, staged_count)
        };

        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let secondary = cx.theme().secondary;
        let foreground = cx.theme().foreground;

        let mut tree = v_flex().id("table-tree").flex_1().w_full().gap_0().overflow_y_scroll();
        match &catalog {
            Ok(catalog) if catalog.tables().is_empty() => {
                tree = tree.child(
                    div()
                        .p_3()
                        .text_color(muted)
                        .child("No Tables."),
                );
            }
            Ok(catalog) => {
                if let Some(groups) = catalog.namespace_groups() {
                    for (group_index, group) in groups.iter().enumerate() {
                        let namespace = group.namespace().clone();
                        let ns_key = namespace.as_str().to_string();
                        let collapsed = self.collapsed_namespaces.contains(&ns_key);
                        let visible_tables: Vec<_> = group
                            .tables()
                            .iter()
                            .filter(|table| self.table_matches_search(table, cx))
                            .cloned()
                            .collect();
                        if visible_tables.is_empty()
                            && !self.table_search.read(cx).value().trim().is_empty()
                        {
                            continue;
                        }
                        let view = cx.entity().downgrade();
                        tree = tree.child(
                            div()
                                .id(("namespace", group_index as u64))
                                .w_full()
                                .px_2()
                                .py_1()
                                .cursor_pointer()
                                .hover(|style| style.bg(secondary))
                                .on_click(cx.listener({
                                    let ns_key = ns_key.clone();
                                    move |this, _, _, cx| {
                                        this.toggle_namespace(&ns_key, cx);
                                    }
                                }))
                                .context_menu({
                                    let view = view.clone();
                                    let namespace = namespace.clone();
                                    move |menu, _, _| {
                                        menu.item(PopupMenuItem::new("Create Table").on_click({
                                            let view = view.clone();
                                            let namespace = namespace.clone();
                                            move |_, window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.open_create_table_form(
                                                        namespace.clone(),
                                                        window,
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                            }
                                        }))
                                    }
                                })
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(if collapsed { "▸" } else { "▾" }),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(ns_key.clone()),
                                        ),
                                ),
                        );
                        if !collapsed {
                            for (index, table) in visible_tables.into_iter().enumerate() {
                                tree = tree.child(table_tree_row(
                                    ((group_index as u64) << 16) | index as u64,
                                    table,
                                    true,
                                    cx,
                                ));
                            }
                        }
                    }
                } else {
                    let tables: Vec<_> = catalog
                        .tables()
                        .iter()
                        .filter(|table| self.table_matches_search(table, cx))
                        .cloned()
                        .collect();
                    if tables.is_empty() {
                        tree = tree.child(
                            div()
                                .p_3()
                                .text_color(muted)
                                .child("No Tables match this search."),
                        );
                    }
                    for (index, table) in tables.into_iter().enumerate() {
                        tree = tree.child(table_tree_row(index as u64, table, false, cx));
                    }
                }
            }
            Err(err) => {
                tree = tree.child(div().p_3().text_color(cx.theme().danger).child(err.clone()));
            }
        }

        let sidebar = v_flex()
            .w(px(220.))
            .h_full()
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Input::new(&self.table_search).prefix(IconName::Search)),
            )
            .child(tree)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Checkbox::new("system-catalogs")
                            .label("Show System Catalogs")
                            .checked(shown)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_system_catalogs(cx);
                            })),
                    ),
            );

        let mut staged_rows = v_flex()
            .id("staged-list")
            .flex_1()
            .w_full()
            .gap_1()
            .p_2()
            .overflow_y_scroll();
        if staged.is_empty() {
            staged_rows = staged_rows.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("No Staged Changes."),
            );
        } else {
            for (index, (change_id, icon, label)) in staged.into_iter().enumerate() {
                staged_rows = staged_rows.child(
                    h_flex()
                        .id(("staged", index as u64))
                        .w_full()
                        .gap_2()
                        .items_center()
                        .px_1()
                        .py_1()
                        .rounded(px(4.))
                        .hover(|style| style.bg(secondary))
                        .child(
                            div()
                                .w(px(18.))
                                .text_xs()
                                .text_color(match icon {
                                    'u' => rgb(0xca8a04),
                                    'i' => rgb(0x16a34a),
                                    _ => rgb(0xdc2626),
                                })
                                .child(match icon {
                                    'u' => "✎",
                                    'i' => "+",
                                    _ => "⌫",
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(foreground)
                                .child(label),
                        )
                        .child(
                            Button::new(("unstage", index as u64))
                                .label("Unstage")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.unstage_one(change_id, cx);
                                })),
                        ),
                );
            }
        }

        let staged_pane = v_flex()
            .w(px(240.))
            .h_full()
            .border_l_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Staged Changes"),
                    )
                    .child(
                        div()
                            .px(px(6.))
                            .rounded(px(999.))
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .text_xs()
                            .child(format!("{staged_count}")),
                    ),
            )
            .child(staged_rows)
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Button::new("apply")
                            .primary()
                            .w_full()
                            .label(format!("Apply ({staged_count})"))
                            .disabled(staged_count == 0)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.apply_changes(window, cx);
                            })),
                    )
                    .child(
                        Button::new("discard-all")
                            .w_full()
                            .label("Discard all")
                            .disabled(staged_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discard_all(cx);
                            })),
                    ),
            );

        let tab_labels: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| self.tab_label(tab, cx))
            .collect();

        let mut tab_bar = TabBar::new("session-tabs")
            .menu(true)
            .selected_index(self.active_tab)
            .suffix(
                Button::new("new-tab")
                    .icon(IconName::Plus)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_query_tab(window, cx);
                    })),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                this.select_tab(*index, cx);
            }));

        for (index, label) in tab_labels.iter().enumerate() {
            let close_index = index;
            tab_bar = tab_bar.child(
                Tab::new().label(label.clone()).suffix(
                    Button::new(("close-tab", index as u64))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab(close_index, cx);
                        })),
                ),
            );
        }

        let (page_rows, page_index) = match self.tabs.get(self.active_tab).map(|tab| &tab.kind) {
            Some(TabKind::Table { page, grid, .. }) | Some(TabKind::Query { page, grid, .. }) => (
                grid.as_ref()
                    .map(|state| state.read(cx).delegate().rows.len())
                    .unwrap_or(0),
                page.index() + 1,
            ),
            _ => (0, 1),
        };
        let status_left = if self.status.is_empty() {
            format!("{page_rows} rows · page {page_index} · {staged_count} Staged Changes")
                .into()
        } else {
            self.status.clone()
        };

        let center = v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .border_b_1()
                    .border_color(border)
                    .child(tab_bar),
            )
            .child(self.render_active_tab(window, cx));

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(foreground)
            .child(h_flex().flex_1().w_full().min_h_0().child(sidebar).child(center).child(staged_pane))
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .child(div().text_xs().text_color(muted).child(status_left)),
            )
    }
}

fn active_tab_stage_error(tabs: &[SessionTab], active_tab: usize) -> SharedString {
    match tabs.get(active_tab).map(|tab| &tab.kind) {
        Some(TabKind::Query { staging, .. }) if *staging == ResultStaging::ReadOnly => {
            "This Result is read-only.".into()
        }
        Some(TabKind::Structure { .. }) => "Open a Table or staged Result Tab first.".into(),
        _ => "Open a Table or staged Result Tab first.".into(),
    }
}

fn table_label(table: &Table) -> String {
    match table.namespace() {
        Some(namespace) => format!("{}.{}", namespace.as_str(), table.name().as_str()),
        None => table.name().as_str().to_string(),
    }
}

fn table_tree_row(
    index: u64,
    table: Table,
    indented: bool,
    cx: &mut Context<SessionView>,
) -> impl IntoElement {
    let for_structure = table.clone();
    let name = table.name().as_str().to_string();
    let secondary = cx.theme().secondary;
    let view = cx.entity().downgrade();
    div()
        .id(("table", index))
        .w_full()
        .px_2()
        .when(indented, |row| row.pl_6())
        .py_1()
        .cursor_pointer()
        .hover(move |style| style.bg(secondary))
        .on_click(cx.listener({
            let table = table.clone();
            move |this, _, window, cx| {
                this.add_table_tab(table.clone(), window, cx);
            }
        }))
        .context_menu({
            let view = view.clone();
            let table = table.clone();
            let for_structure = for_structure.clone();
            move |menu, _, _| {
                menu.item(PopupMenuItem::new("Open").on_click({
                    let view = view.clone();
                    let table = table.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.add_table_tab(table.clone(), window, cx);
                        })
                        .ok();
                    }
                }))
                .item(PopupMenuItem::new("Structure").on_click({
                    let view = view.clone();
                    let for_structure = for_structure.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.add_structure_tab(for_structure.clone(), window, cx);
                        })
                        .ok();
                    }
                }))
                .item(PopupMenuItem::new("Create Table").on_click({
                    let view = view.clone();
                    let namespace = table
                        .namespace()
                        .cloned()
                        .unwrap_or_else(Namespace::main);
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.open_create_table_form(namespace.clone(), window, cx);
                        })
                        .ok();
                    }
                }))
            }
        })
        .child(div().text_sm().child(name))
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max_chars).collect::<String>())
    }
}

fn parse_column_lines(text: &str) -> Vec<ColumnDefinition> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let name = parts.next()?.trim();
            let type_sql = parts.next()?.trim();
            if name.is_empty() || type_sql.is_empty() {
                return None;
            }
            Some(ColumnDefinition::new(name, type_sql))
        })
        .collect()
}

fn parse_column_names(text: &str) -> Vec<ColumnName> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ColumnName::from_name)
        .collect()
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

fn cell_edit_text(cell: &Cell) -> String {
    match cell {
        Cell::Null => String::new(),
        Cell::Integer(value) => value.to_string(),
        Cell::Real(value) => value.to_string(),
        Cell::Text(value) => value.clone(),
        Cell::Blob(_) => String::new(),
    }
}

fn cell_from_input(original: &Cell, text: &str) -> Cell {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Cell::Null;
    }
    match original {
        Cell::Integer(_) => trimmed
            .parse::<i64>()
            .map(Cell::Integer)
            .unwrap_or_else(|_| Cell::Text(trimmed.to_string())),
        Cell::Real(_) => trimmed
            .parse::<f64>()
            .map(Cell::Real)
            .unwrap_or_else(|_| Cell::Text(trimmed.to_string())),
        Cell::Blob(bytes) => Cell::Blob(bytes.clone()),
        Cell::Null | Cell::Text(_) => trimmed
            .parse::<i64>()
            .map(Cell::Integer)
            .unwrap_or_else(|_| Cell::Text(trimmed.to_string())),
    }
}

fn change_kind_icon(change: &StagedChange) -> char {
    match change {
        StagedChange::Insert { .. } => 'i',
        StagedChange::Update { .. } => 'u',
        StagedChange::Delete { .. } => 'd',
    }
}

fn change_label(change: &StagedChange) -> String {
    match change {
        StagedChange::Insert { table, .. } => {
            format!("insert {}", table.name().as_str())
        }
        StagedChange::Update {
            table, identity, ..
        } => format!(
            "update {} · {}",
            table.name().as_str(),
            identity_label(identity)
        ),
        StagedChange::Delete {
            table, identity, ..
        } => format!(
            "delete {} · {}",
            table.name().as_str(),
            identity_label(identity)
        ),
    }
}

fn identity_label(identity: &RowIdentity) -> String {
    identity
        .columns()
        .iter()
        .map(|(column, cell)| format!("{} {}", column.as_str(), cell_label(cell)))
        .collect::<Vec<_>>()
        .join(", ")
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
