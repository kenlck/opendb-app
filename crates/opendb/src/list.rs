use serde::{Deserialize, Serialize};

use crate::connection::Connection;
use crate::connection_string::ConnectionString;
use crate::name::Name;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionList {
    items: Vec<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddResult {
    Added,
    Duplicate,
}

impl ConnectionList {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn connections(&self) -> &[Connection] {
        &self.items
    }

    pub fn add(&mut self, connection: Connection) -> AddResult {
        if self.items.iter().any(|existing| {
            existing.connection_string().as_str() == connection.connection_string().as_str()
        }) {
            return AddResult::Duplicate;
        }
        self.items.push(connection);
        AddResult::Added
    }

    pub fn merge(&mut self, other: ConnectionList) {
        for connection in other.items {
            self.add(connection);
        }
    }

    pub fn rename(&mut self, index: usize, name: Name) -> bool {
        match self.items.get_mut(index) {
            Some(connection) => {
                connection.set_name(name);
                true
            }
            None => false,
        }
    }

    pub fn to_bundle_json(&self) -> Result<String, serde_json::Error> {
        let bundle = Bundle {
            connections: self
                .items
                .iter()
                .map(|connection| BundleItem {
                    name: connection.name().as_str().to_string(),
                    connection_string: connection.connection_string().as_str().to_string(),
                })
                .collect(),
        };
        serde_json::to_string_pretty(&bundle)
    }

    pub fn from_bundle_json(json: &str) -> Result<Self, BundleError> {
        let bundle: Bundle = serde_json::from_str(json)?;
        let mut list = ConnectionList::new();
        for item in bundle.connections {
            let string = ConnectionString::parse(&item.connection_string)?;
            let mut connection = Connection::from_string(string);
            if let Some(name) = Name::new(&item.name) {
                connection.set_name(name);
            }
            list.add(connection);
        }
        Ok(list)
    }
}

#[derive(Serialize, Deserialize)]
struct Bundle {
    connections: Vec<BundleItem>,
}

#[derive(Serialize, Deserialize)]
struct BundleItem {
    name: String,
    connection_string: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Parse(#[from] crate::connection_string::ParseError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(raw: &str) -> Connection {
        Connection::from_string(ConnectionString::parse(raw).unwrap())
    }

    #[test]
    fn add_skips_exact_string_duplicates() {
        let mut list = ConnectionList::new();
        let a = "postgres://ken:pw@localhost/felicity_os";
        assert_eq!(list.add(conn(a)), AddResult::Added);
        assert_eq!(list.add(conn(a)), AddResult::Duplicate);
        assert_eq!(list.connections().len(), 1);
    }

    #[test]
    fn merge_keeps_different_secrets_as_two_connections() {
        let mut list = ConnectionList::new();
        list.add(conn("postgres://ken:pw1@localhost/felicity_os"));
        let mut other = ConnectionList::new();
        other.add(conn("postgres://ken:pw2@localhost/felicity_os"));
        other.add(conn("postgres://ken:pw1@localhost/felicity_os"));
        list.merge(other);
        assert_eq!(list.connections().len(), 2);
        assert_eq!(list.connections()[0].name().as_str(), "felicity_os");
        assert_eq!(list.connections()[1].name().as_str(), "felicity_os");
    }

    #[test]
    fn bundle_roundtrip_preserves_raw_secret() {
        let mut list = ConnectionList::new();
        list.add(conn("postgres://ken:hunter2@localhost/felicity_os"));
        let json = list.to_bundle_json().unwrap();
        assert!(json.contains("hunter2"));
        let restored = ConnectionList::from_bundle_json(&json).unwrap();
        assert_eq!(
            restored.connections()[0].connection_string().as_str(),
            "postgres://ken:hunter2@localhost/felicity_os"
        );
    }

    #[test]
    fn names_may_collide_identity_is_the_string() {
        let mut list = ConnectionList::new();
        list.add(conn("postgres://ken:a@localhost/shop"));
        list.add(conn("/tmp/shop.db"));
        list.rename(1, Name::new("shop").unwrap());
        assert_eq!(list.connections()[0].name().as_str(), "shop");
        assert_eq!(list.connections()[1].name().as_str(), "shop");
        assert_ne!(
            list.connections()[0].connection_string().as_str(),
            list.connections()[1].connection_string().as_str()
        );
    }
}
