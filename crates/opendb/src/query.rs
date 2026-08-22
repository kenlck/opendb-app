use sqlparser::ast::{
    Expr, GroupByExpr, ObjectName, Query, Select, SelectItem, SelectItemQualifiedWildcardKind,
    SetExpr, Statement, TableFactor, WildcardAdditionalOptions,
};
use sqlparser::dialect::{PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::table::Table;
use crate::table_page::{Cell, ColumnName, TablePage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlKind {
    Read,
    Mutating,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultStaging {
    ReadOnly,
    Staged { table: Table },
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult {
    page: TablePage,
    staging: ResultStaging,
}

impl QueryResult {
    pub(crate) fn new(page: TablePage, staging: ResultStaging) -> Self {
        Self { page, staging }
    }

    pub fn columns(&self) -> &[ColumnName] {
        self.page.columns()
    }

    pub fn rows(&self) -> &[Vec<Cell>] {
        self.page.rows()
    }

    pub fn has_next(&self) -> bool {
        self.page.has_next()
    }

    pub fn staging(&self) -> &ResultStaging {
        &self.staging
    }

    pub fn page(&self) -> &TablePage {
        &self.page
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExecuteError {
    #[error("unknown Session")]
    UnknownSession,
    #[error("cannot run mutating SQL while Staged Changes exist")]
    StagedChangesExist,
    #[error("SQL is mutating")]
    MutatingSql,
    #[error("SQL is a read")]
    ReadSql,
    #[error("{0}")]
    Database(String),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Projection {
    All,
    Columns(Vec<String>),
}

pub(crate) fn one_table_projection(sql: &str) -> Option<(String, Projection)> {
    projection_from_sql(&SQLiteDialect {}, sql)
}

pub(crate) fn sql_kind_postgres(sql: &str) -> Result<SqlKind, crate::engine::DatabaseError> {
    sql_kind_from_dialect(&PostgreSqlDialect {}, sql)
}

pub(crate) fn sql_kind_from_dialect(
    dialect: &dyn sqlparser::dialect::Dialect,
    sql: &str,
) -> Result<SqlKind, crate::engine::DatabaseError> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err(crate::engine::DatabaseError::from_engine("empty SQL"));
    }
    let statements = Parser::parse_sql(dialect, trimmed)
        .map_err(crate::engine::DatabaseError::from_engine)?;
    for statement in statements {
        if !matches!(statement, Statement::Query(_)) {
            return Ok(SqlKind::Mutating);
        }
    }
    Ok(SqlKind::Read)
}

fn projection_from_sql(dialect: &dyn sqlparser::dialect::Dialect, sql: &str) -> Option<(String, Projection)> {
    let statements = Parser::parse_sql(dialect, sql).ok()?;
    if statements.len() != 1 {
        return None;
    }
    let Statement::Query(query) = &statements[0] else {
        return None;
    };
    projection_from_query(query)
}

fn projection_from_query(query: &Query) -> Option<(String, Projection)> {
    if query.with.is_some() || !query.pipe_operators.is_empty() {
        return None;
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    projection_from_select(select)
}

fn projection_from_select(select: &Select) -> Option<(String, Projection)> {
    if select.from.len() != 1 {
        return None;
    }
    let from = &select.from[0];
    if !from.joins.is_empty() || !select.lateral_views.is_empty() || select.having.is_some() {
        return None;
    }
    match &select.group_by {
        GroupByExpr::All(_) => return None,
        GroupByExpr::Expressions(exprs, _) if !exprs.is_empty() => return None,
        GroupByExpr::Expressions(_, _) => {}
    }
    if select.into.is_some() {
        return None;
    }
    let TableFactor::Table {
        name, alias, args, ..
    } = &from.relation
    else {
        return None;
    };
    if args.is_some() {
        return None;
    }
    if alias
        .as_ref()
        .is_some_and(|alias| !alias.columns.is_empty())
    {
        return None;
    }
    let table = object_name_last(name)?.to_string();
    let alias = alias.as_ref().map(|alias| alias.name.value.as_str());
    let mut columns = Vec::new();
    let mut all = false;
    for item in &select.projection {
        match item {
            SelectItem::Wildcard(options) => {
                if !wildcard_is_plain(options) {
                    return None;
                }
                all = true;
            }
            SelectItem::QualifiedWildcard(kind, options) => {
                if !wildcard_is_plain(options) {
                    return None;
                }
                let SelectItemQualifiedWildcardKind::ObjectName(name) = kind else {
                    return None;
                };
                let qualifier = object_name_last(name)?;
                if !same_name(qualifier, &table)
                    && !alias.is_some_and(|alias| same_name(qualifier, alias))
                {
                    return None;
                }
                all = true;
            }
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                if is_aggregate_expr(expr) {
                    return None;
                }
                if let Some(column) = column_from_expr(expr, &table, alias) {
                    columns.push(column);
                }
            }
        }
    }
    if all {
        Some((table, Projection::All))
    } else {
        Some((table, Projection::Columns(columns)))
    }
}

fn column_from_expr(expr: &Expr, table: &str, alias: Option<&str>) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => Some(parts[0].value.clone()),
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            let column = parts.last()?.value.clone();
            let qualifier = &parts[parts.len() - 2].value;
            if same_name(qualifier, table) || alias.is_some_and(|alias| same_name(qualifier, alias))
            {
                Some(column)
            } else {
                None
            }
        }
        Expr::Nested(inner) => column_from_expr(inner, table, alias),
        _ => None,
    }
}

fn is_aggregate_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Function(function) => {
            let Some(name) = object_name_last(&function.name) else {
                return false;
            };
            function.over.is_none()
                && [
                    "avg",
                    "count",
                    "count_if",
                    "group_concat",
                    "max",
                    "min",
                    "sum",
                    "total",
                ]
                .iter()
                .any(|aggregate| aggregate.eq_ignore_ascii_case(name))
        }
        Expr::Nested(inner) => is_aggregate_expr(inner),
        _ => false,
    }
}

fn wildcard_is_plain(options: &WildcardAdditionalOptions) -> bool {
    options.opt_ilike.is_none()
        && options.opt_exclude.is_none()
        && options.opt_except.is_none()
        && options.opt_replace.is_none()
        && options.opt_rename.is_none()
}

fn object_name_last(name: &ObjectName) -> Option<&str> {
    name.0.last()?.as_ident().map(|ident| ident.value.as_str())
}

fn same_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

pub(crate) fn identity_present(identity: &[String], projection: &Projection) -> bool {
    match projection {
        Projection::All => true,
        Projection::Columns(columns) => identity
            .iter()
            .all(|column| columns.iter().any(|projected| same_name(projected, column))),
    }
}
