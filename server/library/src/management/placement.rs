//! How safely each library folder can be written to: the placement mode
//! of every saved root that is not excluded, and the probe an
//! administrator can run again. The modes are held in memory with the
//! open publisher session. Reading them never probes.

use super::models::ManagementError;
use super::service::Management;
use crate::manage::{PlacementCheck, PublishCell};
use crate::publish::PlacementMode;

impl Management {
    /// Every saved root files can be written under, with its placement
    /// mode as last probed. `Unknown` until a write or a check has probed
    /// it, which covers a root saved since the session opened. Reads the
    /// settings, so blocking.
    pub fn placement_modes(&self) -> Vec<(String, PlacementMode)> {
        let registry = match crate::settings::registry(&self.setup.config) {
            Ok(registry) => registry,
            Err(error) => {
                tracing::warn!(%error, "cannot read the library roots for the placement modes");
                return Vec::new();
            }
        };
        PublishCell::pending_ids(&registry)
            .into_iter()
            .map(|id| {
                let mode = self.setup.placement.mode(&id);
                (id, mode)
            })
            .collect()
    }

    /// Probe one root now. The probe makes and removes its own folder at
    /// the top of the root. A root excluded or removed in the settings is
    /// not found, whatever the open session still holds. Waits for a write
    /// in flight. Blocking.
    pub fn check_placement(&self, root_id: &str) -> Result<PlacementCheck, ManagementError> {
        self.setup
            .check_placement(root_id)
            .map_err(|error| {
                tracing::warn!(%error, root_id, "placement check could not open the library folders");
                ManagementError::Conflict(
                    "The library folders are not available right now. Try again later.".to_owned(),
                )
            })?
            .ok_or(ManagementError::NotFound("library folder"))
    }
}
