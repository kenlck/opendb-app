use std::error::Error;
use std::sync::Mutex;

use postgres::types::{FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};
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

/// Display cell for any Postgres value. Accepts every OID so one unsupported
/// column cannot fail the whole row (and leave the Session grid on Loading).
struct DecodedCell(Cell);

impl<'a> FromSql<'a> for DecodedCell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self(decode_pg_value(ty, raw)))
    }

    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self(Cell::Null))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn cell_from_row(row: &Row, col: usize) -> Cell {
    match row.try_get::<_, DecodedCell>(col) {
        Ok(DecodedCell(cell)) => cell,
        Err(error) => {
            let type_name = row
                .columns()
                .get(col)
                .map(|column| column.type_().name())
                .unwrap_or("?");
            Cell::Text(format!("<{type_name}: {error}>"))
        }
    }
}

fn decode_pg_value(ty: &Type, raw: &[u8]) -> Cell {
    if let Kind::Array(inner) = ty.kind() {
        return decode_array(inner, raw);
    }
    if ty == &Type::INT2 {
        return i16::from_sql(ty, raw)
            .map(|value| Cell::Integer(i64::from(value)))
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::INT4 {
        return i32::from_sql(ty, raw)
            .map(|value| Cell::Integer(i64::from(value)))
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::INT8 {
        return i64::from_sql(ty, raw)
            .map(Cell::Integer)
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::OID {
        return u32::from_sql(ty, raw)
            .map(|value| Cell::Integer(i64::from(value)))
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::BOOL {
        return bool::from_sql(ty, raw)
            .map(|value| Cell::Integer(if value { 1 } else { 0 }))
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::FLOAT4 {
        return f32::from_sql(ty, raw)
            .map(|value| Cell::Real(f64::from(value)))
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::FLOAT8 {
        return f64::from_sql(ty, raw)
            .map(Cell::Real)
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::BYTEA {
        return Vec::<u8>::from_sql(ty, raw)
            .map(Cell::Blob)
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    if ty == &Type::UUID {
        return decode_uuid(raw).unwrap_or_else(|| fallback_bytes(ty, raw));
    }
    if ty == &Type::TIMESTAMP || ty == &Type::TIMESTAMPTZ {
        return decode_timestamp(raw, ty == &Type::TIMESTAMPTZ)
            .unwrap_or_else(|| fallback_bytes(ty, raw));
    }
    if ty == &Type::DATE {
        return decode_date(raw).unwrap_or_else(|| fallback_bytes(ty, raw));
    }
    if ty == &Type::TIME || ty == &Type::TIMETZ {
        return decode_time(raw).unwrap_or_else(|| fallback_bytes(ty, raw));
    }
    if ty == &Type::NUMERIC {
        return decode_numeric(raw).unwrap_or_else(|| fallback_bytes(ty, raw));
    }
    if ty == &Type::JSONB {
        return decode_jsonb(raw).unwrap_or_else(|| fallback_bytes(ty, raw));
    }
    if <String as FromSql>::accepts(ty) {
        return String::from_sql(ty, raw)
            .map(Cell::Text)
            .unwrap_or_else(|_| fallback_bytes(ty, raw));
    }
    fallback_bytes(ty, raw)
}

fn fallback_bytes(ty: &Type, raw: &[u8]) -> Cell {
    let _ = ty;
    match std::str::from_utf8(raw) {
        Ok(text)
            if !text.is_empty()
                && text
                    .chars()
                    .all(|ch| !ch.is_control() || ch == '\n' || ch == '\t') =>
        {
            Cell::Text(text.to_string())
        }
        _ => Cell::Text(format!("\\x{}", hex_encode(raw))),
    }
}

fn hex_encode(raw: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(raw.len() * 2);
    for byte in raw {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn decode_uuid(raw: &[u8]) -> Option<Cell> {
    if raw.len() != 16 {
        return None;
    }
    Some(Cell::Text(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        raw[0],
        raw[1],
        raw[2],
        raw[3],
        raw[4],
        raw[5],
        raw[6],
        raw[7],
        raw[8],
        raw[9],
        raw[10],
        raw[11],
        raw[12],
        raw[13],
        raw[14],
        raw[15]
    )))
}

/// Postgres timestamp / timestamptz binary: i64 microseconds since 2000-01-01.
fn decode_timestamp(raw: &[u8], with_tz: bool) -> Option<Cell> {
    if raw.len() != 8 {
        return None;
    }
    let micros = i64::from_be_bytes(raw.try_into().ok()?);
    // 2000-01-01T00:00:00Z as Unix microseconds.
    const PG_EPOCH_UNIX_MICROS: i64 = 946_684_800 * 1_000_000;
    let unix_micros = micros.checked_add(PG_EPOCH_UNIX_MICROS)?;
    let (secs, rem_micros) = div_mod_floor(unix_micros, 1_000_000);
    let (days, day_secs) = div_mod_floor(secs, 86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;
    let mut text = format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    );
    if rem_micros != 0 {
        text.push_str(&format!(".{rem_micros:06}"));
    }
    if with_tz {
        text.push_str("+00");
    }
    Some(Cell::Text(text))
}

/// Postgres date binary: i32 days since 2000-01-01.
fn decode_date(raw: &[u8]) -> Option<Cell> {
    if raw.len() != 4 {
        return None;
    }
    let days = i32::from_be_bytes(raw.try_into().ok()?);
    // Days from Unix epoch (1970-01-01) to Postgres epoch (2000-01-01).
    const PG_EPOCH_UNIX_DAYS: i64 = 10_957;
    let (year, month, day) = civil_from_days(i64::from(days) + PG_EPOCH_UNIX_DAYS);
    Some(Cell::Text(format!("{year:04}-{month:02}-{day:02}")))
}

fn decode_time(raw: &[u8]) -> Option<Cell> {
    // TIME is i64 microseconds; TIMETZ is i64 micros + i32 tz offset seconds.
    if raw.len() != 8 && raw.len() != 12 {
        return None;
    }
    let micros = i64::from_be_bytes(raw[..8].try_into().ok()?);
    let (secs, rem_micros) = div_mod_floor(micros, 1_000_000);
    let hour = secs / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;
    let mut text = format!("{hour:02}:{minute:02}:{second:02}");
    if rem_micros != 0 {
        text.push_str(&format!(".{rem_micros:06}"));
    }
    if raw.len() == 12 {
        let offset = i32::from_be_bytes(raw[8..12].try_into().ok()?);
        let sign = if offset <= 0 { '+' } else { '-' };
        let abs = offset.unsigned_abs();
        text.push(sign);
        text.push_str(&format!("{:02}:{:02}", abs / 3600, (abs % 3600) / 60));
    }
    Some(Cell::Text(text))
}

fn decode_jsonb(raw: &[u8]) -> Option<Cell> {
    if raw.is_empty() {
        return None;
    }
    // JSONB values are version byte 1 followed by UTF-8 JSON text.
    let payload = if raw[0] == 1 { &raw[1..] } else { raw };
    std::str::from_utf8(payload)
        .ok()
        .map(|text| Cell::Text(text.to_string()))
}

/// Minimal NUMERIC binary decoder (base-10000 digits). Enough for grid display.
fn decode_numeric(raw: &[u8]) -> Option<Cell> {
    if raw.len() < 8 {
        return None;
    }
    let ndigits = i16::from_be_bytes(raw[0..2].try_into().ok()?) as usize;
    let weight = i16::from_be_bytes(raw[2..4].try_into().ok()?);
    let sign = u16::from_be_bytes(raw[4..6].try_into().ok()?);
    let dscale = u16::from_be_bytes(raw[6..8].try_into().ok()?) as usize;
    const NUMERIC_NAN: u16 = 0xC000;
    const NUMERIC_NEG: u16 = 0x4000;
    if sign == NUMERIC_NAN {
        return Some(Cell::Text("NaN".into()));
    }
    if raw.len() < 8 + ndigits * 2 {
        return None;
    }
    let mut digits = Vec::with_capacity(ndigits);
    for index in 0..ndigits {
        let start = 8 + index * 2;
        digits.push(i16::from_be_bytes(raw[start..start + 2].try_into().ok()?));
    }
    let mut int_part = String::new();
    let mut frac_part = String::new();
    for (index, digit) in digits.iter().enumerate() {
        let pos = weight - index as i16;
        let chunk = format!("{digit:04}");
        if pos >= 0 {
            if int_part.is_empty() {
                int_part.push_str(chunk.trim_start_matches('0'));
                if int_part.is_empty() {
                    int_part.push('0');
                }
            } else {
                int_part.push_str(&chunk);
            }
        } else {
            frac_part.push_str(&chunk);
        }
    }
    if int_part.is_empty() {
        int_part.push('0');
    }
    // Weight can leave leading fractional digits before the first stored digit.
    if weight < -1 {
        let leading = ((-1 - weight) as usize).saturating_mul(4);
        frac_part = format!("{:0>width$}", frac_part, width = leading + frac_part.len());
    }
    if dscale > 0 {
        while frac_part.len() < dscale {
            frac_part.push('0');
        }
        frac_part.truncate(dscale);
    } else {
        frac_part.clear();
    }
    let mut text = if sign == NUMERIC_NEG {
        format!("-{int_part}")
    } else {
        int_part
    };
    if dscale > 0 {
        text.push('.');
        text.push_str(&frac_part);
    }
    Some(Cell::Text(text))
}

fn decode_array(inner: &Type, raw: &[u8]) -> Cell {
    // 1-D array header: ndim, has_null, element_oid, dim, lbound, then values.
    if raw.len() < 20 {
        return Cell::Text(format!("[{}]", inner.name()));
    }
    let ndim = i32::from_be_bytes(match raw[0..4].try_into() {
        Ok(bytes) => bytes,
        Err(_) => return Cell::Text(format!("[{}]", inner.name())),
    });
    if ndim != 1 {
        return Cell::Text(format!("[{}]", inner.name()));
    }
    let has_null = i32::from_be_bytes(raw[4..8].try_into().unwrap_or([0; 4])) != 0;
    let dim = i32::from_be_bytes(raw[12..16].try_into().unwrap_or([0; 4]));
    if dim < 0 {
        return Cell::Text("{}".into());
    }
    let mut offset = 20;
    let mut parts = Vec::with_capacity(dim as usize);
    for _ in 0..dim {
        if offset + 4 > raw.len() {
            return Cell::Text(format!("[{}]", inner.name()));
        }
        let Ok(len_bytes) = raw[offset..offset + 4].try_into() else {
            return Cell::Text(format!("[{}]", inner.name()));
        };
        let len = i32::from_be_bytes(len_bytes);
        offset += 4;
        if len < 0 {
            if !has_null && len != -1 {
                return Cell::Text(format!("[{}]", inner.name()));
            }
            parts.push("NULL".to_string());
            continue;
        }
        let len = len as usize;
        if offset + len > raw.len() {
            return Cell::Text(format!("[{}]", inner.name()));
        }
        let cell = decode_pg_value(inner, &raw[offset..offset + len]);
        offset += len;
        parts.push(match cell {
            Cell::Null => "NULL".into(),
            Cell::Integer(v) => v.to_string(),
            Cell::Real(v) => v.to_string(),
            Cell::Text(v) => v,
            Cell::Blob(v) => format!("\\x{}", hex_encode(&v)),
        });
    }
    Cell::Text(format!("{{{}}}", parts.join(",")))
}

const UNIX_EPOCH_SHIFT: i64 = 719_468; // civil_from_days uses days since 1970-01-01

fn div_mod_floor(value: i64, divisor: i64) -> (i64, i64) {
    let mut quot = value / divisor;
    let mut rem = value % divisor;
    if rem < 0 {
        quot -= 1;
        rem += divisor;
    }
    (quot, rem)
}

/// Howard Hinnant civil_from_days: Unix days since 1970-01-01 → (y, m, d).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + UNIX_EPOCH_SHIFT;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = era * 400 + yoe as i64;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

#[derive(Debug)]
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
}

impl ToSql for SqlParam {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            SqlParam::Null => Ok(IsNull::Yes),
            SqlParam::Integer(value) => {
                if ty == &Type::INT2 {
                    return i16::try_from(*value)?.to_sql(ty, out);
                }
                if ty == &Type::INT4 {
                    return i32::try_from(*value)?.to_sql(ty, out);
                }
                if ty == &Type::OID {
                    return u32::try_from(*value)?.to_sql(ty, out);
                }
                // INT8, and unbound params such as LIMIT/OFFSET.
                value.to_sql(&Type::INT8, out)
            }
            SqlParam::Real(value) => {
                if ty == &Type::FLOAT4 {
                    return (*value as f32).to_sql(ty, out);
                }
                value.to_sql(&Type::FLOAT8, out)
            }
            SqlParam::Text(value) => value.to_sql(ty, out),
            SqlParam::Blob(value) => value.to_sql(ty, out),
        }
    }

    fn accepts(ty: &Type) -> bool {
        matches!(
            *ty,
            Type::INT2
                | Type::INT4
                | Type::INT8
                | Type::OID
                | Type::FLOAT4
                | Type::FLOAT8
                | Type::BYTEA
                | Type::TEXT
                | Type::VARCHAR
                | Type::BPCHAR
                | Type::NAME
                | Type::UNKNOWN
                | Type::JSON
                | Type::JSONB
                | Type::UUID
                | Type::NUMERIC
                | Type::TIMESTAMP
                | Type::TIMESTAMPTZ
                | Type::DATE
                | Type::TIME
                | Type::TIMETZ
                | Type::BOOL
        ) || <String as ToSql>::accepts(ty)
            || <Vec<u8> as ToSql>::accepts(ty)
    }

    to_sql_checked!();
}

fn sql_param_refs(params: &[SqlParam]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|param| param as &(dyn ToSql + Sync)).collect()
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
            cells.push(cell_from_row(&row, index));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_bytes_decode_to_text() {
        let raw = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        assert_eq!(
            decode_uuid(&raw),
            Some(Cell::Text("550e8400-e29b-41d4-a716-446655440000".into()))
        );
    }

    #[test]
    fn timestamp_bytes_decode_to_text() {
        // 2020-01-02 03:04:05 UTC → micros since 2000-01-01
        // Unix 1577934245 - 946684800 = 631249445 seconds = 631249445000000 micros
        let micros: i64 = 631_249_445_000_000;
        let raw = micros.to_be_bytes();
        assert_eq!(
            decode_timestamp(&raw, true),
            Some(Cell::Text("2020-01-02 03:04:05+00".into()))
        );
    }

    #[test]
    fn jsonb_strips_version_byte() {
        let mut raw = vec![1];
        raw.extend_from_slice(br#"{"ok":true}"#);
        assert_eq!(
            decode_jsonb(&raw),
            Some(Cell::Text(r#"{"ok":true}"#.into()))
        );
    }

    #[test]
    fn numeric_decodes_common_values() {
        // 123.45 → ndigits=2, weight=0, sign=0, dscale=2, digits=[123, 4500]
        let mut raw = Vec::new();
        raw.extend_from_slice(&2i16.to_be_bytes());
        raw.extend_from_slice(&0i16.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        raw.extend_from_slice(&2u16.to_be_bytes());
        raw.extend_from_slice(&123i16.to_be_bytes());
        raw.extend_from_slice(&4500i16.to_be_bytes());
        assert_eq!(decode_numeric(&raw), Some(Cell::Text("123.45".into())));
    }

    #[test]
    fn unknown_binary_falls_back_to_hex_text() {
        let cell = fallback_bytes(&Type::UNKNOWN, &[0x00, 0xff]);
        assert_eq!(cell, Cell::Text("\\x00ff".into()));
    }

    #[test]
    fn civil_from_days_matches_unix_epoch_and_y2k() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }
}
