use std::path::PathBuf;

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use url::Url;

use super::{ApplyEngineError, Database, DatabaseError};
use crate::query::{QueryResult, ResultStaging, SqlKind, identity_present, one_table_projection};
use crate::staged::{RowIdentity, StagedChange};
use crate::table::TableName;
use crate::table_page::{Cell, ColumnName, Filter, Page, TABLE_PAGE_SIZE, TablePage};
use crate::table_structure::{StructureColumn, StructureIndex, TableStructure};

pub(crate) struct SqliteDatabase {
    connection: Connection,
}

impl SqliteDatabase {
    pub(crate) fn open(connection_string: &str) -> Result<Self, SqliteOpenError> {
        let path = sqlite_file_path(connection_string);
        if !path.exists() {
            return Err(SqliteOpenError::MissingFile);
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE;
        let connection = Connection::open_with_flags(&path, flags)
            .map_err(|error| SqliteOpenError::Driver(error.to_string()))?;
        Ok(Self { connection })
    }
}

pub(crate) enum SqliteOpenError {
    MissingFile,
    Driver(String),
}

fn sqlite_file_path(raw: &str) -> PathBuf {
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "sqlite" {
            return PathBuf::from(url.path());
        }
    }
    PathBuf::from(raw)
}

fn quote_ident(name: &str) -> String {
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('"');
    for ch in name.chars() {
        if ch == '"' {
            quoted.push_str("\"\"");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('"');
    quoted
}

fn cell_from(value: ValueRef<'_>) -> Cell {
    match value {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(v) => Cell::Integer(v),
        ValueRef::Real(v) => Cell::Real(v),
        ValueRef::Text(v) => Cell::Text(String::from_utf8_lossy(v).into_owned()),
        ValueRef::Blob(v) => Cell::Blob(v.to_vec()),
    }
}

fn value_from_cell(cell: &Cell) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Integer(v) => Value::Integer(*v),
        Cell::Real(v) => Value::Real(*v),
        Cell::Text(v) => Value::Text(v.clone()),
        Cell::Blob(v) => Value::Blob(v.clone()),
    }
}

fn like_contains(value: &str) -> String {
    let mut escaped = String::from("%");
    for ch in value.chars() {
        match ch {
            '%' | '_' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            other => escaped.push(other),
        }
    }
    escaped.push('%');
    escaped
}

fn push_filters(filters: &[Filter], sql: &mut String, params: &mut Vec<Value>) {
    if filters.is_empty() {
        return;
    }
    sql.push_str(" WHERE ");
    for (index, filter) in filters.iter().enumerate() {
        if index > 0 {
            sql.push_str(" AND ");
        }
        match filter {
            Filter::Equals { column, value } => match value {
                Cell::Null => {
                    sql.push_str(&quote_ident(column.as_str()));
                    sql.push_str(" IS NULL");
                }
                other => {
                    sql.push_str(&quote_ident(column.as_str()));
                    sql.push_str(" = ?");
                    params.push(value_from_cell(other));
                }
            },
            Filter::Contains { column, value } => {
                sql.push_str("CAST(");
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" AS TEXT) LIKE ? ESCAPE '\\'");
                params.push(Value::Text(like_contains(value)));
            }
            Filter::IsNull { column } => {
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" IS NULL");
            }
        }
    }
}

