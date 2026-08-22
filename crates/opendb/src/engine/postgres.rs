use std::sync::Mutex;

use postgres::types::Type;
use postgres::{Client, NoTls, Row, Transaction};

use super::common::{like_contains, quote_ident, trim_sql};
use super::{ApplyEngineError, Database, DatabaseError};
use crate::query::{
    QueryResult, ResultStaging, SqlKind, TableReference, identity_present,
    one_table_projection_postgres,
};
use crate::schema_change::{SchemaChange, schema_change_ddl_postgres};
use crate::staged::{RowIdentity, StagedChange};
use crate::table::{Namespace, Table, TableName};
use crate::table_page::{Cell, ColumnName, Filter, Page, TABLE_PAGE_SIZE, TablePage};
use crate::table_structure::{StructureColumn, StructureIndex, TableStructure};

pub(crate) struct PostgresDatabase {
    client: Mutex<Client>,
}

pub(crate) enum PostgresOpenError {
    Database(String),
}

impl PostgresDatabase {
    pub(crate) fn open(connection_string: &str) -> Result<Self, PostgresOpenError> {
        let client = Client::connect(connection_string, NoTls)
            .map_err(|error| PostgresOpenError::Database(error.to_string()))?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Client> {
        self.client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn table_namespace(table: &Table) -> &str {
    table
        .namespace()
        .expect("postgres tables carry a Namespace")
        .as_str()
}

fn table_sql(table: &Table) -> String {
    super::common::qualified_table(Some(table_namespace(table)), table.name().as_str())
}

fn cell_from_row(row: &Row, col: usize) -> Result<Cell, DatabaseError> {
    let ty = row.columns()[col].type_();
    if ty == &Type::INT2 {
        return Ok(row
            .try_get::<_, Option<i16>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(|value| Cell::Integer(i64::from(value)))
            .unwrap_or(Cell::Null));
    }
    if ty == &Type::INT4 {
        return Ok(row
            .try_get::<_, Option<i32>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(|value| Cell::Integer(i64::from(value)))
            .unwrap_or(Cell::Null));
    }
    if ty == &Type::INT8 {
        return Ok(row
            .try_get::<_, Option<i64>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(Cell::Integer)
            .unwrap_or(Cell::Null));
    }
    if ty == &Type::BOOL {
        return Ok(row
            .try_get::<_, Option<bool>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(|value| Cell::Integer(if value { 1 } else { 0 }))
            .unwrap_or(Cell::Null));
    }
    if ty == &Type::FLOAT4 {
        return Ok(row
            .try_get::<_, Option<f32>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(|value| Cell::Real(f64::from(value)))
            .unwrap_or(Cell::Null));
    }
    if ty == &Type::FLOAT8 {
        return Ok(row
            .try_get::<_, Option<f64>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(Cell::Real)
            .unwrap_or(Cell::Null));
    }
    if ty == &Type::BYTEA {
        return Ok(row
            .try_get::<_, Option<Vec<u8>>>(col)
            .map_err(DatabaseError::from_engine)?
            .map(Cell::Blob)
            .unwrap_or(Cell::Null));
    }
    Ok(row
        .try_get::<_, Option<String>>(col)
        .map_err(DatabaseError::from_engine)?
        .map(Cell::Text)
        .unwrap_or(Cell::Null))
}

enum SqlParam {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl SqlParam {
    fn from_cell(cell: &Cell) -> Self {
        match cell {
            Cell::Null => Self::Null,
            Cell::Integer(v) => Self::Integer(*v),
            Cell::Real(v) => Self::Real(*v),
            Cell::Text(v) => Self::Text(v.clone()),
            Cell::Blob(v) => Self::Blob(v.clone()),
        }
    }

    fn as_tosql(&self) -> &(dyn postgres::types::ToSql + Sync) {
        static NULL: Option<i32> = None;
        match self {
            SqlParam::Null => &NULL,
            SqlParam::Integer(v) => v,
            SqlParam::Real(v) => v,
            SqlParam::Text(v) => v,
            SqlParam::Blob(v) => v,
        }
    }
}

fn sql_param_refs(params: &[SqlParam]) -> Vec<&(dyn postgres::types::ToSql + Sync)> {
    params.iter().map(SqlParam::as_tosql).collect()
}

fn push_filters(filters: &[Filter], sql: &mut String, params: &mut Vec<SqlParam>) {
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
                    sql.push_str(" = $");
                    params.push(SqlParam::from_cell(other));
                    sql.push_str(&params.len().to_string());
                }
            },
            Filter::Contains { column, value } => {
                sql.push_str("CAST(");
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" AS TEXT) LIKE $");
                params.push(SqlParam::Text(like_contains(value)));
                sql.push_str(&params.len().to_string());
                sql.push_str(" ESCAPE '\\'");
            }
            Filter::IsNull { column } => {
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" IS NULL");
            }
        }
    }
}

