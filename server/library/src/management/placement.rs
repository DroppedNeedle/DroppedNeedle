//! How safely each library folder can be written to: the placement mode
//! of every saved root that is not excluded, and the probe an
//! administrator can run again. The modes are held in memory with the
//! open publisher session. Reading them never probes.

use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;

use super::models::ManagementError;
use super::service::Management;
use crate::manage::{PlacementCheck, PublishCell, RootPlacement};
use crate::publish::PlacementMode;

impl Management {
    /// Every saved root files can be written under, with its placement
    /// mode as last probed. `Unknown` until a write or a check has probed
    /// it, which covers a root saved since the session opened. Fails when
    /// the saved roots cannot be read: no list is better than one that
    /// shows every folder as safe. Reads the settings, so blocking.
    pub fn placement_modes(&self) -> Result<Vec<(String, PlacementMode)>, ManagementError> {
        placement_modes(&self.setup.settings.library, &self.setup.placement)
    }

    /// Probe one root now. The probe makes and removes its own folder at
    /// the top of the root. A root excluded or removed in the settings is
    /// not found, whatever the open session still holds. Waits for a write
    /// in flight. Blocking.
    pub fn check_placement(&self, root_id: &str) -> Result<PlacementCheck, ManagementError> {
        self.setup
            .publishing.check_placement(root_id)
            .map_err(|error| {
                tracing::warn!(%error, root_id, "placement check could not open the library folders");
                ManagementError::Conflict(
                    "The library folders are not available right now. Try again later.".to_owned(),
                )
            })?
            .ok_or(ManagementError::NotFound("library folder"))
    }
}

fn placement_modes(
    library: &Live<TypedLibrary>,
    placement: &RootPlacement,
) -> Result<Vec<(String, PlacementMode)>, ManagementError> {
    let registry = crate::settings::registry(library).map_err(|error| {
        tracing::warn!(%error, "cannot read the library roots for the placement modes");
        ManagementError::Store(error.to_string())
    })?;
    Ok(PublishCell::pending_ids(&registry)
        .into_iter()
        .map(|id| {
            let mode = placement.mode(&id);
            (id, mode)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};

    use super::*;

    /// Library settings that cannot be read answer an error, never an
    /// empty list: an empty list shows every library folder as safe to
    /// write to.
    #[test]
    fn unreadable_roots_are_an_error_not_an_empty_list() {
        let dir =
            std::env::temp_dir().join(format!("dn-placement-unreadable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // Copied in from `config.json` unvalidated: the roots do not decode.
        std::fs::write(
            dir.join("config.json"),
            r#"{"library_settings": {"library_roots": "not a list"}}"#,
        )
        .expect("config.json");
        let store = ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("settings open");
        let library = store.live::<TypedLibrary>();
        assert!(library.get().is_err(), "the section does not read");

        let answer = placement_modes(&library, &RootPlacement::default());

        drop(store);
        std::fs::remove_dir_all(&dir).expect("scratch dir removed");
        assert!(
            matches!(answer, Err(ManagementError::Store(_))),
            "{answer:?}"
        );
    }
}
