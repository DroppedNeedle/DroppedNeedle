//! Scripted identification providers.

use std::future::Future;

use droppedneedle_providers::RequestPriority;

use crate::identify::models::{LocalAlbumFacts, RecallResult};
use crate::identify::providers::*;
use crate::identify::sources::{EditionPage, EditionQuery, SourceError};

/// Scripted providers for tests: no network, fully deterministic.
#[derive(Debug, Default)]
pub struct FakeProviders {
    pub recall: std::sync::Mutex<Option<RecallResult>>,
    /// Scripted edition search page.
    pub editions: std::sync::Mutex<Option<EditionPage>>,
}

impl FakeProviders {
    pub fn with_recall(result: RecallResult) -> Self {
        Self {
            recall: std::sync::Mutex::new(Some(result)),
            editions: std::sync::Mutex::new(None),
        }
    }

    fn scripted(&self) -> RecallResult {
        self.recall
            .lock()
            .map(|slot| slot.clone().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Replace the scripted recall. Test wiring uses this to
    /// seed case-specific recall after scan assigns real track ids.
    pub fn set_recall(&self, result: RecallResult) {
        if let Ok(mut slot) = self.recall.lock() {
            *slot = Some(result);
        }
    }
}

impl IdentifyProviders for FakeProviders {
    /// The scripted recall, naming `named_group` unless the script names
    /// one.
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        named_group: Option<&str>,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let mut result = self.scripted();
        if result.named_group.is_none() {
            result.named_group = named_group.map(str::to_ascii_lowercase);
        }
        Box::pin(async move {
            RecallOutcome {
                result,
                scored: Scored::default(),
            }
        })
    }

    fn recall_release(
        &self,
        _facts: &LocalAlbumFacts,
        release_mbid: &str,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let mut result = self.scripted();
        result
            .releases
            .retain(|release| release.answers_to(release_mbid));
        Box::pin(async move {
            RecallOutcome {
                result,
                scored: Scored::default(),
            }
        })
    }

    fn search_editions(
        &self,
        _query: EditionQuery,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<EditionPage, SourceError>> + Send + '_>> {
        let page = self
            .editions
            .lock()
            .map(|slot| slot.clone().unwrap_or_default())
            .unwrap_or_default();
        Box::pin(async move { Ok(page) })
    }
}