fn fetch_rows(
    client: &mut Client,
    sql: &str,
    params: &[SqlParam],
) -> Result<TablePage, DatabaseError> {
    let param_refs = sql_param_refs(params);
    let rows = client
        .query(sql, &param_refs)
        .map_err(DatabaseError::from_engine)?;
    if rows.is_empty() {
        return Ok(TablePage::from_fetched(Vec::new(), Vec::new()));
    }
    let columns = rows[0]
        .columns()
        .iter()
        .map(|column| ColumnName::new(column.name().to_string()))
        .collect::<Vec<_>>();
    let column_count = columns.len();
    let mut fetched = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::with_capacity(column_count);
        for index in 0..column_count {
            cells.push(cell_from_row(&row, index)?);
        }
        fetched.push(cells);
    }
    Ok(TablePage::from_fetched(columns, fetched))
}

impl Database for PostgresDatabase {
    fn namespaces_grouped(&self) -> bool {
        true
    }

    fn list_tables(&self) -> Result<Vec<Table>, DatabaseError> {
        let mut client = self.lock();
        let rows = client
            .query(
                "SELECT table_schema, table_name
                 FROM information_schema.tables
                 WHERE table_type = 'BASE TABLE'
                 ORDER BY table_schema, table_name",
                &[],
            )
            .map_err(DatabaseError::from_engine)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                Table::in_namespace(
                    Namespace::new(row.get::<_, String>("table_schema")),
                    TableName::new(row.get::<_, String>("table_name")),
                )
            })
            .collect())
    }

    fn table_page(
        &self,
        table: &Table,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, DatabaseError> {
        let namespace = table_namespace(table);
        let table_name = table.name().as_str();
        let mut client = self.lock();
        let order_rows = client
            .query(
                "SELECT column_name
                 FROM information_schema.columns
                 WHERE table_schema = $1 AND table_name = $2
                 ORDER BY ordinal_position",
                &[&namespace, &table_name],
            )
            .map_err(DatabaseError::from_engine)?;
        let order_names: Vec<String> = order_rows
            .into_iter()
            .map(|row| row.get::<_, String>("column_name"))
            .collect();
        let mut sql = format!("SELECT * FROM {}", table_sql(table));
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
        params.push(SqlParam::Integer((TABLE_PAGE_SIZE + 1) as i64));
        sql.push_str(" LIMIT $");
        sql.push_str(&params.len().to_string());
        params.push(SqlParam::Integer((page.index() * TABLE_PAGE_SIZE) as i64));
        sql.push_str(" OFFSET $");
        sql.push_str(&params.len().to_string());
        fetch_rows(&mut client, &sql, &params)
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
        let mut client = self.lock();
        let mut tx = client
            .transaction()
            .map_err(DatabaseError::from_engine)?;
        for change in changes {
            apply_change(&mut tx, change)?;
        }
        tx.commit().map_err(DatabaseError::from_engine)?;
        Ok(())
    }

    fn sql_kind(&self, sql: &str) -> Result<SqlKind, DatabaseError> {
        crate::query::sql_kind_postgres(sql)
    }

    fn query(&self, sql: &str, page: Page) -> Result<QueryResult, DatabaseError> {
        if self.sql_kind(sql)? != SqlKind::Read {
            return Err(DatabaseError::from_engine("SQL is mutating"));
        }
        let mut client = self.lock();
        let staging = result_staging(&mut client, sql)?;
        let page = query_page(&mut client, sql, page)?;
        Ok(QueryResult::new(page, staging))
    }

    fn execute_sql(&self, sql: &str) -> Result<(), DatabaseError> {
        let mut client = self.lock();
        let mut tx = client
            .transaction()
            .map_err(DatabaseError::from_engine)?;
        tx.batch_execute(sql).map_err(DatabaseError::from_engine)?;
        tx.commit().map_err(DatabaseError::from_engine)?;
        Ok(())
    }

    fn schema_change_ddl(&self, change: &SchemaChange) -> String {
        schema_change_ddl_postgres(change)
    }
}