impl Database for SqliteDatabase {
    fn list_table_names(&self) -> Result<Vec<TableName>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type IN ('table', 'view') AND name IS NOT NULL",
            )
            .map_err(DatabaseError::from_engine)?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(DatabaseError::from_engine)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_engine)?;
        Ok(names.into_iter().map(TableName::new).collect())
    }

    fn table_page(
        &self,
        table: &TableName,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, DatabaseError> {
        let order_names = {
            let pragma = format!("PRAGMA table_info({})", quote_ident(table.as_str()));
            let mut statement = self
                .connection
                .prepare(&pragma)
                .map_err(DatabaseError::from_engine)?;
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(DatabaseError::from_engine)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DatabaseError::from_engine)?
        };
        let mut sql = format!("SELECT * FROM {}", quote_ident(table.as_str()));
        let mut params = Vec::new();
        push_filters(filters, &mut sql, &mut params);
        if !order_names.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(
                &order_names
                    .iter()
                    .map(|name| quote_ident(name))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        sql.push_str(" LIMIT ? OFFSET ?");
        params.push(Value::Integer((TABLE_PAGE_SIZE + 1) as i64));
        params.push(Value::Integer((page.index() * TABLE_PAGE_SIZE) as i64));
        let mut statement = self
            .connection
            .prepare(&sql)
            .map_err(DatabaseError::from_engine)?;
        let columns = statement
            .column_names()
            .into_iter()
            .map(|name| ColumnName::new(name.to_string()))
            .collect::<Vec<_>>();
        let column_count = columns.len();
        let rows = statement
            .query_map(params_from_iter(params.iter()), |row| {
                let mut cells = Vec::with_capacity(column_count);
                for index in 0..column_count {
                    cells.push(cell_from(row.get_ref(index)?));
                }
                Ok(cells)
            })
            .map_err(DatabaseError::from_engine)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_engine)?;
        Ok(TablePage::from_fetched(columns, rows))
    }

    fn table_structure(&self, table: &TableName) -> Result<TableStructure, DatabaseError> {
        table_structure(&self.connection, table)
    }

    fn row_identity(
        &self,
        table: &TableName,
        row: &[(ColumnName, Cell)],
    ) -> Result<Option<RowIdentity>, DatabaseError> {
        let Some(names) = identity_column_names(&self.connection, table)? else {
            return Ok(None);
        };
        let mut columns = Vec::with_capacity(names.len());
        for name in names {
            let Some((_, cell)) = row.iter().find(|(column, _)| column.as_str() == name) else {
                return Ok(None);
            };
            columns.push((ColumnName::new(name), cell.clone()));
        }
        Ok(Some(RowIdentity::new(columns)))
    }

    fn apply_staged(&self, changes: &[StagedChange]) -> Result<(), ApplyEngineError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(DatabaseError::from_engine)?;
        for change in changes {
            apply_change(&tx, change)?;
        }
        tx.commit().map_err(DatabaseError::from_engine)?;
        Ok(())
    }

    fn sql_kind(&self, sql: &str) -> Result<SqlKind, DatabaseError> {
        let statement = self
            .connection
            .prepare(sql)
            .map_err(DatabaseError::from_engine)?;
        if statement.readonly() {
            Ok(SqlKind::Read)
        } else {
            Ok(SqlKind::Mutating)
        }
    }

    fn query(&self, sql: &str, page: Page) -> Result<QueryResult, DatabaseError> {
        if self.sql_kind(sql)? != SqlKind::Read {
            return Err(DatabaseError::from_engine("SQL is mutating"));
        }
        let staging = result_staging(&self.connection, sql)?;
        let page = query_page(&self.connection, sql, page)?;
        Ok(QueryResult::new(page, staging))
    }

    fn execute_sql(&self, sql: &str) -> Result<(), DatabaseError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(DatabaseError::from_engine)?;
        tx.execute_batch(sql).map_err(DatabaseError::from_engine)?;
        tx.commit().map_err(DatabaseError::from_engine)?;
        Ok(())
    }
}

fn trim_sql(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim()
}

