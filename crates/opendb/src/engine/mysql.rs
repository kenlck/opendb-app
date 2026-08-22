use std::sync::Mutex;

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, Params, TxOpts, Value};

use super::common::{like_contains, quote_ident, trim_sql};
use super::{ApplyEngineError, Database, DatabaseError};
use crate::query::{
    QueryResult, ResultStaging, SqlKind, identity_present, one_table_projection_mysql,
};
use crate::schema_change::{SchemaChange, schema_change_ddl_mysql};
use crate::staged::{RowIdentity, StagedChange};
use crate::table::{Table, TableName};
use crate::table_page::{Cell, ColumnName, Filter, Page, TABLE_PAGE_SIZE, TablePage};
use crate::table_structure::{StructureColumn, StructureIndex, TableStructure};

pub(crate) struct MysqlDatabase {
    conn: Mutex<Conn>,
}

pub(crate) enum MysqlOpenError {
    Database(String),
}

impl MysqlDatabase {
    pub(crate) fn open(connection_string: &str) -> Result<Self, MysqlOpenError> {
        let url = mysql_url(connection_string);
        let opts = Opts::from_url(&url).map_err(|error| MysqlOpenError::Database(error.to_string()))?;
        let mut conn =
            Conn::new(opts).map_err(|error| MysqlOpenError::Database(error.to_string()))?;
        conn.query_drop("SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',ANSI_QUOTES')")
            .map_err(|error| MysqlOpenError::Database(error.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Conn> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn mysql_url(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("mariadb://") {
        format!("mysql://{rest}")
    } else {
        raw.to_string()
    }
}

fn cell_from_value(value: Value) -> Cell {
    match value {
        Value::NULL => Cell::Null,
        Value::Int(v) => Cell::Integer(v),
        Value::UInt(v) if v <= i64::MAX as u64 => Cell::Integer(v as i64),
        Value::UInt(v) => Cell::Text(v.to_string()),
        Value::Float(v) => Cell::Real(f64::from(v)),
        Value::Double(v) => Cell::Real(v),
        Value::Bytes(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(text) => Cell::Text(text),
            Err(_) => Cell::Blob(bytes),
        },
        Value::Date(year, month, day, hour, minute, second, micros) => {
            if hour == 0 && minute == 0 && second == 0 && micros == 0 {
                Cell::Text(format!("{year:04}-{month:02}-{day:02}"))
            } else {
                Cell::Text(format!(
                    "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
                ))
            }
        }
        Value::Time(neg, days, hour, minute, second, _) => {
            let sign = if neg { "-" } else { "" };
            Cell::Text(format!(
                "{sign}{:02}:{:02}:{:02}",
                days * 24 + u32::from(hour),
                minute,
                second
            ))
        }
    }
}

fn value_from_cell(cell: &Cell) -> Value {
    match cell {
        Cell::Null => Value::NULL,
        Cell::Integer(v) => Value::Int(*v),
        Cell::Real(v) => Value::Double(*v),
        Cell::Text(v) => Value::Bytes(v.as_bytes().to_vec()),
        Cell::Blob(v) => Value::Bytes(v.clone()),
    }
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
                sql.push_str(" AS CHAR) LIKE ? ESCAPE '\\'");
                params.push(Value::Bytes(like_contains(value).into_bytes()));
            }
            Filter::IsNull { column } => {
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" IS NULL");
            }
        }
    }
}

fn fetch_rows(
    conn: &mut Conn,
    sql: &str,
    params: Vec<Value>,
) -> Result<TablePage, DatabaseError> {
    let mut result = conn
        .exec_iter(sql, Params::Positional(params))
        .map_err(DatabaseError::from_engine)?;
    let columns = result
        .columns()
        .as_ref()
        .iter()
        .map(|column| ColumnName::new(column.name_str().as_ref().to_string()))
        .collect::<Vec<_>>();
    let column_count = columns.len();
    let mut fetched = Vec::new();
    for row in result.by_ref() {
        let row = row.map_err(DatabaseError::from_engine)?;
        let mut cells = Vec::with_capacity(column_count);
        for index in 0..column_count {
            cells.push(cell_from_value(row[index].clone()));
        }
        fetched.push(cells);
    }
    drop(result);
    Ok(TablePage::from_fetched(columns, fetched))
}

impl Database for MysqlDatabase {
    fn namespaces_grouped(&self) -> bool {
        false
    }