fn table_structure(client: &mut Client, table: &Table) -> Result<TableStructure, DatabaseError> {
    let namespace = table_namespace(table);
    let table_name = table.name().as_str();
    let rows = client
        .query(
            "SELECT column_name, data_type, is_nullable
             FROM information_schema.columns
             WHERE table_schema = $1 AND table_name = $2
             ORDER BY ordinal_position",
            &[&namespace, &table_name],
        )
        .map_err(DatabaseError::from_engine)?;
    let columns = rows
        .into_iter()
        .map(|row| {
            let name = row.get::<_, String>("column_name");
            let type_name = row.get::<_, String>("data_type");
            let not_null = row.get::<_, String>("is_nullable");
            StructureColumn::new(
                ColumnName::new(name),
                type_name,
                not_null == "NO",
                false,
            )
        })
        .collect::<Vec<_>>();

    let pk_rows = client
        .query(
            "SELECT a.attname
             FROM pg_index i
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
             JOIN pg_class c ON c.oid = i.indrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE i.indisprimary
               AND n.nspname = $1
               AND c.relname = $2
             ORDER BY array_position(i.indkey, a.attnum)",
            &[&namespace, &table_name],
        )
        .map_err(DatabaseError::from_engine)?;
    let pk_columns: Vec<String> = pk_rows
        .into_iter()
        .map(|row| row.get::<_, String>("attname"))
        .collect();
    let columns = columns
        .into_iter()
        .map(|column| {
            if pk_columns
                .iter()
                .any(|name| name == column.name().as_str())
            {
                StructureColumn::new(
                    column.name().clone(),
                    column.type_name().to_string(),
                    column.not_null(),
                    true,
                )
            } else {
                column
            }
        })
        .collect();

    let index_rows = client
        .query(
            "SELECT indexname, indexdef
             FROM pg_indexes
             WHERE schemaname = $1 AND tablename = $2
             ORDER BY indexname",
            &[&namespace, &table_name],
        )
        .map_err(DatabaseError::from_engine)?;
    let mut indexes = Vec::with_capacity(index_rows.len());
    for row in index_rows {
        let name = row.get::<_, String>("indexname");
        let definition = row.get::<_, String>("indexdef");
        let unique = definition.to_ascii_uppercase().contains("UNIQUE INDEX");
        let columns = index_columns_from_definition(&definition);
        indexes.push(StructureIndex::new(name, unique, columns));
    }

    Ok(TableStructure::new(columns, indexes))
}