fn table_structure(
    connection: &Connection,
    table: &TableName,
) -> Result<TableStructure, DatabaseError> {
    let pragma = format!("PRAGMA table_info({})", quote_ident(table.as_str()));
    let mut statement = connection
        .prepare(&pragma)
        .map_err(DatabaseError::from_engine)?;
    let columns = statement
        .query_map([], |row| {
            let name = row.get::<_, String>(1)?;
            let type_name = row.get::<_, String>(2)?;
            let not_null = row.get::<_, i64>(3)? != 0;
            let pk = row.get::<_, i64>(5)?;
            Ok(StructureColumn::new(
                ColumnName::new(name),
                type_name,
                not_null,
                pk != 0,
            ))
        })
        .map_err(DatabaseError::from_engine)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from_engine)?;

    let list = format!("PRAGMA index_list({})", quote_ident(table.as_str()));
    let mut statement = connection
        .prepare(&list)
        .map_err(DatabaseError::from_engine)?;
    let index_rows = statement
        .query_map([], |row| {
            let name = row.get::<_, String>(1)?;
            let unique = row.get::<_, i64>(2)? != 0;
            Ok((name, unique))
        })
        .map_err(DatabaseError::from_engine)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from_engine)?;

    let mut indexes = Vec::with_capacity(index_rows.len());
    for (index_name, unique) in index_rows {
        let info = format!("PRAGMA index_info({})", quote_ident(&index_name));
        let mut statement = connection
            .prepare(&info)
            .map_err(DatabaseError::from_engine)?;
        let mut index_columns = statement
            .query_map([], |row| {
                let seqno = row.get::<_, i64>(0)?;
                let name = row.get::<_, Option<String>>(2)?;
                Ok((seqno, name))
            })
            .map_err(DatabaseError::from_engine)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_engine)?;
        index_columns.sort_by_key(|(seqno, _)| *seqno);
        let columns = index_columns
            .into_iter()
            .filter_map(|(_, name)| name)
            .collect();
        indexes.push(StructureIndex::new(index_name, unique, columns));
    }

    Ok(TableStructure::new(columns, indexes))
}

fn result_staging(connection: &Connection, sql: &str) -> Result<ResultStaging, DatabaseError> {
    let Some((table, projection)) = one_table_projection(sql) else {
        return Ok(ResultStaging::ReadOnly);
    };
    let table = TableName::new(table);
    let Some(identity) = identity_column_names(connection, &table)? else {
        return Ok(ResultStaging::ReadOnly);
    };
    if identity_present(&identity, &projection) {
        Ok(ResultStaging::Staged { table })
    } else {
        Ok(ResultStaging::ReadOnly)
    }
}

fn query_page(connection: &Connection, sql: &str, page: Page) -> Result<TablePage, DatabaseError> {
    let inner = trim_sql(sql);
    let wrapped = format!("SELECT * FROM ({inner}) LIMIT ? OFFSET ?");
    let mut params = vec![
        Value::Integer((TABLE_PAGE_SIZE + 1) as i64),
        Value::Integer((page.index() * TABLE_PAGE_SIZE) as i64),
    ];
    match fetch_rows(connection, &wrapped, &params) {
        Ok(page) => Ok(page),
        Err(_) => {
            params.clear();
            fetch_rows(connection, inner, &params)
        }
    }
}

fn fetch_rows(
    connection: &Connection,
    sql: &str,
    params: &[Value],
) -> Result<TablePage, DatabaseError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(DatabaseError::from_engine)?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(|name| ColumnName::new(name.to_string()))
        .collect::<Vec<_>>();
    let column_count = columns.len();
    let rows = statement
        .query_map(params_from_iter(params.iter()), |row| {
            let mut cells = Vec::with_capacity(column_count);
            for index in 0..column_count {
                cells.push(cell_from(row.get_ref(index)?));
            }
            Ok(cells)
        })
        .map_err(DatabaseError::from_engine)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from_engine)?;
    Ok(TablePage::from_fetched(columns, rows))
}

