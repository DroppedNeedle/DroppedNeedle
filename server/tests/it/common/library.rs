//! MusicBrainz and AcoustID as library identification sees them, scripted
//! by a journey.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use droppedneedle::library::identify::models::{LocalAlbumFacts, RecallResult};
use droppedneedle::library::identify::providers::{IdentifyProviders, RecallOutcome};
use droppedneedle::library::identify::sources::{EditionPage, EditionQuery, SourceError};
use droppedneedle::providers::musicbrainz::Criticality;

type Recall = Box<dyn Fn(&LocalAlbumFacts) -> RecallResult + Send>;

/// The identification port of a [`TestApp`](super::TestApp), answering from
/// what the journey scripted. Nothing is found until it scripts something.
pub struct ScriptedIdentify {
    recall: Mutex<Recall>,
    editions: Mutex<EditionPage>,
}

impl ScriptedIdentify {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            recall: Mutex::new(Box::new(|_| RecallResult::default())),
            editions: Mutex::new(EditionPage::default()),
        })
    }

    /// Answer every recall with `result`.
    pub fn set_recall(&self, result: RecallResult) {
        self.recall_from(move |_| result.clone());
    }

    /// Answer each recall from the album it is asked about. A journey sets
    /// this before the scan, so the identification the scan queues finds it
    /// whenever the loop gets to it.
    pub fn recall_from(&self, recall: impl Fn(&LocalAlbumFacts) -> RecallResult + Send + 'static) {
        *self.recall.lock().expect("recall script") = Box::new(recall);
    }

    /// Answer every edition search with `page`.
    pub fn set_editions(&self, page: EditionPage) {
        *self.editions.lock().expect("edition script") = page;
    }

    fn recall(&self, facts: &LocalAlbumFacts) -> RecallResult {
        (self.recall.lock().expect("recall script"))(facts)
    }
}

fn outcome(result: RecallResult) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + 'static>> {
    Box::pin(async move {
        RecallOutcome {
            result,
            recall_criticality: Some(Criticality::IdentityCritical),
        }
    })
}

impl IdentifyProviders for ScriptedIdentify {
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        outcome(self.recall(facts))
    }

    fn recall_release(
        &self,
        facts: &LocalAlbumFacts,
        release_mbid: &str,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let mut result = self.recall(facts);
        result
            .releases
            .retain(|release| release.answers_to(release_mbid));
        outcome(result)
    }

    fn search_editions(
        &self,
        _query: EditionQuery,
    ) -> Pin<Box<dyn Future<Output = Result<EditionPage, SourceError>> + Send + '_>> {
        let page = self.editions.lock().expect("edition script").clone();
        Box::pin(async move { Ok(page) })
    }
}
