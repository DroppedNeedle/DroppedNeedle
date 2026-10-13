//! The addresses an admin stated as this server's own: the saved
//! `public_address` setting, read where a request is judged against it.

use std::sync::Arc;

use crate::runtime_config::{ConfigStore, sections::PublicAddress};

use super::{middleware::WarnOnce, origin::Origin};

/// A handle on the saved `public_address` list. Clones share the settings
/// and the one warning. The default states no address.
#[derive(Debug, Clone, Default)]
pub struct StatedAddresses {
    /// The runtime settings. `None` means no address is stated.
    config: Option<Arc<ConfigStore>>,
    /// Spent by the first read that finds the section unreadable.
    unreadable_warned: WarnOnce,
}

impl StatedAddresses {
    /// Read the list from `config`, afresh on every call to
    /// [`StatedAddresses::origins`], so a save applies to the next request.
    pub fn from_config(config: Arc<ConfigStore>) -> Self {
        Self {
            config: Some(config),
            unreadable_warned: WarnOnce::default(),
        }
    }

    /// The stated addresses as origins. Empty when the setting cannot be
    /// read: an unreadable list never widens a check. The log says so once.
    pub fn origins(&self) -> Vec<Origin> {
        let Some(config) = &self.config else {
            return Vec::new();
        };
        match config.get::<PublicAddress>() {
            Ok(section) => section.stated(),
            Err(error) => {
                if self.unreadable_warned.claim() {
                    tracing::warn!(
                        %error,
                        "The public address list in the settings file cannot be read and is \
                         being ignored: until it is rewritten no address counts as stated, \
                         so a change from a page at one is refused and a sign-in there gets \
                         no `Secure` cookie on its account. Stop the server and rewrite the \
                         list with `droppedneedle-tool public-address`."
                    );
                }
                Vec::new()
            }
        }
    }
}
