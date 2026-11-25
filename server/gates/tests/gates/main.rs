//! The gates that read the workspace's source as text: no app, API
//! document, database or value from a workspace crate. Gates that need one
//! of those stay in the server's test binaries.

#[allow(dead_code)]
#[path = "../../../tests/common/sources.rs"]
mod sources;

mod common {
    pub(crate) use super::sources;
}

mod caller_shapes;
mod compat_logging;
mod http_gate;
mod providers_gate;
mod raw_sql;
mod route_gate;
mod source_gates;
mod wiring_gate;