fn identity_column_names(
    connection: &Connection,
    table: &TableName,
) -> Result<Option<Vec<String>>, DatabaseError> {
    let pragma = format!("PRAGMA table_info({})", quote_ident(table.as_str()));
    let mut statement = connection
        .prepare(&pragma)
        .map_err(DatabaseError::from_engine)?;
    let mut pk_columns = statement
        .query_map([], |row| {
            let name = row.get::<_, String>(1)?;
            let pk = row.get::<_, i64>(5)?;
            Ok((pk, name))
        })
        .map_err(DatabaseError::from_engine)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from_engine)?;
    pk_columns.retain(|(pk, _)| *pk > 0);
    if !pk_columns.is_empty() {
        pk_columns.sort_by_key(|(pk, _)| *pk);
        return Ok(Some(pk_columns.into_iter().map(|(_, name)| name).collect()));
    }

    let list = format!("PRAGMA index_list({})", quote_ident(table.as_str()));
    let mut statement = connection
        .prepare(&list)
        .map_err(DatabaseError::from_engine)?;
    let mut indexes = statement
        .query_map([], |row| {
            let name = row.get::<_, String>(1)?;
            let unique = row.get::<_, i64>(2)?;
            let origin = row.get::<_, String>(3)?;
            let partial = row.get::<_, i64>(4)?;
            Ok((name, unique, origin, partial))
        })
        .map_err(DatabaseError::from_engine)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from_engine)?;
    indexes.retain(|(_, unique, origin, partial)| *unique == 1 && *partial == 0 && origin != "pk");
    indexes.sort_by(|a, b| a.0.cmp(&b.0));
    for (index_name, _, _, _) in indexes {
        let info = format!("PRAGMA index_info({})", quote_ident(&index_name));
        let mut statement = connection
            .prepare(&info)
            .map_err(DatabaseError::from_engine)?;
        let mut columns = statement
            .query_map([], |row| {
                let seqno = row.get::<_, i64>(0)?;
                let name = row.get::<_, Option<String>>(2)?;
                Ok((seqno, name))
            })
            .map_err(DatabaseError::from_engine)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_engine)?;
        if columns.iter().any(|(_, name)| name.is_none()) {
            continue;
        }
        columns.sort_by_key(|(seqno, _)| *seqno);
        return Ok(Some(
            columns.into_iter().filter_map(|(_, name)| name).collect(),
        ));
    }
    Ok(None)
}

fn apply_change(connection: &Connection, change: &StagedChange) -> Result<(), ApplyEngineError> {
    match change {
        StagedChange::Insert { table, values, .. } => {
            let sql = insert_sql(table, values);
            let params = values.iter().map(|(_, cell)| value_from_cell(cell));
            connection
                .execute(&sql, params_from_iter(params))
                .map_err(DatabaseError::from_engine)?;
            Ok(())
        }
        StagedChange::Update {
            table,
            identity,
            last_seen,
            new_values,
            ..
        } => {
            let mut sql = format!("UPDATE {} SET ", quote_ident(table.as_str()));
            let mut params = Vec::new();
            for (index, (column, cell)) in new_values.iter().enumerate() {
                if index > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" = ?");
                params.push(value_from_cell(cell));
            }
            push_row_predicate(&mut sql, &mut params, identity, last_seen);
            let changed = connection
                .execute(&sql, params_from_iter(params.iter()))
                .map_err(DatabaseError::from_engine)?;
            if changed != 1 {
                return Err(ApplyEngineError::Conflict);
            }
            Ok(())
        }
        StagedChange::Delete {
            table,
            identity,
            last_seen,
            ..
        } => {
            let mut sql = format!("DELETE FROM {}", quote_ident(table.as_str()));
            let mut params = Vec::new();
            push_row_predicate(&mut sql, &mut params, identity, last_seen);
            let changed = connection
                .execute(&sql, params_from_iter(params.iter()))
                .map_err(DatabaseError::from_engine)?;
            if changed != 1 {
                return Err(ApplyEngineError::Conflict);
            }
            Ok(())
        }
    }
}

fn insert_sql(table: &TableName, values: &[(ColumnName, Cell)]) -> String {
    if values.is_empty() {
        return format!("INSERT INTO {} DEFAULT VALUES", quote_ident(table.as_str()));
    }
    let columns = values
        .iter()
        .map(|(column, _)| quote_ident(column.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = vec!["?"; values.len()].join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(table.as_str()),
        columns,
        placeholders
    )
}

fn push_row_predicate(
    sql: &mut String,
    params: &mut Vec<Value>,
    identity: &RowIdentity,
    last_seen: &[(ColumnName, Cell)],
) {
    let mut seen = Vec::new();
    sql.push_str(" WHERE ");
    let mut first = true;
    for (column, cell) in identity.columns().iter().chain(last_seen.iter()) {
        if seen.iter().any(|name| name == column.as_str()) {
            continue;
        }
        seen.push(column.as_str().to_string());
        if !first {
            sql.push_str(" AND ");
        }
        first = false;
        match cell {
            Cell::Null => {
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" IS NULL");
            }
            other => {
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" = ?");
                params.push(value_from_cell(other));
            }
        }
    }
}
