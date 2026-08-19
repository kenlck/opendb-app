use std::path::PathBuf;

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use url::Url;

use super::{Database, DatabaseError};
use crate::table::TableName;
use crate::table_page::{Cell, ColumnName, Filter, Page, TABLE_PAGE_SIZE, TablePage};

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
}
