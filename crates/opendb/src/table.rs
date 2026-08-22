#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    name: TableName,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TableName(String);

pub struct TableCatalog {
    tables: Vec<Table>,
}

impl Table {
    pub(crate) fn new(name: TableName) -> Self {
        Self { name }
    }

    pub fn name(&self) -> &TableName {
        &self.name
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
    pub(crate) fn new(tables: Vec<Table>) -> Self {
        Self { tables }
    }

    pub fn tables(&self) -> &[Table] {
        &self.tables
    }
}
