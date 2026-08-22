#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColumnName(String);

impl ColumnName {
    pub(crate) fn new(name: String) -> Self {
        Self(name)
    }

    pub fn from_name(name: impl Into<String>) -> Self {
        Self::new(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Equals { column: ColumnName, value: Cell },
    Contains { column: ColumnName, value: String },
    IsNull { column: ColumnName },
}

impl Filter {
    pub fn equals(column: impl Into<String>, value: Cell) -> Self {
        Self::Equals {
            column: ColumnName::new(column.into()),
            value,
        }
    }

    pub fn contains(column: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Contains {
            column: ColumnName::new(column.into()),
            value: value.into(),
        }
    }

    pub fn is_null(column: impl Into<String>) -> Self {
        Self::IsNull {
            column: ColumnName::new(column.into()),
        }
    }
}

pub const TABLE_PAGE_SIZE: usize = 100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Page(usize);

impl Page {
    pub const fn first() -> Self {
        Self(0)
    }

    pub const fn from_index(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    pub const fn prev(self) -> Option<Self> {
        if self.0 == 0 {
            None
        } else {
            Some(Self(self.0 - 1))
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TablePage {
    columns: Vec<ColumnName>,
    rows: Vec<Vec<Cell>>,
    has_next: bool,
}

impl TablePage {
    pub(crate) fn from_fetched(columns: Vec<ColumnName>, mut rows: Vec<Vec<Cell>>) -> Self {
        let has_next = rows.len() > TABLE_PAGE_SIZE;
        if has_next {
            rows.truncate(TABLE_PAGE_SIZE);
        }
        Self {
            columns,
            rows,
            has_next,
        }
    }

    pub fn columns(&self) -> &[ColumnName] {
        &self.columns
    }

    pub fn rows(&self) -> &[Vec<Cell>] {
        &self.rows
    }

    pub fn has_next(&self) -> bool {
        self.has_next
    }
}
