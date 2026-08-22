use crate::engine::quote_ident;
use crate::table::{Namespace, Table, TableName};
use crate::table_page::ColumnName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDefinition {
    name: ColumnName,
    type_sql: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    CreateTable {
        namespace: Namespace,
        table: TableName,
        columns: Vec<ColumnDefinition>,
    },
    DropTable {
        table: Table,
    },
    AddColumn {
        table: Table,
        column: ColumnDefinition,
    },
    DropColumn {
        table: Table,
        column: ColumnName,
    },
    RenameColumn {
        table: Table,
        from: ColumnName,
        to: ColumnName,
    },
    AddIndex {
        name: String,
        table: Table,
        columns: Vec<ColumnName>,
        unique: bool,
    },
    DropIndex {
        name: String,
        table: Table,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaChangeError {
    #[error("unknown Session")]
    UnknownSession,
    #[error("cannot run a Schema Change while Staged Changes exist")]
    StagedChangesExist,
    #[error("{0}")]
    Database(String),
}

impl ColumnDefinition {
    pub fn new(name: impl Into<String>, type_sql: impl Into<String>) -> Self {
        Self {
            name: ColumnName::new(name.into()),
            type_sql: type_sql.into(),
        }
    }

    pub fn name(&self) -> &ColumnName {
        &self.name
    }

    pub fn type_sql(&self) -> &str {
        &self.type_sql
    }
}

pub(crate) fn schema_change_ddl_sqlite(change: &SchemaChange) -> String {
    schema_change_ddl_quoted(change, quote_ident)
}

pub(crate) fn schema_change_ddl_postgres(change: &SchemaChange) -> String {
    schema_change_ddl_quoted(change, quote_ident)
}

pub(crate) fn schema_change_ddl_mysql(change: &SchemaChange) -> String {
    match change {
        SchemaChange::DropIndex { name, table } => format!(
            "DROP INDEX {} ON {}",
            mysql_quote_ident(name),
            qualified_table(table, mysql_quote_ident)
        ),
        _ => schema_change_ddl_quoted(change, mysql_quote_ident),
    }
}

fn schema_change_ddl_quoted(
    change: &SchemaChange,
    quote: fn(&str) -> String,
) -> String {
    match change {
        SchemaChange::CreateTable {
            namespace,
            table,
            columns,
        } => {
            let table_sql = qualified_table_for_create(namespace, table, quote);
            let column_sql = columns
                .iter()
                .map(|column| {
                    format!(
                        "{} {}",
                        quote(column.name().as_str()),
                        column.type_sql()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("CREATE TABLE {table_sql} ({column_sql})")
        }
        SchemaChange::DropTable { table } => {
            format!("DROP TABLE {}", qualified_table(table, quote))
        }
        SchemaChange::AddColumn { table, column } => format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            qualified_table(table, quote),
            quote(column.name().as_str()),
            column.type_sql()
        ),
        SchemaChange::DropColumn { table, column } => format!(
            "ALTER TABLE {} DROP COLUMN {}",
            qualified_table(table, quote),
            quote(column.as_str())
        ),
        SchemaChange::RenameColumn { table, from, to } => format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            qualified_table(table, quote),
            quote(from.as_str()),
            quote(to.as_str())
        ),
        SchemaChange::AddIndex {
            name,
            table,
            columns,
            unique,
        } => {
            let unique = if *unique { "UNIQUE " } else { "" };
            let column_sql = columns
                .iter()
                .map(|column| quote(column.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "CREATE {unique}INDEX {} ON {} ({column_sql})",
                quote(name),
                qualified_table(table, quote)
            )
        }
        SchemaChange::DropIndex { name, table: _ } => {
            format!("DROP INDEX {}", quote(name))
        }
    }
}

fn mysql_quote_ident(name: &str) -> String {
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('`');
    for ch in name.chars() {
        if ch == '`' {
            quoted.push_str("``");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('`');
    quoted
}

fn qualified_table_for_create(
    namespace: &Namespace,
    table: &TableName,
    quote: fn(&str) -> String,
) -> String {
    if namespace.as_str() == "main" {
        quote(table.as_str())
    } else {
        format!(
            "{}.{}",
            quote(namespace.as_str()),
            quote(table.as_str())
        )
    }
}

fn qualified_table(table: &Table, quote: fn(&str) -> String) -> String {
    match table.namespace() {
        None => quote(table.name().as_str()),
        Some(namespace) => qualified_table_for_create(namespace, table.name(), quote),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orders() -> Table {
        Table::flat(TableName::new("orders".into()))
    }

    fn orders_name() -> TableName {
        TableName::new("orders".into())
    }

    #[test]
    fn create_table_ddl_uses_main_namespace_on_sqlite() {
        let ddl = schema_change_ddl_sqlite(&SchemaChange::CreateTable {
            namespace: Namespace::main(),
            table: orders_name(),
            columns: vec![
                ColumnDefinition::new("id", "INTEGER PRIMARY KEY"),
                ColumnDefinition::new("total", "INTEGER NOT NULL"),
            ],
        });
        assert_eq!(
            ddl,
            "CREATE TABLE \"orders\" (\"id\" INTEGER PRIMARY KEY, \"total\" INTEGER NOT NULL)"
        );
    }

    #[test]
    fn create_table_ddl_qualifies_non_main_namespace() {
        let ddl = schema_change_ddl_sqlite(&SchemaChange::CreateTable {
            namespace: Namespace::new("public"),
            table: orders_name(),
            columns: vec![ColumnDefinition::new("id", "INTEGER PRIMARY KEY")],
        });
        assert_eq!(
            ddl,
            "CREATE TABLE \"public\".\"orders\" (\"id\" INTEGER PRIMARY KEY)"
        );
    }

    #[test]
    fn drop_table_add_drop_rename_column_and_index_ddl() {
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::DropTable { table: orders() }),
            "DROP TABLE \"orders\""
        );
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::AddColumn {
                table: orders(),
                column: ColumnDefinition::new("status", "TEXT"),
            }),
            "ALTER TABLE \"orders\" ADD COLUMN \"status\" TEXT"
        );
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::DropColumn {
                table: orders(),
                column: ColumnName::new("status".into()),
            }),
            "ALTER TABLE \"orders\" DROP COLUMN \"status\""
        );
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::RenameColumn {
                table: orders(),
                from: ColumnName::new("status".into()),
                to: ColumnName::new("state".into()),
            }),
            "ALTER TABLE \"orders\" RENAME COLUMN \"status\" TO \"state\""
        );
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::AddIndex {
                name: "orders_status_idx".into(),
                table: orders(),
                columns: vec![ColumnName::new("status".into())],
                unique: false,
            }),
            "CREATE INDEX \"orders_status_idx\" ON \"orders\" (\"status\")"
        );
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::AddIndex {
                name: "orders_status_uidx".into(),
                table: orders(),
                columns: vec![
                    ColumnName::new("status".into()),
                    ColumnName::new("id".into()),
                ],
                unique: true,
            }),
            "CREATE UNIQUE INDEX \"orders_status_uidx\" ON \"orders\" (\"status\", \"id\")"
        );
        assert_eq!(
            schema_change_ddl_sqlite(&SchemaChange::DropIndex {
                name: "orders_status_idx".into(),
                table: orders(),
            }),
            "DROP INDEX \"orders_status_idx\""
        );
    }

    #[test]
    fn mysql_drop_index_includes_table() {
        assert_eq!(
            schema_change_ddl_mysql(&SchemaChange::DropIndex {
                name: "orders_status_idx".into(),
                table: orders(),
            }),
            "DROP INDEX `orders_status_idx` ON `orders`"
        );
    }
}
