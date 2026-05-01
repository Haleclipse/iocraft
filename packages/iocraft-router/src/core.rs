use std::collections::HashMap;

use super::route::{Route, RouteId};

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("No routes configured")]
    NoRoutes,
    #[error("Route '{0}' not found")]
    RouteNotFound(String),
    #[error("Initial route '{0}' not found in configuration")]
    InitialRouteMissing(String),
}

pub type RouterResult<T> = Result<T, RouterError>;

#[derive(Debug, Clone)]
pub struct RouterConfig {
    routes: HashMap<RouteId, Route>,
    default_route: Option<RouteId>,
    pub enable_history: bool,
    pub max_history: usize,
}

impl RouterConfig {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            default_route: None,
            enable_history: true,
            max_history: 50,
        }
    }

    pub fn add_route(mut self, route: Route) -> Self {
        let id = route.id.clone();
        if route.is_default && self.default_route.is_none() {
            self.default_route = Some(id.clone());
        }
        self.routes.insert(id, route);
        self
    }

    pub fn with_default_route(mut self, id: impl Into<RouteId>) -> Self {
        self.default_route = Some(id.into());
        self
    }

    pub fn with_max_history(mut self, max: usize) -> Self {
        self.max_history = max;
        self
    }

    pub fn default_route(&self) -> Option<&RouteId> {
        self.default_route.as_ref()
    }

    pub fn routes(&self) -> &HashMap<RouteId, Route> {
        &self.routes
    }

    pub fn initial_route(&self) -> RouterResult<RouteId> {
        if let Some(default) = &self.default_route {
            if self.routes.contains_key(default) {
                Ok(default.clone())
            } else {
                Err(RouterError::InitialRouteMissing(default.0.clone()))
            }
        } else if let Some((id, _)) = self.routes.iter().next() {
            Ok(id.clone())
        } else {
            Err(RouterError::NoRoutes)
        }
    }
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self::new()
    }
}
