mod sqlite;

use crate::table::TableName;
use crate::table_page::{Filter, Page, TablePage};

pub(crate) use sqlite::{SqliteDatabase, SqliteOpenError};

pub(crate) trait Database {
    fn list_table_names(&self) -> Result<Vec<TableName>, DatabaseError>;
    fn table_page(
        &self,
        table: &TableName,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, DatabaseError>;
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
