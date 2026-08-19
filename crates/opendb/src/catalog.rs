use serde::{Deserialize, Serialize};

use crate::table::{Table, TableCatalog, TableName};

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
pub(crate) enum ClassifiedName {
    User(TableName),
    SystemCatalog(TableName),
}

pub(crate) fn is_system_catalog(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("sqlite_")
}

pub(crate) fn classify(name: TableName) -> ClassifiedName {
    if is_system_catalog(name.as_str()) {
        ClassifiedName::SystemCatalog(name)
    } else {
        ClassifiedName::User(name)
    }
}

pub(crate) fn assemble(
    classified: impl IntoIterator<Item = ClassifiedName>,
    preference: SystemCatalogPreference,
) -> TableCatalog {
    let tables = classified
        .into_iter()
        .filter_map(|entry| match (preference, entry) {
            (SystemCatalogPreference::Hidden, ClassifiedName::SystemCatalog(_)) => None,
            (_, ClassifiedName::User(name) | ClassifiedName::SystemCatalog(name)) => {
                Some(Table::new(name))
            }
        })
        .collect();
    TableCatalog::new(tables)
}