fn index_columns_from_definition(definition: &str) -> Vec<String> {
    let upper = definition.to_ascii_uppercase();
    let Some(start) = upper.rfind('(') else {
        return Vec::new();
    };
    let Some(end) = upper.rfind(')') else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    definition[start + 1..end]
        .split(',')
        .map(|part| part.trim().trim_matches('"').to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

fn result_staging(client: &mut Client, sql: &str) -> Result<ResultStaging, DatabaseError> {
    let Some((table_ref, projection)) = one_table_projection_postgres(sql) else {
        return Ok(ResultStaging::ReadOnly);
    };
    let table = resolve_table(client, &table_ref)?;
    let Some(identity) = identity_column_names(client, &table)? else {
        return Ok(ResultStaging::ReadOnly);
    };
    if identity_present(&identity, &projection) {
        Ok(ResultStaging::Staged { table })
    } else {
        Ok(ResultStaging::ReadOnly)
    }
}

fn resolve_table(client: &mut Client, reference: &TableReference) -> Result<Table, DatabaseError> {
    if let Some(namespace) = reference.namespace() {
        let rows = client
            .query(
                "SELECT 1
                 FROM information_schema.tables
                 WHERE table_type = 'BASE TABLE'
                   AND table_schema = $1
                   AND table_name = $2",
                &[&namespace, &reference.name()],
            )
            .map_err(DatabaseError::from_engine)?;
        if rows.is_empty() {
            return Err(DatabaseError::from_engine(format!(
                "table not found: {namespace}.{}",
                reference.name()
            )));
        }
        return Ok(Table::in_namespace(
            Namespace::new(namespace.to_string()),
            TableName::new(reference.name().to_string()),
        ));
    }
    let rows = client
        .query(
            "SELECT table_schema
             FROM information_schema.tables
             WHERE table_type = 'BASE TABLE'
               AND table_name = $1
             ORDER BY CASE WHEN table_schema = 'public' THEN 0 ELSE 1 END, table_schema",
            &[&reference.name()],
        )
        .map_err(DatabaseError::from_engine)?;
    let Some(row) = rows.first() else {
        return Err(DatabaseError::from_engine(format!(
            "table not found: {}",
            reference.name()
        )));
    };
    let namespace = Namespace::new(row.get::<_, String>("table_schema"));
    Ok(Table::in_namespace(
        namespace,
        TableName::new(reference.name().to_string()),
    ))
}

fn query_page(client: &mut Client, sql: &str, page: Page) -> Result<TablePage, DatabaseError> {
    let inner = trim_sql(sql);
    let wrapped = format!("SELECT * FROM ({inner}) AS q LIMIT $1 OFFSET $2");
    let limit = (TABLE_PAGE_SIZE + 1) as i64;
    let offset = (page.index() * TABLE_PAGE_SIZE) as i64;
    fetch_rows(
        client,
        &wrapped,
        &[SqlParam::Integer(limit), SqlParam::Integer(offset)],
    )
}

fn identity_column_names(
    client: &mut Client,
    table: &Table,
) -> Result<Option<Vec<String>>, DatabaseError> {
    let namespace = table_namespace(table);
    let table_name = table.name().as_str();
    let pk_rows = client
        .query(
            "SELECT a.attname
             FROM pg_index i
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
             JOIN pg_class c ON c.oid = i.indrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE i.indisprimary
               AND n.nspname = $1
               AND c.relname = $2
             ORDER BY array_position(i.indkey, a.attnum)",
            &[&namespace, &table_name],
        )
        .map_err(DatabaseError::from_engine)?;
    if !pk_rows.is_empty() {
        return Ok(Some(
            pk_rows
                .into_iter()
                .map(|row| row.get::<_, String>("attname"))
                .collect(),
        ));
    }

    let unique_rows = client
        .query(
            "SELECT i.relname AS index_name
             FROM pg_class t
             JOIN pg_namespace n ON n.oid = t.relnamespace
             JOIN pg_index ix ON t.oid = ix.indrelid
             JOIN pg_class i ON i.oid = ix.indexrelid
             WHERE n.nspname = $1
               AND t.relname = $2
               AND ix.indisunique
               AND NOT ix.indisprimary
             ORDER BY i.relname",
            &[&namespace, &table_name],
        )
        .map_err(DatabaseError::from_engine)?;
    for row in unique_rows {
        let index_name: String = row.get("index_name");
        let column_rows = client
            .query(
                "SELECT a.attname
                 FROM pg_index i
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
                 JOIN pg_class c ON c.oid = i.indexrelid
                 WHERE c.relname = $1
                 ORDER BY array_position(i.indkey, a.attnum)",
                &[&index_name],
            )
            .map_err(DatabaseError::from_engine)?;
        if column_rows.is_empty() {
            continue;
        }
        return Ok(Some(
            column_rows
                .into_iter()
                .map(|row| row.get::<_, String>("attname"))
                .collect(),
        ));
    }
    Ok(None)
}

fn apply_change(
    tx: &mut Transaction<'_>,
    change: &StagedChange,
) -> Result<(), ApplyEngineError> {
    match change {
        StagedChange::Insert { table, values, .. } => {
            let sql = insert_sql(table, values);
            let params: Vec<SqlParam> = values
                .iter()
                .map(|(_, cell)| SqlParam::from_cell(cell))
                .collect();
            let param_refs = sql_param_refs(&params);
            tx.execute(&sql, &param_refs)
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
            let mut sql = format!("UPDATE {} SET ", table_sql(table));
            let mut params = Vec::new();
            for (index, (column, cell)) in new_values.iter().enumerate() {
                if index > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&quote_ident(column.as_str()));
                sql.push_str(" = $");
                params.push(SqlParam::from_cell(cell));
                sql.push_str(&params.len().to_string());
            }
            push_row_predicate(&mut sql, &mut params, identity, last_seen);
            let param_refs = sql_param_refs(&params);
            let changed = tx
                .execute(&sql, &param_refs)
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
            let mut sql = format!("DELETE FROM {}", table_sql(table));
            let mut params = Vec::new();
            push_row_predicate(&mut sql, &mut params, identity, last_seen);
            let param_refs = sql_param_refs(&params);
            let changed = tx
                .execute(&sql, &param_refs)
                .map_err(DatabaseError::from_engine)?;
            if changed != 1 {
                return Err(ApplyEngineError::Conflict);
            }
            Ok(())
        }
    }
}

fn insert_sql(table: &Table, values: &[(ColumnName, Cell)]) -> String {
    if values.is_empty() {
        return format!("INSERT INTO {} DEFAULT VALUES", table_sql(table));
    }
    let columns = values
        .iter()
        .map(|(column, _)| quote_ident(column.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=values.len())
        .map(|index| format!("${index}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_sql(table),
        columns,
        placeholders
    )
}

fn push_row_predicate(
    sql: &mut String,
    params: &mut Vec<SqlParam>,
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
                sql.push_str(" = $");
                params.push(SqlParam::from_cell(other));
                sql.push_str(&params.len().to_string());
            }
        }
    }
}
