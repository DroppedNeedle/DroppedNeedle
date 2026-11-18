//! Scripted identification providers and a review fixture.

use std::future::Future;

use droppedneedle_providers::RequestPriority;

use crate::identify::models::{
    CandidateEvidence, LocalAlbumFacts, RecallResult, ReviewItem, ReviewState,
};
use crate::identify::providers::*;
use crate::identify::sources::{EditionPage, EditionQuery, SourceError};
use crate::identify::sqlite::SqliteIdentifyStore;
use crate::identify::stores::StoreError;

/// Scripted providers for tests: no network, fully deterministic.
#[derive(Debug, Default)]
pub struct FakeProviders {
    pub recall: std::sync::Mutex<Option<RecallResult>>,
    /// Scripted edition search page.
    pub editions: std::sync::Mutex<Option<EditionPage>>,
    /// The albums candidates were recalled for, in order.
    pub recalled: std::sync::Mutex<Vec<String>>,
    /// A group recall reports in place of the lowered one it was asked
    /// about, as the real recall does for a tagged release that moved.
    pub moved_groups: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl FakeProviders {
    pub fn with_recall(result: RecallResult) -> Self {
        Self {
            recall: std::sync::Mutex::new(Some(result)),
            ..Self::default()
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
    /// The scripted recall, naming `named_group` (or the group it moved
    /// to) unless the script names one.
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        named_group: Option<&str>,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        if let Ok(mut recalled) = self.recalled.lock() {
            recalled.push(facts.local_album_id.clone());
        }
        let mut result = self.scripted();
        if result.named_group.is_none() {
            let asked = named_group.map(str::to_ascii_lowercase);
            let moved = asked.as_ref().and_then(|group| {
                self.moved_groups
                    .lock()
                    .ok()
                    .and_then(|moved| moved.get(group).cloned())
            });
            result.named_group = moved.or(asked);
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

/// File one review for an ambiguous album. Returns the stored item.
pub fn file_review(
    store: &SqliteIdentifyStore,
    id: &str,
    local_album_id: &str,
    reason_code: &str,
    candidates: Vec<CandidateEvidence>,
) -> Result<ReviewItem, StoreError> {
    let review = ReviewItem {
        id: id.to_owned(),
        local_album_id: local_album_id.to_owned(),
        reason_code: reason_code.to_owned(),
        candidates,
        state: ReviewState::Pending,
        resolved_by_user_id: None,
        selected_candidate_key: None,
    };
    store.file(review.clone())?;
    Ok(review)
}
