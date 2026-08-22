pub(crate) fn quote_ident(name: &str) -> String {
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('"');
    for ch in name.chars() {
        if ch == '"' {
            quoted.push_str("\"\"");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('"');
    quoted
}

pub(crate) fn qualified_table(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        None => quote_ident(name),
        Some(ns) => format!("{}.{}", quote_ident(ns), quote_ident(name)),
    }
}

pub(crate) fn trim_sql(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim()
}

pub(crate) fn like_contains(value: &str) -> String {
    let mut escaped = String::from("%");
    for ch in value.chars() {
        match ch {
            '%' | '_' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            other => escaped.push(other),
        }
    }
    escaped.push('%');
    escaped
}
