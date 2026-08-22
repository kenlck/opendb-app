#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    namespace: Option<Namespace>,
    name: TableName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceGroup {
    namespace: Namespace,
    tables: Vec<Table>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TableName(String);

pub struct TableCatalog {
    groups: Option<Vec<NamespaceGroup>>,
    tables: Vec<Table>,
}

impl Namespace {
    pub fn main() -> Self {
        Self("main".into())
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Table {
    pub(crate) fn flat(name: TableName) -> Self {
        Self {
            namespace: None,
            name,
        }
    }

    pub(crate) fn in_namespace(namespace: Namespace, name: TableName) -> Self {
        Self {
            namespace: Some(namespace),
            name,
        }
    }

    pub fn namespace(&self) -> Option<&Namespace> {
        self.namespace.as_ref()
    }

    pub fn name(&self) -> &TableName {
        &self.name
    }
}

impl NamespaceGroup {
    pub(crate) fn new(namespace: Namespace, tables: Vec<Table>) -> Self {
        Self { namespace, tables }
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn tables(&self) -> &[Table] {
        &self.tables
    }
}

impl TableName {
    pub fn new(name: String) -> Self {
        Self(name)
    }

    pub fn from_name(name: impl Into<String>) -> Self {
        Self::new(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TableCatalog {
    pub(crate) fn flat(tables: Vec<Table>) -> Self {
        Self {
            groups: None,
            tables,
        }
    }

    pub(crate) fn grouped(groups: Vec<NamespaceGroup>) -> Self {
        let tables = groups
            .iter()
            .flat_map(|group| group.tables.iter().cloned())
            .collect();
        Self { groups: Some(groups), tables }
    }

    pub fn is_grouped(&self) -> bool {
        self.groups.is_some()
    }

    pub fn namespace_groups(&self) -> Option<&[NamespaceGroup]> {
        self.groups.as_deref()
    }

    pub fn tables(&self) -> &[Table] {
        &self.tables
    }
}
