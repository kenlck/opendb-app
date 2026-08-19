use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::catalog::{SystemCatalogPreference, assemble, classify};
use crate::connection::Connection;
use crate::connection_string::Engine;
use crate::engine::{Database, SqliteDatabase, SqliteOpenError};
use crate::name::Name;
use crate::table::{TableCatalog, TableName};
use crate::table_page::{Filter, Page, TablePage};

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

    pub fn table_page(
        &self,
        id: SessionId,
        table: &TableName,
        filters: &[Filter],
        page: Page,
    ) -> Result<TablePage, CatalogError> {
        let session = self.session(id)?;
        session
            .database
            .table_page(table, filters, page)
            .map_err(|error| CatalogError::Database(error.to_string()))
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
    use crate::table::TableName;
    use crate::table_page::{Cell, Filter, Page};

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

    #[test]
    fn table_page_is_bounded_and_has_next() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
                [],
            )
            .unwrap();
        for index in 1..=101 {
            connection
                .execute(
                    "INSERT INTO users (name) VALUES (?)",
                    [format!("user-{index:03}")],
                )
                .unwrap();
        }
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let page = client
            .table_page(id, &TableName::new("users".into()), &[], Page::first())
            .unwrap();
        assert_eq!(
            page.columns()
                .iter()
                .map(|column| column.as_str())
                .collect::<Vec<_>>(),
            ["id", "name"]
        );
        assert_eq!(page.rows().len(), 100);
        assert_eq!(
            page.rows()[0],
            [Cell::Integer(1), Cell::Text("user-001".into())]
        );
        assert_eq!(
            page.rows()[99],
            [Cell::Integer(100), Cell::Text("user-100".into())]
        );
        assert!(page.has_next());
        let next = client
            .table_page(
                id,
                &TableName::new("users".into()),
                &[],
                Page::first().next(),
            )
            .unwrap();
        assert_eq!(next.rows().len(), 1);
        assert_eq!(
            next.rows()[0],
            [Cell::Integer(101), Cell::Text("user-101".into())]
        );
        assert!(!next.has_next());
    }

    #[test]
    fn next_page_uses_the_current_filters() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, city TEXT)",
                [],
            )
            .unwrap();
        for index in 1..=101 {
            connection
                .execute(
                    "INSERT INTO users (name, city) VALUES (?, 'keep')",
                    [format!("keep-{index:03}")],
                )
                .unwrap();
        }
        for index in 1..=50 {
            connection
                .execute(
                    "INSERT INTO users (name, city) VALUES (?, 'drop')",
                    [format!("drop-{index:03}")],
                )
                .unwrap();
        }
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let filters = [Filter::equals("city", Cell::Text("keep".into()))];
        let first = client
            .table_page(id, &TableName::new("users".into()), &filters, Page::first())
            .unwrap();
        assert_eq!(first.rows().len(), 100);
        assert_eq!(
            first.rows()[0],
            [
                Cell::Integer(1),
                Cell::Text("keep-001".into()),
                Cell::Text("keep".into())
            ]
        );
        assert_eq!(
            first.rows()[99],
            [
                Cell::Integer(100),
                Cell::Text("keep-100".into()),
                Cell::Text("keep".into())
            ]
        );
        assert!(first.has_next());
        let second = client
            .table_page(
                id,
                &TableName::new("users".into()),
                &filters,
                Page::first().next(),
            )
            .unwrap();
        assert_eq!(second.rows().len(), 1);
        assert_eq!(
            second.rows()[0],
            [
                Cell::Integer(101),
                Cell::Text("keep-101".into()),
                Cell::Text("keep".into())
            ]
        );
        assert!(!second.has_next());
    }

    #[test]
    fn equals_contains_and_null_filters_combine() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, note TEXT, flag TEXT);
                 INSERT INTO items (id, name, note, flag) VALUES
                    (1, 'apple', 'pie', NULL),
                    (2, 'apple', 'tart', 'x'),
                    (3, 'banana', 'pie', NULL),
                    (4, 'apple pie', 'pie', NULL);",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let page = client
            .table_page(
                id,
                &TableName::new("items".into()),
                &[
                    Filter::equals("name", Cell::Text("apple".into())),
                    Filter::contains("note", "pi"),
                    Filter::is_null("flag"),
                ],
                Page::first(),
            )
            .unwrap();
        assert_eq!(
            page.rows(),
            [[
                Cell::Integer(1),
                Cell::Text("apple".into()),
                Cell::Text("pie".into()),
                Cell::Null
            ]]
        );
        assert!(!page.has_next());
    }

    #[test]
    fn table_without_row_identity_still_pages() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute("CREATE TABLE notes (body TEXT, tag TEXT)", [])
            .unwrap();
        for index in 1..=101 {
            connection
                .execute(
                    "INSERT INTO notes (body, tag) VALUES (?, 'a')",
                    [format!("note-{index:03}")],
                )
                .unwrap();
        }
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let page = client
            .table_page(id, &TableName::new("notes".into()), &[], Page::first())
            .unwrap();
        assert_eq!(
            page.columns()
                .iter()
                .map(|column| column.as_str())
                .collect::<Vec<_>>(),
            ["body", "tag"]
        );
        assert_eq!(page.rows().len(), 100);
        assert_eq!(
            page.rows()[0],
            [Cell::Text("note-001".into()), Cell::Text("a".into())]
        );
        assert_eq!(
            page.rows()[99],
            [Cell::Text("note-100".into()), Cell::Text("a".into())]
        );
        assert!(page.has_next());
    }

    #[test]
    fn filters_are_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE wide (
                    c00 TEXT, c01 TEXT, c02 TEXT, c03 TEXT,
                    c04 TEXT, c05 TEXT, c06 TEXT, c07 TEXT,
                    c08 TEXT, c09 TEXT, c10 TEXT, c11 TEXT
                 );
                 INSERT INTO wide VALUES (
                    'a','b','c','d','e','f','g','h','i','j','k','l'
                 );
                 INSERT INTO wide VALUES (
                    'a','b','c','d','e','f','g','h','i','j','k','NO'
                 );",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let filters = [
            Filter::equals("c00", Cell::Text("a".into())),
            Filter::equals("c01", Cell::Text("b".into())),
            Filter::equals("c02", Cell::Text("c".into())),
            Filter::equals("c03", Cell::Text("d".into())),
            Filter::equals("c04", Cell::Text("e".into())),
            Filter::equals("c05", Cell::Text("f".into())),
            Filter::equals("c06", Cell::Text("g".into())),
            Filter::equals("c07", Cell::Text("h".into())),
            Filter::equals("c08", Cell::Text("i".into())),
            Filter::equals("c09", Cell::Text("j".into())),
            Filter::equals("c10", Cell::Text("k".into())),
            Filter::equals("c11", Cell::Text("l".into())),
        ];
        let page = client
            .table_page(id, &TableName::new("wide".into()), &filters, Page::first())
            .unwrap();
        assert_eq!(
            page.rows(),
            [[
                Cell::Text("a".into()),
                Cell::Text("b".into()),
                Cell::Text("c".into()),
                Cell::Text("d".into()),
                Cell::Text("e".into()),
                Cell::Text("f".into()),
                Cell::Text("g".into()),
                Cell::Text("h".into()),
                Cell::Text("i".into()),
                Cell::Text("j".into()),
                Cell::Text("k".into()),
                Cell::Text("l".into()),
            ]]
        );
        assert!(!page.has_next());
    }
}
