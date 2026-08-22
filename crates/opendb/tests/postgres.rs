use opendb::{
    Cell, Client, ColumnDefinition, ColumnName, Connection, ConnectionString, Filter, Namespace,
    Page, ResultStaging, SchemaChange, SessionId, SqlKind, SystemCatalogPreference, Table,
    TableName,
};

fn postgres_url() -> Option<String> {
    std::env::var("OPENDB_TEST_POSTGRES")
        .ok()
        .filter(|value| !value.is_empty())
}

fn open_postgres() -> Option<(tempfile::TempDir, Client, SessionId)> {
    let url = postgres_url()?;
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path().join("preferences.json")).unwrap();
    let connection = Connection::from_string(ConnectionString::parse(&url).unwrap());
    let id = client
        .open_session(&connection)
        .expect("postgres Session opens");
    Some((dir, client, id))
}

fn named_table(client: &Client, id: SessionId, namespace: &str, name: &str) -> Table {
    client
        .tables(id)
        .unwrap()
        .namespace_groups()
        .expect("grouped catalog")
        .iter()
        .find(|group| group.namespace().as_str() == namespace)
        .unwrap_or_else(|| panic!("missing Namespace {namespace}"))
        .tables()
        .iter()
        .find(|table| table.name().as_str() == name)
        .cloned()
        .unwrap_or_else(|| panic!("missing Table {namespace}.{name}"))
}

fn seed(client: &Client, id: SessionId) {
    client
        .execute_mutating(
            id,
            "CREATE SCHEMA IF NOT EXISTS auth;
             DROP TABLE IF EXISTS public.opendb_users;
             DROP TABLE IF EXISTS auth.opendb_users;
             DROP TABLE IF EXISTS auth.opendb_orders;
             CREATE TABLE public.opendb_users (
                 id SERIAL PRIMARY KEY,
                 name TEXT
             );
             INSERT INTO public.opendb_users (name) VALUES ('ken'), ('ada');
             CREATE TABLE auth.opendb_users (
                 id SERIAL PRIMARY KEY,
                 name TEXT
             );
             INSERT INTO auth.opendb_users (name) VALUES ('root');",
        )
        .unwrap();
}

#[test]
fn postgres_connection_string_opens_a_session() {
    let Some((_, client, id)) = open_postgres() else {
        return;
    };
    assert!(client.session_name(id).is_ok());
}

#[test]
fn postgres_tables_are_grouped_by_namespace_and_hide_system_catalogs() {
    let Some((_, client, id)) = open_postgres() else {
        return;
    };
    seed(&client, id);
    let catalog = client.tables(id).unwrap();
    assert!(catalog.is_grouped());
    let groups = catalog.namespace_groups().unwrap();
    let public = groups
        .iter()
        .find(|group| group.namespace().as_str() == "public")
        .expect("public Namespace");
    assert!(
        public
            .tables()
            .iter()
            .any(|table| table.name().as_str() == "opendb_users")
    );
    let auth = groups
        .iter()
        .find(|group| group.namespace().as_str() == "auth")
        .expect("auth Namespace");
    assert!(
        auth.tables()
            .iter()
            .any(|table| table.name().as_str() == "opendb_users")
    );
    assert!(groups.iter().all(|group| {
        !matches!(
            group.namespace().as_str(),
            "pg_catalog" | "information_schema" | "pg_toast"
        )
    }));
}

#[test]
fn postgres_system_catalog_toggle_reveals_pg_catalog() {
    let Some((_, mut client, id)) = open_postgres() else {
        return;
    };
    seed(&client, id);
    client
        .set_show_system_catalogs(SystemCatalogPreference::Shown)
        .unwrap();
    let catalog = client.tables(id).unwrap();
    assert!(
        catalog
            .namespace_groups()
            .unwrap()
            .iter()
            .any(|group| group.namespace().as_str() == "pg_catalog")
    );
}

#[test]
fn postgres_page_filter_stage_apply_query_and_schema_change() {
    let Some((_, mut client, id)) = open_postgres() else {
        return;
    };
    seed(&client, id);
    let users = named_table(&client, id, "public", "opendb_users");
    let page = client
        .table_page(id, &users, &[], Page::first())
        .unwrap();
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
        client.sql_kind(id, "SELECT name FROM public.opendb_users ORDER BY id"),
        Ok(SqlKind::Read)
    );
    let result = client
        .query(
            id,
            "SELECT id, name FROM public.opendb_users ORDER BY id",
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
    assert_eq!(table.namespace().unwrap().as_str(), "public");
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
}

#[test]
fn postgres_create_table_lands_in_browsed_namespace() {
    let Some((_, client, id)) = open_postgres() else {
        return;
    };
    seed(&client, id);
    client
        .execute_schema_change(
            id,
            &SchemaChange::CreateTable {
                namespace: Namespace::new("auth"),
                table: TableName::new("opendb_orders".into()),
                columns: vec![
                    ColumnDefinition::new("id", "SERIAL PRIMARY KEY"),
                    ColumnDefinition::new("total", "INTEGER NOT NULL"),
                ],
            },
        )
        .unwrap();
    let catalog = client.tables(id).unwrap();
    let auth = catalog
        .namespace_groups()
        .unwrap()
        .iter()
        .find(|group| group.namespace().as_str() == "auth")
        .unwrap();
    assert!(
        auth.tables()
            .iter()
            .any(|table| table.name().as_str() == "opendb_orders")
    );
    let public = catalog
        .namespace_groups()
        .unwrap()
        .iter()
        .find(|group| group.namespace().as_str() == "public")
        .unwrap();
    assert!(
        public
            .tables()
            .iter()
            .all(|table| table.name().as_str() != "opendb_orders")
    );
    let auth_users = named_table(&client, id, "auth", "opendb_users");
    let page = client
        .table_page(id, &auth_users, &[], Page::first())
        .unwrap();
    assert_eq!(page.rows(), [[Cell::Integer(1), Cell::Text("root".into())]]);
}
