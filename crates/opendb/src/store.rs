use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::list::{BundleError, ConnectionList};

pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ConnectionList, StoreError> {
        match fs::read_to_string(&self.path) {
            Ok(json) => Ok(ConnectionList::from_bundle_json(&json)?),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(ConnectionList::new()),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    pub fn save(&self, list: &ConnectionList) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = list.to_bundle_json()?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Connection;
    use crate::connection_string::ConnectionString;

    #[test]
    fn missing_file_is_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("connection-list.json"));
        assert!(store.load().unwrap().connections().is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("connection-list.json"));
        let mut list = ConnectionList::new();
        list.add(Connection::from_string(
            ConnectionString::parse("postgres://ken:pw@localhost/felicity_os").unwrap(),
        ));
        store.save(&list).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.connections().len(), 1);
        assert_eq!(loaded.connections()[0].name().as_str(), "felicity_os");
    }
}
