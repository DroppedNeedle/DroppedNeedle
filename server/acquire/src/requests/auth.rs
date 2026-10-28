//! What the requests routes read from the caller beyond access.
//!
//! Who may call a route is decided by the `User`, `Curator` and `Admin`
//! extractors from the auth crate, and an admin-only service method takes
//! an `&Admin`. What stays here is role-based behaviour that is not an
//! access check. The owner-or-admin test is `Principal::is_admin`, so
//! work done for a user never reads as an admin.

use droppedneedle_auth::Role;

/// Whether an ask skips the approval queue: trusted and admin asks
/// auto-approve and dispatch, a user's wait for review.
pub fn auto_approves(role: Role) -> bool {
    role.is_curator()
}
