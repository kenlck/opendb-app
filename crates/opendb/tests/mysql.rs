use opendb::{
    Cell, Client, ColumnDefinition, ColumnName, Connection, ConnectionString, Filter, Namespace,
    Page, ResultStaging, SchemaChange, SessionId, SqlKind, Table, TableName,
};

fn mysql_url() -> Option<String> {
    std::env::var("OPENDB_TEST_MYSQL")
        .ok()
        .filter(|value| !value.is_empty())
}

fn open_mysql() -> Option<(tempfile::TempDir, Client, SessionId)> {
    let url = mysql_url()?;
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
    let connection = Connection::from_string(ConnectionString::parse(&url).unwrap());
    let id = client
        .open_session(&connection)
        .expect("mysql Session opens");
    Some((dir, client, id))
}

fn named_table(client: &Client, id: SessionId, name: &str) -> Table {
    client
        .tables(id)
        .unwrap()
        .tables()
        .iter()
        .find(|table| table.name().as_str() == name)
        .cloned()
        .unwrap_or_else(|| panic!("missing Table {name}"))
}

fn seed(client: &Client, id: SessionId) {
    client
        .execute_mutating(id, "DROP TABLE IF EXISTS opendb_users")
        .unwrap();
    client
        .execute_mutating(id, "DROP TABLE IF EXISTS opendb_orders")
        .unwrap();
    client
        .execute_mutating(
            id,
            "CREATE TABLE opendb_users (
                 id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                 name TEXT
             )",
        )
        .unwrap();
    client
        .execute_mutating(
            id,
            "INSERT INTO opendb_users (name) VALUES ('ken'), ('ada')",
        )
        .unwrap();
}

#[test]
fn mysql_connection_string_opens_a_session() {
    let Some((_, client, id)) = open_mysql() else {
        return;
    };
    assert!(client.session_name(id).is_ok());
}

#[test]
fn mysql_catalog_is_flat() {
    let Some((_, client, id)) = open_mysql() else {
        return;
    };
    seed(&client, id);
    let catalog = client.tables(id).unwrap();
    assert!(!catalog.is_grouped());
    assert!(
        catalog
            .tables()
            .iter()
            .any(|table| table.name().as_str() == "opendb_users")
    );
    assert!(
        catalog
            .tables()
            .iter()
            .all(|table| table.namespace().is_none())
    );
}

#[test]
fn mysql_page_filter_stage_apply_query_and_schema_change() {
    let Some((_, mut client, id)) = open_mysql() else {
        return;
    };
    seed(&client, id);
    let users = named_table(&client, id, "opendb_users");
    let page = client.table_page(id, &users, &[], Page::first()).unwrap();
    assert_eq!(
        page.rows(),
        [
            [Cell::Integer(1), Cell::Text("ken".into())],
            [Cell::Integer(2), Cell::Text("ada".into())]
        ]
    );
    let filtered = client
        .table_page(
            id,
            &users,
            &[Filter::contains("name", "ad")],
            Page::first(),
        )
        .unwrap();
    assert_eq!(
        filtered.rows(),
        [[Cell::Integer(2), Cell::Text("ada".into())]]
    );
    let last_seen: Vec<_> = page
        .columns()
        .iter()
        .cloned()
        .zip(page.rows()[0].iter().cloned())
        .collect();
    client
        .stage_update(
            id,
            users.clone(),
            last_seen,
            vec![(ColumnName::from_name("name"), Cell::Text("zoe".into()))],
        )
        .unwrap();
    client.apply(id).unwrap();
    let applied = client.table_page(id, &users, &[], Page::first()).unwrap();
    assert_eq!(
        applied.rows(),
        [
            [Cell::Integer(1), Cell::Text("zoe".into())],
            [Cell::Integer(2), Cell::Text("ada".into())]
        ]
    );
    assert_eq!(
        client.sql_kind(id, "SELECT name FROM opendb_users ORDER BY id"),
        Ok(SqlKind::Read)
    );
    let result = client
        .query(
            id,
            "SELECT id, name FROM opendb_users ORDER BY id",
            Page::first(),
        )
        .unwrap();
    assert_eq!(
        result.rows()[0],
        [Cell::Integer(1), Cell::Text("zoe".into())]
    );
    let ResultStaging::Staged { table } = result.staging() else {
        panic!("expected staged Result");
    };
    assert_eq!(table.name().as_str(), "opendb_users");
    client
        .execute_schema_change(
            id,
            &SchemaChange::AddColumn {
                table: users.clone(),
                column: ColumnDefinition::new("email", "TEXT"),
            },
        )
        .unwrap();
    let structure = client.table_structure(id, &users).unwrap();
    assert!(
        structure
            .columns()
            .iter()
            .any(|column| column.name().as_str() == "email")
    );
    client
        .execute_schema_change(
            id,
            &SchemaChange::CreateTable {
                namespace: Namespace::main(),
                table: TableName::new("opendb_orders".into()),
                columns: vec![
                    ColumnDefinition::new("id", "INT NOT NULL AUTO_INCREMENT PRIMARY KEY"),
                    ColumnDefinition::new("total", "INT NOT NULL"),
                ],
            },
        )
        .unwrap();
    let catalog = client.tables(id).unwrap();
    assert!(
        catalog
            .tables()
            .iter()
            .any(|table| table.name().as_str() == "opendb_orders")
    );
}
