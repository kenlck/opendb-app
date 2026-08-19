use crate::connection_string::ConnectionString;
use crate::name::Name;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    connection_string: ConnectionString,
    name: Name,
}

impl Connection {
    pub fn from_string(connection_string: ConnectionString) -> Self {
        let name = connection_string.default_name().clone();
        Self {
            connection_string,
            name,
        }
    }

    pub fn name(&self) -> &Name {
        &self.name
    }

    pub fn set_name(&mut self, name: Name) {
        self.name = name;
    }

    pub fn connection_string(&self) -> &ConnectionString {
        &self.connection_string
    }
}
