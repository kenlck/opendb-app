use serde::{Deserialize, Serialize};

use crate::table::{Namespace, NamespaceGroup, Table, TableCatalog};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemCatalogPreference {
    #[default]
    Hidden,
    Shown,
}

impl SystemCatalogPreference {
    pub fn toggled(self) -> Self {
        match self {
            Self::Hidden => Self::Shown,
            Self::Shown => Self::Hidden,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClassifiedTable {
    User(Table),
    SystemCatalog(Table),
}

pub(crate) fn is_sqlite_system_catalog(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("sqlite_")
}

pub(crate) fn is_postgres_system_namespace(namespace: &str) -> bool {
    matches!(
        namespace.to_ascii_lowercase().as_str(),
        "pg_catalog" | "information_schema" | "pg_toast"
    )
}

pub(crate) fn classify_flat(table: Table) -> ClassifiedTable {
    if is_sqlite_system_catalog(table.name().as_str()) {
        ClassifiedTable::SystemCatalog(table)
    } else {
        ClassifiedTable::User(table)
    }
}

pub(crate) fn classify_grouped(table: Table) -> ClassifiedTable {
    if is_postgres_system_namespace(table.namespace().unwrap().as_str()) {
        ClassifiedTable::SystemCatalog(table)
    } else {
        ClassifiedTable::User(table)
    }
}

pub(crate) fn assemble_flat(
    classified: impl IntoIterator<Item = ClassifiedTable>,
    preference: SystemCatalogPreference,
) -> TableCatalog {
    let tables = classified
        .into_iter()
        .filter_map(|entry| match (preference, entry) {
            (SystemCatalogPreference::Hidden, ClassifiedTable::SystemCatalog(_)) => None,
            (_, ClassifiedTable::User(table) | ClassifiedTable::SystemCatalog(table)) => {
                Some(table)
            }
        })
        .collect();
    TableCatalog::flat(tables)
}

pub(crate) fn assemble_grouped(
    classified: impl IntoIterator<Item = ClassifiedTable>,
    preference: SystemCatalogPreference,
) -> TableCatalog {
    let mut groups: Vec<(Namespace, Vec<Table>)> = Vec::new();
    for entry in classified {
        match (preference, entry) {
            (SystemCatalogPreference::Hidden, ClassifiedTable::SystemCatalog(_)) => continue,
            (_, ClassifiedTable::User(table) | ClassifiedTable::SystemCatalog(table)) => {
                let namespace = table
                    .namespace()
                    .cloned()
                    .expect("grouped catalog tables carry a Namespace");
                if let Some(group) = groups.iter_mut().find(|(ns, _)| ns == &namespace) {
                    group.1.push(table);
                } else {
                    groups.push((namespace, vec![table]));
                }
            }
        }
    }
    let groups = groups
        .into_iter()
        .map(|(namespace, tables)| NamespaceGroup::new(namespace, tables))
        .collect();
    TableCatalog::grouped(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::TableName;

    fn namespaced(namespace: &str, name: &str) -> Table {
        Table::in_namespace(Namespace::new(namespace), TableName::new(name.into()))
    }

    #[test]
    fn grouped_catalog_hides_system_namespaces_and_keeps_user_groups() {
        let classified = [
            classify_grouped(namespaced("public", "users")),
            classify_grouped(namespaced("auth", "users")),
            classify_grouped(namespaced("pg_catalog", "pg_class")),
            classify_grouped(namespaced("information_schema", "tables")),
        ];
        let catalog = assemble_grouped(classified, SystemCatalogPreference::Hidden);
        assert!(catalog.is_grouped());
        let groups = catalog.namespace_groups().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].namespace().as_str(), "public");
        assert_eq!(groups[0].tables()[0].name().as_str(), "users");
        assert_eq!(groups[1].namespace().as_str(), "auth");
        assert_eq!(groups[1].tables()[0].name().as_str(), "users");
        assert!(
            catalog
                .tables()
                .iter()
                .all(|table| table.namespace().unwrap().as_str() != "pg_catalog")
        );
    }

    #[test]
    fn grouped_catalog_toggle_shows_system_namespaces() {
        let classified = [
            classify_grouped(namespaced("public", "users")),
            classify_grouped(namespaced("pg_catalog", "pg_class")),
        ];
        let catalog = assemble_grouped(classified, SystemCatalogPreference::Shown);
        let names: Vec<_> = catalog
            .namespace_groups()
            .unwrap()
            .iter()
            .map(|group| group.namespace().as_str())
            .collect();
        assert_eq!(names, ["public", "pg_catalog"]);
    }
}
