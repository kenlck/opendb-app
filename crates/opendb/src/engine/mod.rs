mod sqlite;

use crate::staged::{RowIdentity, StagedChange};
use crate::table::TableName;
use crate::table_page::{Cell, ColumnName, Filter, Page, TablePage};

pub(crate) use sqlite::{SqliteDatabase, SqliteOpenError};

pub(crate) trait Database {
    fn list_table_names(&self) -> Result<Vec<TableName>, DatabaseError>;
    fn table_page(
        &self,
        table: &TableName,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, DatabaseError>;
    fn row_identity(
        &self,
        table: &TableName,
        row: &[(ColumnName, Cell)],
    ) -> Result<Option<RowIdentity>, DatabaseError>;
    fn apply_staged(&self, changes: &[StagedChange]) -> Result<(), ApplyEngineError>;
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
