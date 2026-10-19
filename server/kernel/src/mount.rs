//! Route registration with its posture. An area declares each route once,
//! with `routes!` over the handler's `#[utoipa::path]`, in the group that
//! says who may call it. The route and its API document entry come from the
//! same declaration, so they cannot drift. The server crate layers each
//! group: the session gate for user, curator and admin, the rate limiter
//! for all.
//!
//! One path per `routes!` call: a call listing handlers at different paths
//! mounts every handler at every one of those paths. `route_walk` in the
//! server tests catches it.

use utoipa::openapi::{Components, OpenApi};
pub use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouter},
    routes,
};

/// An area's routes, grouped by who may call them. Building one needs no
/// state, so the API document can be produced without a running server.
pub struct Routes<S = ()> {
    public: OpenApiRouter<S>,
    user: OpenApiRouter<S>,
    curator: OpenApiRouter<S>,
    admin: OpenApiRouter<S>,
}

/// The groups of a [`Routes`], for the server crate to layer.
pub struct Postures<S = ()> {
    /// Anyone, no session.
    pub public: OpenApiRouter<S>,
    /// Any signed-in user.
    pub user: OpenApiRouter<S>,
    /// Admin or trusted.
    pub curator: OpenApiRouter<S>,
    /// Admin only.
    pub admin: OpenApiRouter<S>,
}

impl<S: Clone + Send + Sync + 'static> Default for Routes<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone + Send + Sync + 'static> Routes<S> {
    /// No routes yet.
    pub fn new() -> Self {
        Self {
            public: OpenApiRouter::default(),
            user: OpenApiRouter::default(),
            curator: OpenApiRouter::default(),
            admin: OpenApiRouter::default(),
        }
    }

    /// Routes anyone may call.
    pub fn public(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        self.public = self.public.routes(routes);
        self
    }

    /// Routes any signed-in user may call.
    pub fn user(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        self.user = self.user.routes(routes);
        self
    }

    /// A user route whose last path parameter takes the rest of the path,
    /// slashes included. The document names it `{x}`; the router mounts it
    /// as `{*x}`.
    pub fn user_at(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        self.user = with_rest_parameter(self.user, routes);
        self
    }

    /// Routes an admin or trusted user may call.
    pub fn curator(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        self.curator = self.curator.routes(routes);
        self
    }

    /// Routes only an admin may call.
    pub fn admin(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        self.admin = self.admin.routes(routes);
        self
    }

    /// Both sets of routes, each in its own group.
    pub fn merge(self, other: Routes<S>) -> Self {
        Self {
            public: self.public.merge(other.public),
            user: self.user.merge(other.user),
            curator: self.curator.merge(other.curator),
            admin: self.admin.merge(other.admin),
        }
    }

    /// The document entries of every group.
    pub fn openapi(&self) -> OpenApi {
        let mut doc = self.public.get_openapi().clone();
        for group in [&self.user, &self.curator, &self.admin] {
            doc.merge(group.get_openapi().clone());
        }
        doc
    }

    /// Give every group its state.
    pub fn with_state<S2>(self, state: S) -> Routes<S2> {
        Routes {
            public: self.public.with_state(state.clone()),
            user: self.user.with_state(state.clone()),
            curator: self.curator.with_state(state.clone()),
            admin: self.admin.with_state(state),
        }
    }

    /// The groups, for layering.
    pub fn into_postures(self) -> Postures<S> {
        Postures {
            public: self.public,
            user: self.user,
            curator: self.curator,
            admin: self.admin,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Routes<()> {
    /// Every group in one router with no layers, for tests that mount one
    /// area alone.
    pub fn into_router(self) -> axum::Router {
        let Postures {
            public,
            user,
            curator,
            admin,
        } = self.into_postures();
        public.merge(user).merge(curator).merge(admin).into()
    }
}

/// Mount `routes` with its last `{x}` turned into `{*x}`, and document it
/// at the annotated path.
fn with_rest_parameter<S: Clone + Send + Sync + 'static>(
    mut router: OpenApiRouter<S>,
    (schemas, paths, method_router): UtoipaMethodRouter<S>,
) -> OpenApiRouter<S> {
    for path in paths.paths.keys() {
        router = router.route(&rest_path(path), method_router.clone());
    }
    let doc = router.get_openapi_mut();
    doc.paths.merge(paths);
    doc.components
        .get_or_insert_with(Components::new)
        .schemas
        .extend(schemas);
    router
}

/// `/a/{b}/{c}` becomes `/a/{b}/{*c}`. A path with no parameter is kept.
fn rest_path(path: &str) -> String {
    match path.rfind('{') {
        Some(at) => format!("{}{{*{}", &path[..at], &path[at + 1..]),
        None => path.to_owned(),
    }
}
