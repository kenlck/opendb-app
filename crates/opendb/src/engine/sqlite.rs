use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};
use url::Url;

use super::{Database, DatabaseError};
use crate::table::TableName;

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
}
