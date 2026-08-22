use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme, Disableable, WindowExt, h_flex, v_flex};
use opendb::{
    Cell, Client, ColumnDefinition, ColumnName, Filter, Namespace, Page, ResultStaging,
    SchemaChange, SessionId, SqlKind, StagedChange, StagedChangeId, SystemCatalogPreference,
    TableName, TablePage,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DraftKind {
    Equals,
    Contains,
    IsNull,
}

#[derive(Clone, PartialEq, Eq)]
enum GridSource {
    Table,
    Query { sql: String, staging: ResultStaging },
}

struct PageGrid {
    columns: Vec<Column>,
    column_names: Vec<ColumnName>,
    rows: Vec<Vec<Cell>>,
    selected_row: Option<usize>,
    editing: Option<(usize, usize)>,
    insert_draft: Option<Vec<Cell>>,
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
            insert_draft: None,
            edit_input,
            editable,
        }
    }

    fn row_count(&self) -> usize {
        self.rows.len() + usize::from(self.insert_draft.is_some())
    }

    fn row_cells(&self, row_ix: usize) -> Option<&[Cell]> {
        if row_ix < self.rows.len() {
            self.rows.get(row_ix).map(|row| row.as_slice())
        } else {
            self.insert_draft.as_deref()
        }
    }

    fn row_cells_mut(&mut self, row_ix: usize) -> Option<&mut Vec<Cell>> {
        if row_ix < self.rows.len() {
            self.rows.get_mut(row_ix)
        } else {
            self.insert_draft.as_mut()
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

    fn start_insert_draft(&mut self) {
        let width = self.column_names.len();
        self.insert_draft = Some(vec![Cell::Null; width]);
        self.selected_row = Some(self.rows.len());
        self.editing = None;
    }

    fn take_insert_values(&mut self) -> Vec<(ColumnName, Cell)> {
        let Some(draft) = self.insert_draft.take() else {
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
    selected: Option<TableName>,
    filters: Vec<Filter>,
    page: Page,
    has_next: bool,
    grid: Option<Entity<TableState<PageGrid>>>,
    column_input: Entity<InputState>,
    value_input: Entity<InputState>,
    edit_input: Entity<InputState>,
    query_editor: Entity<EditorState>,
    schema_table_input: Entity<InputState>,
    schema_columns_input: Entity<InputState>,
    schema_column_name_input: Entity<InputState>,
    schema_column_type_input: Entity<InputState>,
    schema_rename_to_input: Entity<InputState>,
    schema_index_name_input: Entity<InputState>,
    schema_index_columns_input: Entity<InputState>,
    schema_unique_index: bool,
    grid_source: GridSource,
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
        let edit_input = cx.new(|cx| InputState::new(window, cx));
        let query_editor = cx.new(|cx| EditorState::new(window, cx).placeholder("SQL"));
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
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            entity
                .update(cx, |this, cx| this.on_should_close(window, cx))
                .unwrap_or(true)
        });
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
            edit_input,
            query_editor,
            schema_table_input,
            schema_columns_input,
            schema_column_name_input,
            schema_column_type_input,
            schema_rename_to_input,
            schema_index_name_input,
            schema_index_columns_input,
            schema_unique_index: false,
            grid_source: GridSource::Table,
            draft_kind: DraftKind::Equals,
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

    fn open_table(&mut self, table: TableName, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(table);
        self.filters.clear();
        self.page = Page::first();
        self.grid_source = GridSource::Table;
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
        self.grid_source = GridSource::Table;
        self.reload(window, cx);
    }

    fn remove_filter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.filters.len() {
            self.filters.remove(index);
            self.page = Page::first();
            self.grid_source = GridSource::Table;
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

    fn active_table(&self) -> Option<TableName> {
        match &self.grid_source {
            GridSource::Table => self.selected.clone(),
            GridSource::Query {
                staging: ResultStaging::Staged { table },
                ..
            } => Some(table.clone()),
            GridSource::Query { .. } => None,
        }
    }

    fn run_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.query_editor.read(cx).value().to_string();
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
        self.grid_source = GridSource::Query {
            sql,
            staging: ResultStaging::ReadOnly,
        };
        self.page = Page::first();
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
                self.reload(window, cx);
            }
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
            }
        }
    }

    fn commit_cell(&mut self, cx: &mut Context<Self>) {
        let Some(grid) = self.grid.clone() else {
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
            self.status = match self.grid_source {
                GridSource::Query { .. } => "This Result is read-only.".into(),
                GridSource::Table => "Pick a Table first.".into(),
            };
            cx.notify();
            return;
        };
        let Some(grid) = self.grid.clone() else {
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
            self.status = match self.grid_source {
                GridSource::Query { .. } => "This Result is read-only.".into(),
                GridSource::Table => "Pick a Table first.".into(),
            };
            cx.notify();
            return;
        }
        let Some(grid) = &self.grid else {
            self.status = "Pick a Table first.".into();
            cx.notify();
            return;
        };
        grid.update(cx, |state, cx| {
            state.delegate_mut().start_insert_draft();
            state.refresh(cx);
        });
        cx.notify();
    }

    fn stage_delete_row(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.active_table() else {
            self.status = match self.grid_source {
                GridSource::Query { .. } => "This Result is read-only.".into(),
                GridSource::Table => "Pick a Table first.".into(),
            };
            cx.notify();
            return;
        };
        let Some(grid) = self.grid.clone() else {
            return;
        };
        let Some(row_ix) = grid.read(cx).delegate().selected_row else {
            self.status = "Pick a row first.".into();
            cx.notify();
            return;
        };
        if row_ix >= grid.read(cx).delegate().rows.len() {
            grid.update(cx, |state, cx| {
                state.delegate_mut().insert_draft = None;
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
                self.reload(window, cx);
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
        match self.grid_source.clone() {
            GridSource::Table => {
                let Some(table) = self.selected.clone() else {
                    return;
                };
                match self.client.read(cx).table_page(
                    self.session_id,
                    &table,
                    &self.filters,
                    self.page,
                ) {
                    Ok(page) => self.show_page(&page, true, window, cx),
                    Err(err) => self.status = format!("{err}").into(),
                }
            }
            GridSource::Query { sql, .. } => {
                match self.client.read(cx).query(self.session_id, &sql, self.page) {
                    Ok(result) => {
                        let editable = matches!(result.staging(), ResultStaging::Staged { .. });
                        self.grid_source = GridSource::Query {
                            sql,
                            staging: result.staging().clone(),
                        };
                        self.show_page(result.page(), editable, window, cx);
                    }
                    Err(err) => self.status = format!("{err}").into(),
                }
            }
        }
        cx.notify();
    }

    fn show_page(
        &mut self,
        page: &TablePage,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.has_next = page.has_next();
        let delegate = PageGrid::from_page(page, self.edit_input.clone(), editable);
        match &self.grid {
            Some(state) => {
                state.update(cx, |state, cx| {
                    *state.delegate_mut() = delegate;
                    state.refresh(cx);
                    cx.notify();
                });
            }
            None => {
                self.grid =
                    Some(cx.new(|cx| TableState::new(delegate, window, cx).sortable(false)));
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
        let ddl = self.client.read(cx).schema_change_ddl(&change);
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
                self.reload(window, cx);
            }
            Err(err) => {
                self.status = format!("{err}").into();
                cx.notify();
            }
        }
    }

    fn open_create_table_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.entity().downgrade();
        let table_input = self.schema_table_input.clone();
        let columns_input = self.schema_columns_input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Create Table")
                .child("Table lands in the main Namespace on SQLite.")
                .child(Input::new(&table_input))
                .child("One column per line: name TYPE")
                .child(Input::new(&columns_input))
                .footer(
                    DialogFooter::new()
                        .child(Button::new("preview-create").primary().label("Preview DDL").on_click({
                            let session = session.clone();
                            let table_input = table_input.clone();
                            let columns_input = columns_input.clone();
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
                                            namespace: Namespace::main(),
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
        table: TableName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        let name_input = self.schema_column_name_input.clone();
        let type_input = self.schema_column_type_input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Add column to {}", table.as_str()))
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
        table: TableName,
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
                    table.as_str()
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
        table: TableName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        let name_input = self.schema_index_name_input.clone();
        let columns_input = self.schema_index_columns_input.clone();
        let unique = self.schema_unique_index;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Add index on {}", table.as_str()))
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
        table: &TableName,
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
                let mut indexes = v_flex().gap_1().child("Indexes");
                if structure.indexes().is_empty() {
                    indexes = indexes.child("No indexes.");
                } else {
                    for (index_ix, index) in structure.indexes().iter().enumerate() {
                        let unique = if index.unique() { "UNIQUE " } else { "" };
                        let index_name = index.name().to_string();
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
                                        move |this, _, window, cx| {
                                            this.confirm_schema_change(
                                                SchemaChange::DropIndex {
                                                    name: index_name.clone(),
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
}

impl Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, title, table_names, staged) = {
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
            let staged = client
                .staged_changes(self.session_id)
                .map(|changes| {
                    changes
                        .iter()
                        .map(|change| (change.id(), change_label(change)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (shown, title, table_names, staged)
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

        let mut staged_rows = v_flex().gap_1();
        for (index, (change_id, label)) in staged.iter().enumerate() {
            let change_id = *change_id;
            staged_rows = staged_rows.child(
                h_flex().gap_2().child(label.clone()).child(
                    Button::new(("unstage", index as u64))
                        .label("Unstage")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.unstage_one(change_id, cx);
                        })),
                ),
            );
        }

        let can_stage = self.active_table().is_some();
        let grid = match &self.grid {
            Some(state) => v_flex()
                .flex_1()
                .min_h(px(240.))
                .child(DataTable::new(state).stripe(true)),
            None => v_flex().flex_1().child("Pick a Table."),
        };

        let schema_panel = if let Some(table) = self.selected.clone() {
            self.render_structure(&table, window, cx).into_any_element()
        } else {
            div().into_any_element()
        };

        v_flex()
            .size_full()
            .p_5()
            .gap_3()
            .bg(cx.theme().background)
            .child(title)
            .child(
                Button::new("create-table")
                    .label("Create Table")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_table_form(window, cx);
                    })),
            )
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
                        .child(Editor::new(&self.query_editor).h(px(120.)))
                        .child(
                            Button::new("run-sql").primary().label("Run").on_click(
                                cx.listener(|this, _, window, cx| this.run_sql(window, cx)),
                            ),
                        )
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
                        .child(schema_panel)
                        .child(grid)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("next-page")
                                        .label("Next page")
                                        .disabled(!self.has_next)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.next_page(window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("insert-row")
                                        .label("Insert row")
                                        .disabled(!can_stage)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.start_insert_row(cx);
                                        })),
                                )
                                .child(
                                    Button::new("stage-insert")
                                        .label("Stage insert")
                                        .disabled(!can_stage)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.stage_insert_row(cx);
                                        })),
                                )
                                .child(
                                    Button::new("stage-delete")
                                        .label("Stage delete")
                                        .disabled(!can_stage)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.stage_delete_row(cx);
                                        })),
                                )
                                .child(Button::new("apply").primary().label("Apply").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.apply_changes(window, cx);
                                    }),
                                ))
                                .child(Button::new("discard").label("Discard").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.discard_all(cx);
                                    }),
                                )),
                        )
                        .child(staged_rows),
                ),
            )
            .child(self.status.clone())
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

fn change_label(change: &StagedChange) -> String {
    match change {
        StagedChange::Insert { table, .. } => format!("Insert into {}", table.as_str()),
        StagedChange::Update { table, .. } => format!("Update {}", table.as_str()),
        StagedChange::Delete { table, .. } => format!("Delete from {}", table.as_str()),
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
