//! What the requests routes read from the caller beyond access.
//!
//! Who may call a route is decided by the `User`, `Curator` and `Admin`
//! extractors from the auth crate, and an admin-only service method takes
//! an `&Admin`. What stays here is role-based behaviour that is not an
//! access check, and the owner-or-admin test, which goes through the
//! checked conversion so work done for a user never reads as an admin.

use droppedneedle_auth::{Admin, Principal, Role};

/// Whether an ask skips the approval queue: trusted and admin asks
/// auto-approve and dispatch, a user's wait for review (v2).
pub fn auto_approves(role: Role) -> bool {
    role.is_curator()
}

/// Whether the caller acts on every user's rows rather than only their
/// own.
pub fn is_admin(principal: &Principal) -> bool {
    Admin::try_from(principal.clone()).is_ok()
}
