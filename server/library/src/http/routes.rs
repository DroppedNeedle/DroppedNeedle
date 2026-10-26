//! Library routes, at their full `/api/v3` paths.

use droppedneedle_kernel::mount::{Routes, routes};

use super::handlers::*;
use crate::wiring::LibraryServices;

/// Every library route, grouped by who may call it.
pub fn library_routes() -> Routes<LibraryServices> {
    Routes::new()
        .user(routes!(list_roots))
        .admin(routes!(add_root))
        .admin(routes!(trigger_scan))
        .user(routes!(list_runs))
        .user(routes!(get_run))
        .curator(routes!(enqueue_identify))
        .curator(routes!(manage_preview))
        .curator(routes!(manage_apply))
        .curator(routes!(manage_undo))
        .admin(routes!(list_plugin_files))
        .admin(routes!(baseline_restore))
        .merge(super::scan::routes())
        .merge(super::operations::routes())
        .merge(super::corrections::routes())
        .merge(super::management::routes())
        .merge(super::management_tools::routes())
        .merge(super::reconcile::routes())
        .merge(super::repairs::routes())
        .merge(super::reviews::routes())
        .merge(super::mutations::routes())
        .merge(super::recycle_bin::routes())
        .merge(super::edition::routes())
        .merge(super::history::routes())
        .merge(super::contrib::routes())
}
