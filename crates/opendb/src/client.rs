use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::catalog::{SystemCatalogPreference, assemble, classify};
use crate::connection::Connection;
use crate::connection_string::Engine;
use crate::engine::{Database, SqliteDatabase, SqliteOpenError};
use crate::name::Name;
use crate::table::TableCatalog;

pub struct Client {
    preferences_path: PathBuf,
    system_catalogs: SystemCatalogPreference,
    sessions: Vec<Session>,
    next_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

struct Session {
    id: SessionId,
    name: Name,
    database: Box<dyn Database>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseResult {
    Closed,
    AlreadyClosed,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpenError {
    #[error("Engine {0:?} is not supported yet")]
    UnsupportedEngine(Engine),
    #[error("Database file not found")]
    MissingFile,
    #[error("{0}")]
    Database(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("unknown Session")]
    UnknownSession,
    #[error("{0}")]
    Database(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PreferenceError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
struct PreferencesFile {
    #[serde(default)]
    system_catalogs: SystemCatalogPreference,
}

impl Client {
    pub fn open(preferences_path: impl Into<PathBuf>) -> Result<Self, PreferenceError> {
        let preferences_path = preferences_path.into();
        let system_catalogs = match fs::read_to_string(&preferences_path) {
            Ok(json) => serde_json::from_str::<PreferencesFile>(&json)?.system_catalogs,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                SystemCatalogPreference::Hidden
            }
            Err(error) => return Err(PreferenceError::Io(error)),
        };
        Ok(Self {
            preferences_path,
            system_catalogs,
            sessions: Vec::new(),
            next_id: 1,
        })
    }

    pub fn show_system_catalogs(&self) -> SystemCatalogPreference {
        self.system_catalogs
    }

    pub fn set_show_system_catalogs(
        &mut self,
        preference: SystemCatalogPreference,
    ) -> Result<(), PreferenceError> {
        self.system_catalogs = preference;
        if let Some(parent) = self.preferences_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&PreferencesFile {
            system_catalogs: preference,
        })?;
        let tmp = self.preferences_path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        #[cfg(windows)]
        if self.preferences_path.exists() {
            fs::remove_file(&self.preferences_path)?;
        }
        fs::rename(&tmp, &self.preferences_path)?;
        Ok(())
    }

    pub fn open_session(&mut self, connection: &Connection) -> Result<SessionId, OpenError> {
        let database: Box<dyn Database> = match connection.connection_string().engine() {
            Engine::Sqlite => {
                let opened = SqliteDatabase::open(connection.connection_string().as_str());
                match opened {
                    Ok(database) => Box::new(database),
                    Err(SqliteOpenError::MissingFile) => {
                        return Err(OpenError::MissingFile);
                    }
                    Err(SqliteOpenError::Driver(message)) => {
                        return Err(OpenError::Database(message));
                    }
                }
            }
            engine => return Err(OpenError::UnsupportedEngine(engine)),
        };
        let id = SessionId(self.next_id);
        self.next_id += 1;
        self.sessions.push(Session {
            id,
            name: connection.name().clone(),
            database,
        });
        Ok(id)
    }

    pub fn close_session(&mut self, id: SessionId) -> CloseResult {
        let before = self.sessions.len();
        self.sessions.retain(|session| session.id != id);
        if self.sessions.len() == before {
            CloseResult::AlreadyClosed
        } else {
            CloseResult::Closed
        }
    }

    pub fn session_name(&self, id: SessionId) -> Result<&Name, CatalogError> {
        Ok(&self.session(id)?.name)
    }

    pub fn tables(&self, id: SessionId) -> Result<TableCatalog, CatalogError> {
        let session = self.session(id)?;
        let names = session
            .database
            .list_table_names()
            .map_err(|error| CatalogError::Database(error.to_string()))?;
        Ok(assemble(
            names.into_iter().map(classify),
            self.system_catalogs,
        ))
    }

    fn session(&self, id: SessionId) -> Result<&Session, CatalogError> {
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .ok_or(CatalogError::UnknownSession)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_string::ConnectionString;

    fn connection_at(path: &std::path::Path) -> Connection {
        Connection::from_string(ConnectionString::parse(path.to_str().unwrap()).unwrap())
    }

    fn seed_users(path: &std::path::Path) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
                 INSERT INTO users (name) VALUES ('ken');",
            )
            .unwrap();
    }

    #[test]
    fn open_session_is_not_interned_by_connection_string() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        std::fs::write(&db_path, []).unwrap();
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let connection = connection_at(&db_path);
        let a = client.open_session(&connection).unwrap();
        let b = client.open_session(&connection).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn tables_hide_system_catalogs_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        seed_users(&db_path);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let catalog = client.tables(id).unwrap();
        let names: Vec<_> = catalog
            .tables()
            .iter()
            .map(|table| table.name().as_str())
            .collect();
        assert_eq!(names, ["users"]);
    }

    #[test]
    fn client_wide_toggle_reveals_system_catalogs_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        seed_users(&db_path);
        let prefs = dir.path().join("preferences.json");
        let mut client = Client::open(&prefs).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        client
            .set_show_system_catalogs(SystemCatalogPreference::Shown)
            .unwrap();
        let catalog = client.tables(id).unwrap();
        let mut names: Vec<_> = catalog
            .tables()
            .iter()
            .map(|table| table.name().as_str())
            .collect();
        names.sort();
        assert_eq!(names, ["sqlite_sequence", "users"]);
        let restarted = Client::open(&prefs).unwrap();
        assert_eq!(
            restarted.show_system_catalogs(),
            SystemCatalogPreference::Shown
        );
    }

    #[test]
    fn sqlite_catalog_is_flat() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
                 CREATE TABLE orders (id INTEGER PRIMARY KEY, total INTEGER);
                 INSERT INTO users (name) VALUES ('ken');",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let catalog = client.tables(id).unwrap();
        let mut names: Vec<_> = catalog
            .tables()
            .iter()
            .map(|table| table.name().as_str())
            .collect();
        names.sort();
        assert_eq!(names, ["orders", "users"]);
    }

    #[test]
    fn non_sqlite_engine_fails_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let connection = Connection::from_string(
            ConnectionString::parse("postgres://ken:pw@localhost/shop").unwrap(),
        );
        assert_eq!(
            client.open_session(&connection),
            Err(OpenError::UnsupportedEngine(Engine::Postgres))
        );
    }

    #[test]
    fn missing_sqlite_file_fails_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let connection = connection_at(&dir.path().join("no-such.db"));
        assert_eq!(
            client.open_session(&connection),
            Err(OpenError::MissingFile)
        );
    }
}
