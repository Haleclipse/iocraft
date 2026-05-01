use std::collections::HashMap;
use std::sync::Arc;

use iocraft::prelude::*;

use super::core::{RouterConfig, RouterError, RouterResult};
use super::route::{Route, RouteId};

pub type PageRenderer = Box<dyn Fn(Hooks) -> AnyElement<'static> + Send + Sync>;

// ──── ReactiveRouterHandle ────

#[derive(Clone)]
pub struct ReactiveRouterHandle {
    current_route: State<RouteId>,
    history: State<Vec<RouteId>>,
    config: Arc<RouterConfig>,
}

impl ReactiveRouterHandle {
    pub fn new_with_hooks(hooks: &mut Hooks, config: Arc<RouterConfig>) -> RouterResult<Self> {
        let initial = config.initial_route()?;
        let current_route = hooks.use_state(move || initial);
        let history = hooks.use_state(Vec::new);

        Ok(Self {
            current_route,
            history,
            config,
        })
    }

    pub fn navigate(&mut self, id: impl Into<RouteId>) -> RouterResult<()> {
        let target = id.into();
        if !self.config.routes().contains_key(&target) {
            return Err(RouterError::RouteNotFound(target.0));
        }

        let current = self.current_route.read().clone();
        if current != target {
            if self.config.enable_history {
                let max = self.config.max_history;
                let mut hist = self.history.write();
                hist.insert(0, current);
                if hist.len() > max {
                    hist.truncate(max);
                }
            }
            self.current_route.set(target);
        }
        Ok(())
    }

    pub fn go_back(&mut self) -> bool {
        if !self.config.enable_history {
            return false;
        }
        let prev = {
            let mut hist = self.history.write();
            if hist.is_empty() {
                return false;
            }
            hist.remove(0)
        };
        self.current_route.set(prev);
        true
    }

    pub fn can_go_back(&self) -> bool {
        self.config.enable_history && !self.history.read().is_empty()
    }

    pub fn current_route_id(&self) -> RouteId {
        self.current_route.read().clone()
    }

    pub fn current_route(&self) -> Option<&Route> {
        let id = self.current_route.read();
        self.config.routes().get(&*id)
    }

    pub fn current_route_state(&self) -> State<RouteId> {
        self.current_route
    }

    pub fn config(&self) -> &RouterConfig {
        &self.config
    }
}

// ──── Context + Hooks ────

#[derive(Clone)]
pub struct RouterContext {
    pub handle: ReactiveRouterHandle,
}

pub fn use_router(hooks: &mut Hooks) -> ReactiveRouterHandle {
    hooks.use_context::<RouterContext>().handle.clone()
}

pub fn use_route(hooks: &mut Hooks) -> State<RouteId> {
    hooks
        .use_context::<RouterContext>()
        .handle
        .current_route_state()
}

// ──── UIRouter component ────

#[derive(Props)]
pub struct UIRouterProps {
    pub app: Arc<RouterApp>,
}

impl Default for UIRouterProps {
    fn default() -> Self {
        let app = UIRouterBuilder::new()
            .route("default", "Default", |_| {
                element! { Text(content: "Default") }.into()
            })
            .default("default")
            .build()
            .expect("default router");
        Self { app: Arc::new(app) }
    }
}

#[component]
pub fn UIRouter(mut hooks: Hooks, props: &UIRouterProps) -> impl Into<AnyElement<'static>> {
    let router = use_router(&mut hooks);
    let route_id = router.current_route_state().read().clone();

    element! {
        View(width: 100pct, height: 100pct) {
            #(if let Some(renderer) = props.app.pages.get(&route_id) {
                Some(renderer(hooks))
            } else if let Some(fallback) = &props.app.fallback {
                Some(fallback(hooks))
            } else {
                Some(element! {
                    View(
                        width: 100pct,
                        height: 100pct,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        flex_direction: FlexDirection::Column,
                    ) {
                        Text(content: "Route Not Found", weight: Weight::Bold, color: Color::Red)
                        Text(content: format!("Unknown route: {route_id}"))
                    }
                }.into())
            } as Option<AnyElement>)
        }
    }
}

// ──── Builder ────

pub struct UIRouterBuilder {
    config: RouterConfig,
    pages: HashMap<RouteId, PageRenderer>,
    fallback: Option<PageRenderer>,
}

impl UIRouterBuilder {
    pub fn new() -> Self {
        Self {
            config: RouterConfig::new(),
            pages: HashMap::new(),
            fallback: None,
        }
    }

    pub fn route<F>(mut self, id: impl Into<RouteId>, name: impl Into<String>, renderer: F) -> Self
    where
        F: Fn(Hooks) -> AnyElement<'static> + Send + Sync + 'static,
    {
        let route = Route::new(id.into(), name.into());
        let route_id = route.id.clone();
        self.config = self.config.add_route(route);
        self.pages.insert(route_id, Box::new(renderer));
        self
    }

    pub fn fallback<F>(mut self, renderer: F) -> Self
    where
        F: Fn(Hooks) -> AnyElement<'static> + Send + Sync + 'static,
    {
        self.fallback = Some(Box::new(renderer));
        self
    }

    pub fn default(mut self, id: impl Into<RouteId>) -> Self {
        self.config = self.config.with_default_route(id);
        self
    }

    pub fn max_history(mut self, max: usize) -> Self {
        self.config = self.config.with_max_history(max);
        self
    }

    pub fn build(self) -> RouterResult<RouterApp> {
        let _ = self.config.initial_route()?;
        Ok(RouterApp {
            config: Arc::new(self.config),
            pages: self.pages,
            fallback: self.fallback,
        })
    }
}

impl Default for UIRouterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RouterApp {
    pub config: Arc<RouterConfig>,
    pub pages: HashMap<RouteId, PageRenderer>,
    pub fallback: Option<PageRenderer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_basic() {
        let app = UIRouterBuilder::new()
            .route("home", "Home", |_| element!(Text(content: "H")).into())
            .route("about", "About", |_| element!(Text(content: "A")).into())
            .default("home")
            .build()
            .expect("build");

        assert_eq!(app.config.initial_route().unwrap().0, "home");
        assert_eq!(app.pages.len(), 2);
    }

    #[test]
    fn builder_no_routes_fails() {
        let result = UIRouterBuilder::new().default("x").build();
        assert!(result.is_err());
    }

    #[test]
    fn route_id_traits() {
        let id = RouteId::from("test");
        assert_eq!(format!("{id}"), "test");
        assert_eq!(id.as_ref(), "test");

        use std::borrow::Borrow;
        let s: &str = id.borrow();
        assert_eq!(s, "test");
    }
}
