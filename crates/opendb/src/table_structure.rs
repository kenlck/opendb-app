use crate::table_page::ColumnName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableStructure {
    columns: Vec<StructureColumn>,
    indexes: Vec<StructureIndex>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureColumn {
    name: ColumnName,
    type_name: String,
    not_null: bool,
    primary_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureIndex {
    name: String,
    unique: bool,
    columns: Vec<String>,
}

impl TableStructure {
    pub(crate) fn new(columns: Vec<StructureColumn>, indexes: Vec<StructureIndex>) -> Self {
        Self { columns, indexes }
    }

    pub fn columns(&self) -> &[StructureColumn] {
        &self.columns
    }

    pub fn indexes(&self) -> &[StructureIndex] {
        &self.indexes
    }
}

impl StructureColumn {
    pub(crate) fn new(
        name: ColumnName,
        type_name: String,
        not_null: bool,
        primary_key: bool,
    ) -> Self {
        Self {
            name,
            type_name,
            not_null,
            primary_key,
        }
    }

    pub fn name(&self) -> &ColumnName {
        &self.name
    }

    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    pub fn not_null(&self) -> bool {
        self.not_null
    }

    pub fn primary_key(&self) -> bool {
        self.primary_key
    }
}

impl StructureIndex {
    pub(crate) fn new(name: String, unique: bool, columns: Vec<String>) -> Self {
        Self {
            name,
            unique,
            columns,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn unique(&self) -> bool {
        self.unique
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}