    fn list_tables(&self) -> Result<Vec<Table>, DatabaseError> {
        let mut conn = self.lock();
        let names: Vec<String> = conn
            .query(
                "SELECT table_name
                 FROM information_schema.tables
                 WHERE table_schema = DATABASE()
                   AND table_type = 'BASE TABLE'
                 ORDER BY table_name",
            )
            .map_err(DatabaseError::from_engine)?;
        Ok(names
            .into_iter()
            .map(|name| Table::flat(TableName::new(name)))
            .collect())
    }

    fn table_page(
        &self,
        table: &Table,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, DatabaseError> {
        let table_name = table.name().as_str();
        let mut conn = self.lock();
        let order_names: Vec<String> = conn
            .exec(
                "SELECT column_name
                 FROM information_schema.columns
                 WHERE table_schema = DATABASE() AND table_name = ?
                 ORDER BY ordinal_position",
                (table_name,),
            )
            .map_err(DatabaseError::from_engine)?;
        let mut sql = format!("SELECT * FROM {}", quote_ident(table_name));
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
        params.push(Value::Int((TABLE_PAGE_SIZE + 1) as i64));
        params.push(Value::Int((page.index() * TABLE_PAGE_SIZE) as i64));
        fetch_rows(&mut conn, &sql, params)
    }

    fn table_structure(&self, table: &Table) -> Result<TableStructure, DatabaseError> {
        table_structure(&mut self.lock(), table)
    }

    fn row_identity(
        &self,
        table: &Table,
        row: &[(ColumnName, Cell)],
    ) -> Result<Option<RowIdentity>, DatabaseError> {
        let Some(names) = identity_column_names(&mut self.lock(), table)? else {
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
        let mut conn = self.lock();
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .map_err(DatabaseError::from_engine)?;
        for change in changes {
            apply_change(&mut tx, change)?;
        }
        tx.commit().map_err(DatabaseError::from_engine)?;
        Ok(())
    }

    fn sql_kind(&self, sql: &str) -> Result<SqlKind, DatabaseError> {
        crate::query::sql_kind_mysql(sql)
    }

    fn query(&self, sql: &str, page: Page) -> Result<QueryResult, DatabaseError> {
        if self.sql_kind(sql)? != SqlKind::Read {
            return Err(DatabaseError::from_engine("SQL is mutating"));
        }
        let mut conn = self.lock();
        let staging = result_staging(&mut conn, sql)?;
        let page = query_page(&mut conn, sql, page)?;
        Ok(QueryResult::new(page, staging))
    }

    fn execute_sql(&self, sql: &str) -> Result<(), DatabaseError> {
        let mut conn = self.lock();
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .map_err(DatabaseError::from_engine)?;
        tx.query_drop(sql).map_err(DatabaseError::from_engine)?;
        tx.commit().map_err(DatabaseError::from_engine)?;
        Ok(())
    }

    fn schema_change_ddl(&self, change: &SchemaChange) -> String {
        schema_change_ddl_mysql(change)
    }
}

fn table_structure(conn: &mut Conn, table: &Table) -> Result<TableStructure, DatabaseError> {
    let table_sql = quote_ident(table.name().as_str());
    let column_rows: Vec<(String, String, String, String)> = conn
        .query(format!("SHOW COLUMNS FROM {table_sql}"))
        .map_err(DatabaseError::from_engine)?;
    let columns = column_rows
        .into_iter()
        .map(|(name, type_name, nullable, key)| {
            StructureColumn::new(
                ColumnName::new(name),
                type_name,
                nullable.eq_ignore_ascii_case("NO"),
                key.eq_ignore_ascii_case("PRI"),
            )
        })
        .collect();

    let index_rows: Vec<(String, i64, i64, String)> = conn
        .exec(
            "SELECT index_name, non_unique, seq_in_index, column_name
             FROM information_schema.statistics
             WHERE table_schema = DATABASE() AND table_name = ?
             ORDER BY index_name, seq_in_index",
            (table.name().as_str(),),
        )
        .map_err(DatabaseError::from_engine)?;
    let mut indexes: Vec<(String, bool, Vec<String>)> = Vec::new();
    for (name, non_unique, _, column) in index_rows {
        if let Some(existing) = indexes.iter_mut().find(|(index_name, _, _)| index_name == &name)
        {
            existing.2.push(column);
        } else {
            indexes.push((name, non_unique == 0, vec![column]));
        }
    }

    Ok(TableStructure::new(
        columns,
        indexes
            .into_iter()
            .map(|(name, unique, columns)| StructureIndex::new(name, unique, columns))
            .collect(),
    ))
}

fn result_staging(conn: &mut Conn, sql: &str) -> Result<ResultStaging, DatabaseError> {
    let Some((table_ref, projection)) = one_table_projection_mysql(sql) else {
        return Ok(ResultStaging::ReadOnly);
    };
    let table = Table::flat(TableName::new(table_ref.name().to_string()));
    let Some(identity) = identity_column_names(conn, &table)? else {
        return Ok(ResultStaging::ReadOnly);
    };
    if identity_present(&identity, &projection) {
        Ok(ResultStaging::Staged { table })
    } else {
        Ok(ResultStaging::ReadOnly)
    }
}

fn query_page(conn: &mut Conn, sql: &str, page: Page) -> Result<TablePage, DatabaseError> {
    let inner = trim_sql(sql);
    let wrapped = format!("SELECT * FROM ({inner}) AS q LIMIT ? OFFSET ?");
    let limit = Value::Int((TABLE_PAGE_SIZE + 1) as i64);
    let offset = Value::Int((page.index() * TABLE_PAGE_SIZE) as i64);
    fetch_rows(conn, &wrapped, vec![limit, offset])
}

fn identity_column_names(
    conn: &mut Conn,
    table: &Table,
) -> Result<Option<Vec<String>>, DatabaseError> {
    let table_name = table.name().as_str();
    let pk: Vec<String> = conn
        .exec(
            "SELECT column_name
             FROM information_schema.key_column_usage
             WHERE table_schema = DATABASE()
               AND table_name = ?
               AND constraint_name = 'PRIMARY'
             ORDER BY ordinal_position",
            (table_name,),
        )
        .map_err(DatabaseError::from_engine)?;
    if !pk.is_empty() {
        return Ok(Some(pk));
    }

    let unique_indexes: Vec<String> = conn
        .exec(
            "SELECT DISTINCT index_name
             FROM information_schema.statistics
             WHERE table_schema = DATABASE()
               AND table_name = ?
               AND non_unique = 0
               AND index_name <> 'PRIMARY'
             ORDER BY index_name",
            (table_name,),
        )
        .map_err(DatabaseError::from_engine)?;
    for index_name in unique_indexes {
        let columns: Vec<String> = conn
            .exec(
                "SELECT column_name
                 FROM information_schema.statistics
                 WHERE table_schema = DATABASE()
                   AND table_name = ?
                   AND index_name = ?
                 ORDER BY seq_in_index",
                (table_name, index_name),
            )
            .map_err(DatabaseError::from_engine)?;
        if !columns.is_empty() {
            return Ok(Some(columns));
        }
    }
    Ok(None)
}

fn apply_change(
    tx: &mut mysql::Transaction<'_>,
    change: &StagedChange,
) -> Result<(), ApplyEngineError> {
    match change {
        StagedChange::Insert { table, values, .. } => {
            let sql = insert_sql(table, values);
            let params: Vec<Value> = values.iter().map(|(_, cell)| value_from_cell(cell)).collect();
            tx.exec_drop(&sql, Params::Positional(params))
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
            let mut sql = format!("UPDATE {} SET ", quote_ident(table.name().as_str()));
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
            tx.exec_drop(&sql, Params::Positional(params.clone()))
                .map_err(DatabaseError::from_engine)?;
            if tx.affected_rows() != 1 {
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
            let mut sql = format!("DELETE FROM {}", quote_ident(table.name().as_str()));
            let mut params = Vec::new();
            push_row_predicate(&mut sql, &mut params, identity, last_seen);
            tx.exec_drop(&sql, Params::Positional(params))
                .map_err(DatabaseError::from_engine)?;
            if tx.affected_rows() != 1 {
                return Err(ApplyEngineError::Conflict);
            }
            Ok(())
        }
    }
}

fn insert_sql(table: &Table, values: &[(ColumnName, Cell)]) -> String {
    if values.is_empty() {
        return format!(
            "INSERT INTO {} () VALUES ()",
            quote_ident(table.name().as_str())
        );
    }
    let columns = values
        .iter()
        .map(|(column, _)| quote_ident(column.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = vec!["?"; values.len()].join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(table.name().as_str()),
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
