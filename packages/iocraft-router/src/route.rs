use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteId(pub String);

impl From<&str> for RouteId {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

impl From<String> for RouteId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl fmt::Display for RouteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for RouteId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for RouteId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Route {
    pub id: RouteId,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub metadata: HashMap<String, String>,
}

impl Route {
    pub fn new(id: impl Into<RouteId>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            is_default: false,
            metadata: HashMap::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn as_default(mut self) -> Self {
        self.is_default = true;
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}
