use std::fmt;

use url::Url;

use crate::name::Name;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    Postgres,
    Sqlite,
    Mysql,
}

impl Engine {
    /// Short badge for the Connection List (PG / SQL / MY).
    pub fn badge(self) -> &'static str {
        match self {
            Self::Postgres => "PG",
            Self::Sqlite => "SQL",
            Self::Mysql => "MY",
        }
    }

    /// Engine name for Session titlebars (`Postgres`, `SQLite`, `MySQL`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "Postgres",
            Self::Sqlite => "SQLite",
            Self::Mysql => "MySQL",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("connection string is empty")]
    Empty,
    #[error("unsupported URL scheme")]
    UnsupportedScheme,
    #[error("missing database name")]
    MissingDatabase,
    #[error("missing name")]
    MissingName,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionString {
    raw: String,
    engine: Engine,
    default_name: Name,
}

impl ConnectionString {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let raw = input.trim().to_owned();
        if raw.is_empty() {
            return Err(ParseError::Empty);
        }

        if is_windows_path(&raw) {
            let default_name = name_from_file_path(&raw)?;
            return Ok(Self {
                raw,
                engine: Engine::Sqlite,
                default_name,
            });
        }

        if raw.contains("://") {
            return parse_url(&raw);
        }

        let default_name = name_from_file_path(&raw)?;
        Ok(Self {
            raw,
            engine: Engine::Sqlite,
            default_name,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn engine(&self) -> Engine {
        self.engine
    }

    pub fn default_name(&self) -> &Name {
        &self.default_name
    }

    /// Host/file · Database label for the Connection List. Never includes the secret.
    pub fn list_subtitle(&self) -> String {
        match self.engine {
            Engine::Sqlite => sqlite_path_label(&self.raw),
            Engine::Postgres | Engine::Mysql => {
                let host = Url::parse(&self.raw)
                    .ok()
                    .and_then(|url| {
                        url.host_str()
                            .map(str::to_string)
                            .or_else(|| url.host().map(|host| host.to_string()))
                    })
                    .unwrap_or_else(|| "localhost".into());
                format!("{host} · {}", self.default_name.as_str())
            }
        }
    }
}

fn sqlite_path_label(raw: &str) -> String {
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "sqlite" {
            let path = url.path();
            if !path.is_empty() && path != "/" {
                return path.to_string();
            }
        }
    }
    raw.to_string()
}

impl fmt::Display for ConnectionString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_password(&self.raw))
    }
}

impl fmt::Debug for ConnectionString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConnectionString({})", redact_password(&self.raw))
    }
}

fn is_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn parse_url(raw: &str) -> Result<ConnectionString, ParseError> {
    let url = Url::parse(raw).map_err(|_| ParseError::UnsupportedScheme)?;
    let scheme = url.scheme();

    match scheme {
        "postgres" | "postgresql" => {
            let default_name = database_from_url(&url)?;
            Ok(ConnectionString {
                raw: raw.to_owned(),
                engine: Engine::Postgres,
                default_name,
            })
        }
        "mysql" | "mariadb" => {
            let default_name = database_from_url(&url)?;
            Ok(ConnectionString {
                raw: raw.to_owned(),
                engine: Engine::Mysql,
                default_name,
            })
        }
        "sqlite" => {
            let path = url.path();
            let default_name = name_from_file_path(path)?;
            Ok(ConnectionString {
                raw: raw.to_owned(),
                engine: Engine::Sqlite,
                default_name,
            })
        }
        _ => Err(ParseError::UnsupportedScheme),
    }
}

fn database_from_url(url: &Url) -> Result<Name, ParseError> {
    let segment = url
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    if segment.is_empty() {
        return Err(ParseError::MissingDatabase);
    }
    Name::new(segment).ok_or(ParseError::MissingDatabase)
}

fn name_from_file_path(path: &str) -> Result<Name, ParseError> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let stem = std::path::Path::new(trimmed)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(ParseError::MissingName)?;
    Name::new(stem).ok_or(ParseError::MissingName)
}

fn redact_password(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_owned();
    };
    if url.password().is_some() {
        let _ = url.set_password(Some("****"));
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_defaults_name_from_database() {
        let cs = ConnectionString::parse("postgres://ken:pw@localhost/felicity_os").unwrap();
        assert_eq!(cs.engine(), Engine::Postgres);
        assert_eq!(cs.default_name().as_str(), "felicity_os");
        assert_eq!(cs.as_str(), "postgres://ken:pw@localhost/felicity_os");
    }

    #[test]
    fn postgres_missing_database_errors() {
        assert_eq!(
            ConnectionString::parse("postgres://ken:pw@localhost"),
            Err(ParseError::MissingDatabase)
        );
        assert_eq!(
            ConnectionString::parse("postgres://ken:pw@localhost/"),
            Err(ParseError::MissingDatabase)
        );
    }

    #[test]
    fn mysql_missing_database_errors() {
        assert_eq!(
            ConnectionString::parse("mysql://ken:pw@localhost"),
            Err(ParseError::MissingDatabase)
        );
    }

    #[test]
    fn sqlite_file_path() {
        let cs = ConnectionString::parse("/tmp/shop.db").unwrap();
        assert_eq!(cs.engine(), Engine::Sqlite);
        assert_eq!(cs.default_name().as_str(), "shop");
        assert_eq!(cs.list_subtitle(), "/tmp/shop.db");
        assert_eq!(Engine::Sqlite.badge(), "SQL");
        assert_eq!(Engine::Sqlite.label(), "SQLite");
    }

    #[test]
    fn list_subtitle_is_host_and_database_without_secret() {
        let cs = ConnectionString::parse("postgres://ken:secret@db.internal/myapp").unwrap();
        assert_eq!(cs.list_subtitle(), "db.internal · myapp");
        assert!(!cs.list_subtitle().contains("secret"));
        assert_eq!(Engine::Postgres.badge(), "PG");
        assert_eq!(Engine::Mysql.badge(), "MY");
    }

    #[test]
    fn display_and_debug_redact_password() {
        let cs = ConnectionString::parse("postgres://ken:secret@localhost/felicity_os").unwrap();
        let display = cs.to_string();
        let debug = format!("{cs:?}");
        assert!(!display.contains("secret"));
        assert!(!debug.contains("secret"));
        assert!(display.contains("****"));
        assert!(debug.contains("****"));
        assert_eq!(cs.as_str(), "postgres://ken:secret@localhost/felicity_os");
    }
}
