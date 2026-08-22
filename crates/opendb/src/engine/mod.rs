mod common;
mod mysql;
mod postgres;
mod sqlite;

pub(crate) use common::quote_ident;

use crate::query::{QueryResult, SqlKind};
use crate::schema_change::SchemaChange;
use crate::staged::{RowIdentity, StagedChange};
use crate::table::Table;
use crate::table_page::{Cell, ColumnName, Filter, Page, TablePage};
use crate::table_structure::TableStructure;

pub(crate) use mysql::{MysqlDatabase, MysqlOpenError};
pub(crate) use postgres::{PostgresDatabase, PostgresOpenError};
pub(crate) use sqlite::{SqliteDatabase, SqliteOpenError};

pub(crate) trait Database {
    fn namespaces_grouped(&self) -> bool;
    fn list_tables(&self) -> Result<Vec<Table>, DatabaseError>;
    fn table_page(
        &self,
        table: &Table,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, DatabaseError>;
    fn table_structure(&self, table: &Table) -> Result<TableStructure, DatabaseError>;
    fn row_identity(
        &self,
        table: &Table,
        row: &[(ColumnName, Cell)],
    ) -> Result<Option<RowIdentity>, DatabaseError>;
    fn apply_staged(&self, changes: &[StagedChange]) -> Result<(), ApplyEngineError>;
    fn sql_kind(&self, sql: &str) -> Result<SqlKind, DatabaseError>;
    fn query(&self, sql: &str, page: Page) -> Result<QueryResult, DatabaseError>;
    fn execute_sql(&self, sql: &str) -> Result<(), DatabaseError>;
    fn schema_change_ddl(&self, change: &SchemaChange) -> String;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub(crate) struct DatabaseError {
    message: String,
}

impl DatabaseError {
    pub(crate) fn from_engine(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ApplyEngineError {
    #[error("a row changed since it was read")]
    Conflict,
    #[error("{0}")]
    Database(String),
}

impl From<DatabaseError> for ApplyEngineError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error.message)
    }
}
