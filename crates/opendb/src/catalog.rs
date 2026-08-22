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
    let namespace = namespace.to_ascii_lowercase();
    matches!(
        namespace.as_str(),
        "pg_catalog" | "information_schema" | "pg_toast"
    ) || namespace.starts_with("pg_temp")
        || namespace.starts_with("pg_toast_temp")
        || namespace.starts_with("_timescaledb_")
}

/// Prefer `public` when present; otherwise the first Namespace in catalog order.
pub fn default_namespace<'a>(
    namespaces: impl IntoIterator<Item = &'a Namespace>,
) -> Option<Namespace> {
    let mut first = None;
    for namespace in namespaces {
        if namespace.as_str() == "public" {
            return Some(namespace.clone());
        }
        if first.is_none() {
            first = Some(namespace.clone());
        }
    }
    first
}

/// Tables in one Namespace, optionally filtered by a case-insensitive name query.
pub fn tables_in_namespace<'a>(
    catalog: &'a TableCatalog,
    namespace: &Namespace,
    query: &str,
) -> Vec<&'a Table> {
    let query = query.trim().to_ascii_lowercase();
    let Some(groups) = catalog.namespace_groups() else {
        return Vec::new();
    };
    let Some(group) = groups.iter().find(|group| group.namespace() == namespace) else {
        return Vec::new();
    };
    group
        .tables()
        .iter()
        .filter(|table| {
            if query.is_empty() {
                return true;
            }
            table.name().as_str().to_ascii_lowercase().contains(&query)
        })
        .collect()
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
            classify_grouped(namespaced("_timescaledb_internal", "chunk")),
            classify_grouped(namespaced("_timescaledb_catalog", "hypertable")),
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
        assert!(
            catalog.tables().iter().all(|table| {
                !table
                    .namespace()
                    .unwrap()
                    .as_str()
                    .starts_with("_timescaledb_")
            })
        );
    }

    #[test]
    fn default_namespace_prefers_public_then_first_user_schema() {
        let public = Namespace::new("public");
        let auth = Namespace::new("auth");
        assert_eq!(
            default_namespace([&auth, &public]).as_ref().map(Namespace::as_str),
            Some("public")
        );
        assert_eq!(
            default_namespace([&auth]).as_ref().map(Namespace::as_str),
            Some("auth")
        );
        assert_eq!(default_namespace(None::<&Namespace>), None);
    }

    #[test]
    fn tables_in_namespace_filters_search_to_current_schema_only() {
        let classified = [
            classify_grouped(namespaced("public", "users")),
            classify_grouped(namespaced("public", "orders")),
            classify_grouped(namespaced("auth", "users")),
        ];
        let catalog = assemble_grouped(classified, SystemCatalogPreference::Hidden);
        let public = Namespace::new("public");
        let names: Vec<_> = tables_in_namespace(&catalog, &public, "use")
            .iter()
            .map(|table| table.name().as_str())
            .collect();
        assert_eq!(names, ["users"]);
        let auth = Namespace::new("auth");
        let auth_names: Vec<_> = tables_in_namespace(&catalog, &auth, "")
            .iter()
            .map(|table| table.name().as_str())
            .collect();
        assert_eq!(auth_names, ["users"]);
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
