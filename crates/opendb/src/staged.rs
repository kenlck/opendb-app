use crate::table::Table;
use crate::table_page::{Cell, ColumnName};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StagedChangeId(u64);

impl StagedChangeId {
    pub(crate) fn new(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowIdentity {
    columns: Vec<(ColumnName, Cell)>,
}

impl RowIdentity {
    pub(crate) fn new(columns: Vec<(ColumnName, Cell)>) -> Self {
        Self { columns }
    }

    pub fn columns(&self) -> &[(ColumnName, Cell)] {
        &self.columns
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StagedChange {
    Insert {
        id: StagedChangeId,
        table: Table,
        values: Vec<(ColumnName, Cell)>,
    },
    Update {
        id: StagedChangeId,
        table: Table,
        identity: RowIdentity,
        last_seen: Vec<(ColumnName, Cell)>,
        new_values: Vec<(ColumnName, Cell)>,
    },
    Delete {
        id: StagedChangeId,
        table: Table,
        identity: RowIdentity,
        last_seen: Vec<(ColumnName, Cell)>,
    },
}

impl StagedChange {
    pub fn id(&self) -> StagedChangeId {
        match self {
            Self::Insert { id, .. } | Self::Update { id, .. } | Self::Delete { id, .. } => *id,
        }
    }

    pub fn table(&self) -> &Table {
        match self {
            Self::Insert { table, .. }
            | Self::Update { table, .. }
            | Self::Delete { table, .. } => table,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StageError {
    #[error("unknown Session")]
    UnknownSession,
    #[error("update and delete require Row Identity")]
    MissingRowIdentity,
    #[error("{0}")]
    Database(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApplyError {
    #[error("unknown Session")]
    UnknownSession,
    #[error("a row changed since it was read")]
    Conflict,
    #[error("{0}")]
    Database(String),
}
