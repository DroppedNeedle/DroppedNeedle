//! Route registration with its posture. An area declares each route once,
//! with `routes!` over the handler's `#[utoipa::path]`, in the group that
//! says who may call it. The route and its API document entry come from the
//! same declaration, so they cannot drift, and each operation's `security`
//! in the document is stamped from its group. The server crate layers each
//! group: the session gate for user, curator and admin, the rate limiter
//! for all. An own-credential group never gets the session gate: its
//! handlers check their own credential.
//!
//! One path per `routes!` call: a call listing handlers at different paths
//! mounts every handler at every one of those paths. `route_walk` in the
//! server tests catches it.
//!
//! The compat shims (Subsonic, Jellyfin) register through [`CompatRoutes`]
//! instead: they are not in the API document, carry their own credentials
//! outside the session gate, and record each path template so the app can
//! fix a request path's case before routing.
//!
//! Nothing else registers a route; `route_gate` in the server tests keeps
//! it that way.

use axum::{handler::Handler, routing::MethodRouter};
use utoipa::openapi::{Components, OpenApi, security::SecurityRequirement};
pub use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouter},
    routes,
};

/// The document's security scheme for a signed-in session. Its scope names
/// the group: `user`, `curator` or `admin`.
pub const SESSION_SCHEME: &str = "session";

/// The document's security scheme for the wrapped API key.
pub const WRAPPED_KEY_SCHEME: &str = "wrapped_key";

fn session(posture: &str) -> SecurityRequirement {
    SecurityRequirement::new(SESSION_SCHEME, [posture])
}

/// A credential a route checks itself, outside the session gate.
pub enum OwnCredential {
    /// The shared `X-Wrapped-API-Key` secret of the wrapped API.
    WrappedKey,
}

/// An area's routes, grouped by who may call them. Building one needs no
/// state, so the API document can be produced without a running server.
///
/// The groups sit behind a box: areas build their routes in long chains of
/// calls that pass this by value, and a debug build gives each call its
/// own stack slots, so an inline value made those frames large enough to
/// overflow a test thread's stack.
pub struct Routes<S = ()>(Box<Postures<S>>);

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
    /// Whoever holds the wrapped API key, no session.
    pub wrapped_key: OpenApiRouter<S>,
}

impl<S: Clone + Send + Sync + 'static> Default for Routes<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone + Send + Sync + 'static> Routes<S> {
    /// No routes yet.
    pub fn new() -> Self {
        Self(Box::new(Postures {
            public: OpenApiRouter::default(),
            user: OpenApiRouter::default(),
            curator: OpenApiRouter::default(),
            admin: OpenApiRouter::default(),
            wrapped_key: OpenApiRouter::default(),
        }))
    }

    /// Routes anyone may call.
    pub fn public(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        let group = std::mem::take(&mut self.0.public);
        self.0.public = group.routes(routes);
        self
    }

    /// Routes any signed-in user may call.
    pub fn user(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        let group = std::mem::take(&mut self.0.user);
        self.0.user = group.routes(routes);
        self
    }

    /// A user route whose last path parameter takes the rest of the path,
    /// slashes included. The document names it `{x}`; the router mounts it
    /// as `{*x}`.
    pub fn user_at(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        let group = std::mem::take(&mut self.0.user);
        self.0.user = with_rest_parameter(group, routes);
        self
    }

    /// Routes an admin or trusted user may call.
    pub fn curator(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        let group = std::mem::take(&mut self.0.curator);
        self.0.curator = group.routes(routes);
        self
    }

    /// Routes only an admin may call.
    pub fn admin(mut self, routes: UtoipaMethodRouter<S>) -> Self {
        let group = std::mem::take(&mut self.0.admin);
        self.0.admin = group.routes(routes);
        self
    }

