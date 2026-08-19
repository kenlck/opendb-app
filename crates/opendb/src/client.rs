use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::catalog::{SystemCatalogPreference, assemble, classify};
use crate::connection::Connection;
use crate::connection_string::Engine;
use crate::engine::{ApplyEngineError, Database, SqliteDatabase, SqliteOpenError};
use crate::name::Name;
use crate::query::{ExecuteError, QueryResult, SqlKind};
use crate::staged::{ApplyError, StageError, StagedChange, StagedChangeId};
use crate::table::{TableCatalog, TableName};
use crate::table_page::{Cell, ColumnName, Filter, Page, TablePage};

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
    staged: Vec<StagedChange>,
    next_change_id: u64,
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
            staged: Vec::new(),
            next_change_id: 1,
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

    pub fn sql_kind(&self, id: SessionId, sql: &str) -> Result<SqlKind, ExecuteError> {
        let session = self.session(id).map_err(|_| ExecuteError::UnknownSession)?;
        session
            .database
            .sql_kind(sql)
            .map_err(|error| ExecuteError::Database(error.to_string()))
    }

    pub fn query(&self, id: SessionId, sql: &str, page: Page) -> Result<QueryResult, ExecuteError> {
        let session = self.session(id).map_err(|_| ExecuteError::UnknownSession)?;
        match session
            .database
            .sql_kind(sql)
            .map_err(|error| ExecuteError::Database(error.to_string()))?
        {
            SqlKind::Read => session
                .database
                .query(sql, page)
                .map_err(|error| ExecuteError::Database(error.to_string())),
            SqlKind::Mutating => Err(ExecuteError::MutatingSql),
        }
    }

    pub fn execute_mutating(&self, id: SessionId, sql: &str) -> Result<(), ExecuteError> {
        let session = self.session(id).map_err(|_| ExecuteError::UnknownSession)?;
        match session
            .database
            .sql_kind(sql)
            .map_err(|error| ExecuteError::Database(error.to_string()))?
        {
            SqlKind::Read => return Err(ExecuteError::ReadSql),
            SqlKind::Mutating => {}
        }
        if !session.staged.is_empty() {
            return Err(ExecuteError::StagedChangesExist);
        }
        session
            .database
            .execute_sql(sql)
            .map_err(|error| ExecuteError::Database(error.to_string()))
    }

    pub fn stage_insert(
        &mut self,
        id: SessionId,
        table: TableName,
        values: Vec<(ColumnName, Cell)>,
    ) -> Result<StagedChangeId, StageError> {
        let session = self.session_mut(id)?;
        let change_id = StagedChangeId::new(session.next_change_id);
        session.next_change_id += 1;
        session.staged.push(StagedChange::Insert {
            id: change_id,
            table,
            values,
        });
        Ok(change_id)
    }

    pub fn stage_update(
        &mut self,
        id: SessionId,
        table: TableName,
        last_seen: Vec<(ColumnName, Cell)>,
        new_values: Vec<(ColumnName, Cell)>,
    ) -> Result<StagedChangeId, StageError> {
        let session = self.session_mut(id)?;
        let identity = session
            .database
            .row_identity(&table, &last_seen)
            .map_err(|error| StageError::Database(error.to_string()))?
            .ok_or(StageError::MissingRowIdentity)?;
        let change_id = StagedChangeId::new(session.next_change_id);
        session.next_change_id += 1;
        session.staged.push(StagedChange::Update {
            id: change_id,
            table,
            identity,
            last_seen,
            new_values,
        });
        Ok(change_id)
    }

    pub fn stage_delete(
        &mut self,
        id: SessionId,
        table: TableName,
        last_seen: Vec<(ColumnName, Cell)>,
    ) -> Result<StagedChangeId, StageError> {
        let session = self.session_mut(id)?;
        let identity = session
            .database
            .row_identity(&table, &last_seen)
            .map_err(|error| StageError::Database(error.to_string()))?
            .ok_or(StageError::MissingRowIdentity)?;
        let change_id = StagedChangeId::new(session.next_change_id);
        session.next_change_id += 1;
        session.staged.push(StagedChange::Delete {
            id: change_id,
            table,
            identity,
            last_seen,
        });
        Ok(change_id)
    }

    pub fn unstage(&mut self, id: SessionId, change: StagedChangeId) -> Result<(), StageError> {
        let session = self.session_mut(id)?;
        session.staged.retain(|staged| staged.id() != change);
        Ok(())
    }

    pub fn discard_staged_changes(&mut self, id: SessionId) -> Result<(), StageError> {
        self.session_mut(id)?.staged.clear();
        Ok(())
    }

    pub fn staged_changes(&self, id: SessionId) -> Result<&[StagedChange], CatalogError> {
        Ok(&self.session(id)?.staged)
    }

    pub fn apply(&mut self, id: SessionId) -> Result<(), ApplyError> {
        let session = self
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(ApplyError::UnknownSession)?;
        session
            .database
            .apply_staged(&session.staged)
            .map_err(|error| match error {
                ApplyEngineError::Conflict => ApplyError::Conflict,
                ApplyEngineError::Database(message) => ApplyError::Database(message),
            })?;
        session.staged.clear();
        Ok(())
    }

    fn session(&self, id: SessionId) -> Result<&Session, CatalogError> {
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .ok_or(CatalogError::UnknownSession)
    }

    fn session_mut(&mut self, id: SessionId) -> Result<&mut Session, StageError> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(StageError::UnknownSession)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_string::ConnectionString;
    use crate::query::{ExecuteError, ResultStaging, SqlKind};
    use crate::staged::{ApplyError, StageError, StagedChange};
    use crate::table::TableName;
    use crate::table_page::{Cell, ColumnName, Filter, Page, TablePage};

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

    fn users() -> TableName {
        TableName::new("users".into())
    }

    fn notes() -> TableName {
        TableName::new("notes".into())
    }

    fn named_row(page: &TablePage, index: usize) -> Vec<(ColumnName, Cell)> {
        page.columns()
            .iter()
            .cloned()
            .zip(page.rows()[index].iter().cloned())
            .collect()
    }

    fn open_seeded_client(dir: &tempfile::TempDir) -> (Client, SessionId, std::path::PathBuf) {
        let db_path = dir.path().join("shop.db");
        seed_users(&db_path);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        (client, id, db_path)
    }

    #[test]
    fn stage_insert_update_delete_do_not_write_until_apply() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
                 INSERT INTO users (name) VALUES ('ken');
                 INSERT INTO users (name) VALUES ('ada');",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let page = client.table_page(id, &users(), &[], Page::first()).unwrap();
        client
            .stage_update(
                id,
                users(),
                named_row(&page, 0),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        client
            .stage_delete(id, users(), named_row(&page, 1))
            .unwrap();
        client
            .stage_insert(
                id,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("bob".into()))],
            )
            .unwrap();
        let staged = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(
            staged.rows(),
            [
                [Cell::Integer(1), Cell::Text("ken".into())],
                [Cell::Integer(2), Cell::Text("ada".into())]
            ]
        );
        client.apply(id).unwrap();
        let applied = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(
            applied.rows(),
            [
                [Cell::Integer(1), Cell::Text("zoe".into())],
                [Cell::Integer(3), Cell::Text("bob".into())]
            ]
        );
        assert!(client.staged_changes(id).unwrap().is_empty());
    }

    #[test]
    fn update_and_delete_require_row_identity_insert_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
                 INSERT INTO users (name) VALUES ('ken');
                 CREATE TABLE notes (body TEXT, tag TEXT);
                 INSERT INTO notes (body, tag) VALUES ('hello', 'a');",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let notes_page = client.table_page(id, &notes(), &[], Page::first()).unwrap();
        let insert_id = client
            .stage_insert(
                id,
                notes(),
                vec![(ColumnName::new("body".into()), Cell::Text("world".into()))],
            )
            .unwrap();
        assert_eq!(client.staged_changes(id).unwrap()[0].id(), insert_id);
        assert_eq!(
            client.stage_update(
                id,
                notes(),
                named_row(&notes_page, 0),
                vec![(ColumnName::new("body".into()), Cell::Text("nope".into()))],
            ),
            Err(StageError::MissingRowIdentity)
        );
        assert_eq!(
            client.stage_delete(id, notes(), named_row(&notes_page, 0)),
            Err(StageError::MissingRowIdentity)
        );
        let users_page = client.table_page(id, &users(), &[], Page::first()).unwrap();
        client
            .stage_update(
                id,
                users(),
                named_row(&users_page, 0),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        client.apply(id).unwrap();
        let notes_after = client.table_page(id, &notes(), &[], Page::first()).unwrap();
        assert_eq!(
            notes_after.rows(),
            [
                [Cell::Text("hello".into()), Cell::Text("a".into())],
                [Cell::Text("world".into()), Cell::Null]
            ]
        );
        let users_after = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(
            users_after.rows(),
            [[Cell::Integer(1), Cell::Text("zoe".into())]]
        );
    }

    #[test]
    fn unstage_one_leaves_the_rest_and_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, id, _) = open_seeded_client(&dir);
        let page = client.table_page(id, &users(), &[], Page::first()).unwrap();
        let update = client
            .stage_update(
                id,
                users(),
                named_row(&page, 0),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        let insert = client
            .stage_insert(
                id,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("bob".into()))],
            )
            .unwrap();
        client.unstage(id, update).unwrap();
        let remaining: Vec<_> = client
            .staged_changes(id)
            .unwrap()
            .iter()
            .map(StagedChange::id)
            .collect();
        assert_eq!(remaining, [insert]);
        let still = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(still.rows(), [[Cell::Integer(1), Cell::Text("ken".into())]]);
    }

    #[test]
    fn discard_clears_the_bag_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, id, _) = open_seeded_client(&dir);
        let page = client.table_page(id, &users(), &[], Page::first()).unwrap();
        client
            .stage_update(
                id,
                users(),
                named_row(&page, 0),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        client
            .stage_insert(
                id,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("bob".into()))],
            )
            .unwrap();
        client.discard_staged_changes(id).unwrap();
        assert!(client.staged_changes(id).unwrap().is_empty());
        let still = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(still.rows(), [[Cell::Integer(1), Cell::Text("ken".into())]]);
    }

    #[test]
    fn apply_is_one_transaction_and_rolls_back_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE);
                 INSERT INTO users (name) VALUES ('ken');",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        client
            .stage_insert(
                id,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("ada".into()))],
            )
            .unwrap();
        client
            .stage_insert(
                id,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("ken".into()))],
            )
            .unwrap();
        assert!(client.apply(id).is_err());
        let still = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(still.rows(), [[Cell::Integer(1), Cell::Text("ken".into())]]);
        assert_eq!(client.staged_changes(id).unwrap().len(), 2);
    }

    #[test]
    fn apply_rolls_back_when_row_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, id, db_path) = open_seeded_client(&dir);
        let page = client.table_page(id, &users(), &[], Page::first()).unwrap();
        client
            .stage_update(
                id,
                users(),
                named_row(&page, 0),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute("UPDATE users SET name = 'other' WHERE id = 1", [])
            .unwrap();
        drop(connection);
        assert_eq!(client.apply(id), Err(ApplyError::Conflict));
        let still = client.table_page(id, &users(), &[], Page::first()).unwrap();
        assert_eq!(
            still.rows(),
            [[Cell::Integer(1), Cell::Text("other".into())]]
        );
        assert_eq!(client.staged_changes(id).unwrap().len(), 1);
    }

    #[test]
    fn two_sessions_on_the_same_connection_have_independent_bags() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        seed_users(&db_path);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let connection = connection_at(&db_path);
        let a = client.open_session(&connection).unwrap();
        let b = client.open_session(&connection).unwrap();
        let page = client.table_page(a, &users(), &[], Page::first()).unwrap();
        client
            .stage_update(
                a,
                users(),
                named_row(&page, 0),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        client
            .stage_insert(
                b,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("bob".into()))],
            )
            .unwrap();
        assert_eq!(client.staged_changes(a).unwrap().len(), 1);
        assert_eq!(client.staged_changes(b).unwrap().len(), 1);
        client.apply(a).unwrap();
        assert!(client.staged_changes(a).unwrap().is_empty());
        assert_eq!(client.staged_changes(b).unwrap().len(), 1);
        let after_a = client.table_page(b, &users(), &[], Page::first()).unwrap();
        assert_eq!(
            after_a.rows(),
            [[Cell::Integer(1), Cell::Text("zoe".into())]]
        );
        client.apply(b).unwrap();
        let after_b = client.table_page(a, &users(), &[], Page::first()).unwrap();
        assert_eq!(
            after_b.rows(),
            [
                [Cell::Integer(1), Cell::Text("zoe".into())],
                [Cell::Integer(2), Cell::Text("bob".into())]
            ]
        );
    }

    #[test]
    fn read_query_runs_immediately_and_is_paged() {
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
        assert_eq!(
            client.sql_kind(id, "SELECT id, name FROM users ORDER BY id"),
            Ok(SqlKind::Read)
        );
        let catalog = client.tables(id).unwrap();
        let names: Vec<_> = catalog
            .tables()
            .iter()
            .map(|table| table.name().as_str())
            .collect();
        assert_eq!(names, ["users"]);
        let page = client
            .query(id, "SELECT id, name FROM users ORDER BY id", Page::first())
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
            .query(
                id,
                "SELECT id, name FROM users ORDER BY id",
                Page::first().next(),
            )
            .unwrap();
        assert_eq!(next.rows().len(), 1);
        assert_eq!(
            next.rows()[0],
            [Cell::Integer(101), Cell::Text("user-101".into())]
        );
        assert!(!next.has_next());
        let catalogs = client
            .query(
                id,
                "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
                Page::first(),
            )
            .unwrap();
        assert_eq!(
            catalogs.rows(),
            [
                [Cell::Text("sqlite_sequence".into())],
                [Cell::Text("users".into())]
            ]
        );
        assert_eq!(catalogs.staging(), &ResultStaging::ReadOnly);
    }

    #[test]
    fn join_and_aggregate_results_are_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
                 CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total INTEGER);
                 INSERT INTO users (name) VALUES ('ken');
                 INSERT INTO orders (id, user_id, total) VALUES (1, 1, 9);",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        let joined = client
            .query(
                id,
                "SELECT users.id, orders.total FROM users JOIN orders ON users.id = orders.user_id",
                Page::first(),
            )
            .unwrap();
        assert_eq!(joined.rows(), [[Cell::Integer(1), Cell::Integer(9)]]);
        assert_eq!(joined.staging(), &ResultStaging::ReadOnly);
        let aggregated = client
            .query(id, "SELECT COUNT(*) FROM users", Page::first())
            .unwrap();
        assert_eq!(aggregated.rows(), [[Cell::Integer(1)]]);
        assert_eq!(aggregated.staging(), &ResultStaging::ReadOnly);
        let missing_identity = client
            .query(id, "SELECT name FROM users", Page::first())
            .unwrap();
        assert_eq!(missing_identity.rows(), [[Cell::Text("ken".into())]]);
        assert_eq!(missing_identity.staging(), &ResultStaging::ReadOnly);
    }

    #[test]
    fn one_table_result_with_row_identity_can_stage() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, id, _) = open_seeded_client(&dir);
        let result = client
            .query(id, "SELECT id, name FROM users ORDER BY id", Page::first())
            .unwrap();
        assert_eq!(result.staging(), &ResultStaging::Staged { table: users() });
        assert_eq!(
            result.rows(),
            [[Cell::Integer(1), Cell::Text("ken".into())]]
        );
        let ResultStaging::Staged { table } = result.staging().clone() else {
            panic!("expected staged Result");
        };
        client
            .stage_update(
                id,
                table,
                named_row_from(result.columns(), &result.rows()[0]),
                vec![(ColumnName::new("name".into()), Cell::Text("zoe".into()))],
            )
            .unwrap();
        client.apply(id).unwrap();
        let after = client
            .query(id, "SELECT id, name FROM users ORDER BY id", Page::first())
            .unwrap();
        assert_eq!(after.rows(), [[Cell::Integer(1), Cell::Text("zoe".into())]]);
        assert!(client.staged_changes(id).unwrap().is_empty());
    }

    fn named_row_from(columns: &[ColumnName], row: &[Cell]) -> Vec<(ColumnName, Cell)> {
        columns.iter().cloned().zip(row.iter().cloned()).collect()
    }

    #[test]
    fn mutating_sql_is_refused_while_staged_changes_exist() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, id, _) = open_seeded_client(&dir);
        client
            .stage_insert(
                id,
                users(),
                vec![(ColumnName::new("name".into()), Cell::Text("bob".into()))],
            )
            .unwrap();
        assert_eq!(
            client.sql_kind(id, "INSERT INTO users (name) VALUES ('ada')"),
            Ok(SqlKind::Mutating)
        );
        assert_eq!(
            client.execute_mutating(id, "INSERT INTO users (name) VALUES ('ada')"),
            Err(ExecuteError::StagedChangesExist)
        );
        let still = client
            .query(id, "SELECT id, name FROM users ORDER BY id", Page::first())
            .unwrap();
        assert_eq!(still.rows(), [[Cell::Integer(1), Cell::Text("ken".into())]]);
        assert_eq!(client.staged_changes(id).unwrap().len(), 1);
    }

    #[test]
    fn mutating_sql_runs_as_its_own_transaction_when_bag_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("shop.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE);
                 INSERT INTO users (name) VALUES ('ken');",
            )
            .unwrap();
        drop(connection);
        let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
        let id = client.open_session(&connection_at(&db_path)).unwrap();
        client
            .execute_mutating(id, "INSERT INTO users (name) VALUES ('ada')")
            .unwrap();
        assert!(client.staged_changes(id).unwrap().is_empty());
        let after_insert = client
            .query(id, "SELECT id, name FROM users ORDER BY id", Page::first())
            .unwrap();
        assert_eq!(
            after_insert.rows(),
            [
                [Cell::Integer(1), Cell::Text("ken".into())],
                [Cell::Integer(2), Cell::Text("ada".into())]
            ]
        );
        assert!(
            client
                .execute_mutating(
                    id,
                    "INSERT INTO users (name) VALUES ('bob');\nINSERT INTO users (name) VALUES ('ken');"
                )
                .is_err()
        );
        let rolled_back = client
            .query(id, "SELECT id, name FROM users ORDER BY id", Page::first())
            .unwrap();
        assert_eq!(
            rolled_back.rows(),
            [
                [Cell::Integer(1), Cell::Text("ken".into())],
                [Cell::Integer(2), Cell::Text("ada".into())]
            ]
        );
        assert!(client.staged_changes(id).unwrap().is_empty());
    }
}
