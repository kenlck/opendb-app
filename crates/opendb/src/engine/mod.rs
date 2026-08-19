mod sqlite;

use crate::table::TableName;

pub(crate) use sqlite::{SqliteDatabase, SqliteOpenError};

pub(crate) trait Database {
    fn list_table_names(&self) -> Result<Vec<TableName>, DatabaseError>;
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