    /// Routes whose handlers check `kind` themselves, with no session.
    pub fn own_credential(mut self, kind: OwnCredential, routes: UtoipaMethodRouter<S>) -> Self {
        match kind {
            OwnCredential::WrappedKey => {
                let group = std::mem::take(&mut self.0.wrapped_key);
                self.0.wrapped_key = group.routes(routes);
            }
        }
        self
    }

    /// Both sets of routes, each in its own group.
    pub fn merge(self, other: Routes<S>) -> Self {
        let (mine, other) = (*self.0, *other.0);
        Self(Box::new(Postures {
            public: mine.public.merge(other.public),
            user: mine.user.merge(other.user),
            curator: mine.curator.merge(other.curator),
            admin: mine.admin.merge(other.admin),
            wrapped_key: mine.wrapped_key.merge(other.wrapped_key),
        }))
    }

    /// The document entries of every group, each operation's `security`
    /// stamped from its group. The stamp replaces whatever an annotation
    /// said, so the group is the only source of an operation's posture.
    pub fn openapi(&self) -> OpenApi {
        let groups = &self.0;
        let mut doc = OpenApi::default();
        for (group, posture) in [
            (&groups.public, SecurityRequirement::default()),
            (&groups.user, session("user")),
            (&groups.curator, session("curator")),
            (&groups.admin, session("admin")),
            (
                &groups.wrapped_key,
                SecurityRequirement::new(WRAPPED_KEY_SCHEME, Vec::<String>::new()),
            ),
        ] {
            let mut part = group.get_openapi().clone();
            for item in part.paths.paths.values_mut() {
                for operation in [
                    &mut item.get,
                    &mut item.put,
                    &mut item.post,
                    &mut item.delete,
                    &mut item.options,
                    &mut item.head,
                    &mut item.patch,
                    &mut item.trace,
                ]
                .into_iter()
                .flatten()
                {
                    operation.security = Some(vec![posture.clone()]);
                }
            }
            doc.merge(part);
        }
        doc
    }

    /// Give every group its state.
    pub fn with_state<S2>(self, state: S) -> Routes<S2> {
        let groups = *self.0;
        Routes(Box::new(Postures {
            public: groups.public.with_state(state.clone()),
            user: groups.user.with_state(state.clone()),
            curator: groups.curator.with_state(state.clone()),
            admin: groups.admin.with_state(state.clone()),
            wrapped_key: groups.wrapped_key.with_state(state),
        }))
    }

    /// The groups, for layering.
    pub fn into_postures(self) -> Postures<S> {
        *self.0
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
            wrapped_key,
        } = self.into_postures();
        public
            .merge(user)
            .merge(curator)
            .merge(admin)
            .merge(wrapped_key)
            .into()
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

/// The routes of a compat shim, at the paths its clients call, with the
/// template of each recorded in registration order. Not documented and
/// never behind the session gate: the shim checks its own credentials.
pub struct CompatRoutes<S = ()> {
    router: axum::Router<S>,
    templates: Vec<&'static str>,
}

impl<S: Clone + Send + Sync + 'static> Default for CompatRoutes<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone + Send + Sync + 'static> CompatRoutes<S> {
    /// No routes yet.
    pub fn new() -> Self {
        Self {
            router: axum::Router::new(),
            templates: Vec::new(),
        }
    }

    /// Serve `methods` at `template` and record the template.
    pub fn at(mut self, template: &'static str, methods: MethodRouter<S>) -> Self {
        self.router = self.router.route(template, methods);
        self.templates.push(template);
        self
    }

    /// The answer to a known path called with a method it does not serve.
    pub fn method_not_allowed_fallback<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.router = self.router.method_not_allowed_fallback(handler);
        self
    }

    /// Give the routes their state.
    pub fn with_state<S2>(self, state: S) -> CompatRoutes<S2> {
        CompatRoutes {
            router: self.router.with_state(state),
            templates: self.templates,
        }
    }

    /// The router and the recorded templates.
    pub fn into_parts(self) -> (axum::Router<S>, Vec<&'static str>) {
        (self.router, self.templates)
    }
}
