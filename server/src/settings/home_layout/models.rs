//! The wire shapes of `/api/v3/me/home-layout`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use droppedneedle_kernel::runtime_config::sections::PersonalizationMode;

/// One widget's cell in the home grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeLayoutWidget {
    /// Stable widget id: a section key with a `shelf:` prefix, or the
    /// singleton's name.
    pub id: String,
    /// Widget kind, from the client's contract.
    pub kind: String,
    /// Column of the top-left cell.
    pub x: i64,
    /// Row of the top-left cell.
    pub y: i64,
    /// Width in cells.
    pub w: i64,
    /// Height in cells.
    pub h: i64,
}

/// Whose layout the response carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HomeLayoutSource {
    /// The caller's own saved layout.
    User,
    /// The admin's saved layout, served because personalization is forced.
    Admin,
    /// Nothing is saved; the client's built-in order applies.
    Default,
}

/// The caller's home layout and who decides it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeLayoutResponse {
    /// Layout schema version; `0` when nothing is saved.
    pub version: i64,
    /// Stored revision (updated_at); `0` when nothing is saved.
    pub revision: i64,
    /// Widgets, empty when the built-in order applies.
    pub widgets: Vec<HomeLayoutWidget>,
    /// Widgets the person hid.
    pub hidden: Vec<String>,
    /// Whether layouts are each person's or the admin's.
    pub posture: PersonalizationMode,
    /// Where the layout came from.
    pub source: HomeLayoutSource,
    /// Whether this caller may save a layout now.
    pub editable: bool,
}

/// A layout to save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeLayoutUpdate {
    /// Layout schema version the client writes.
    pub version: i64,
    /// Stored revision the client is saving against; ignored on a first save.
    pub revision: i64,
    /// Widgets in the caller's order.
    pub widgets: Vec<HomeLayoutWidget>,
    /// Widgets the caller hides.
    pub hidden: Vec<String>,
}
