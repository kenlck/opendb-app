mod catalog;
mod chrome_layout;
mod client;
mod connection;
mod connection_string;
mod engine;
mod list;
mod name;
mod query;
mod schema_change;
mod staged;
mod store;
mod table;
mod table_page;
mod table_structure;

pub use catalog::SystemCatalogPreference;
// Namespace picker helpers for Session chrome (TablePlus-style schema control).
pub use catalog::{default_namespace, tables_in_namespace};
pub use chrome_layout::{
    GRID_ROW_HEIGHT_PX, GRID_STRIPE, TAB_BAR_HEIGHT_PX, column_width_px, filter_chips_own_row,
    grid_emits_phantom_rows, namespace_footer_is_single_control, session_window_title,
    staged_inspector_shows_actions, staged_inspector_width_px,
};
pub use client::{CatalogError, Client, CloseResult, OpenError, PreferenceError, SessionId};
pub use connection::Connection;
pub use connection_string::{ConnectionString, Engine, ParseError};
pub use list::{AddResult, BundleError, ConnectionList};
pub use name::Name;
pub use query::{ExecuteError, QueryResult, ResultStaging, SqlKind};
pub use schema_change::{ColumnDefinition, SchemaChange, SchemaChangeError};
pub use staged::{ApplyError, RowIdentity, StageError, StagedChange, StagedChangeId};
pub use store::{FileStore, StoreError};
pub use table::{Namespace, NamespaceGroup, Table, TableCatalog, TableName};
pub use table_page::{Cell, ColumnName, Filter, Page, TABLE_PAGE_SIZE, TablePage};
pub use table_structure::{StructureColumn, StructureIndex, TableStructure};
