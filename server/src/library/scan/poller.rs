//! Central library-revision poller feeding the event bus.
//!
//! Port of v2's library revision poller. One
//! process-wide poll replaces the per-connection poll loops library SSE
//! streams used to run: every subscriber shares a single published feed.
//! The loop publishes only when revisions change, re-resolves its getters
//! every iteration (a settings-save rebuild never strands it on a stale
//! instance), and survives transient source errors.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Channel carrying revision publications.
pub const LIBRARY_REVISIONS_CHANNEL: &str = "library-revisions";
/// Event name for changed revisions.
pub const ACTIVITY_CHANGED_EVENT: &str = "activity.changed";

/// Revision source: one read of every stream revision (v2
/// `ActivityRevisionSource`). Fallible so transient DB errors stay
/// survivable inside the loop.
pub trait RevisionSource: Send + Sync {
    fn stream_revisions(&self) -> Result<HashMap<String, u64>, String>;
}

/// Revision publisher: one fire-and-forget publication.
pub trait RevisionPublisher: Send + Sync {
    fn publish(&self, channel: &str, event: &str, event_id: &str, revisions: &HashMap<String, u64>);
}

/// Stable event id for one revision map (v2 `_revision_event_id`):
/// `activity:` plus the first 16 sha256 hex digits of the sorted map.
pub fn revision_event_id(revisions: &HashMap<String, u64>) -> String {
    let mut sorted: Vec<(&String, &u64)> = revisions.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let encoded = serde_json::to_string(&sorted).unwrap_or_default();
    let digest = Sha256::digest(encoded.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("activity:{}", &hex[..16])
}

/// Poller loop state: the last published revision map.
#[derive(Debug, Default)]
pub struct PollerState {
    previous: Option<HashMap<String, u64>>,
}

impl PollerState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// One poll iteration: read revisions, publish when changed. Returns true
/// when a publication went out. Source errors log and publish nothing.
pub fn poll_once<S, P>(state: &mut PollerState, source: &S, publisher: &P) -> bool
where
    S: RevisionSource,
    P: RevisionPublisher,
{
    let revisions = match source.stream_revisions() {
        Ok(revisions) => revisions,
        Err(error) => {
            tracing::error!(error = %error, "library revision poll failed");
            return false;
        }
    };
    if state.previous.as_ref() == Some(&revisions) {
        return false;
    }
    state.previous = Some(revisions.clone());
    publisher.publish(
        LIBRARY_REVISIONS_CHANNEL,
        ACTIVITY_CHANGED_EVENT,
        &revision_event_id(&revisions),
        &revisions,
    );
    true
}

/// Run the poll loop until `shutdown` is set. Getters resolve every
/// iteration so a singleton rebuild never strands the loop.
///
/// The wired bundle publishes no library-revision feed yet, so no
/// production loop calls this; the unit tests pin the iteration and
/// the scan tests pin the shutdown behavior. The first SSE or
/// subscriber feed over [`RevisionSource`] state must drive this
/// loop (or [`poll_once`] per tick) instead of polling per
/// connection.
pub async fn poll_library_revisions_periodically<S, P>(
    source_getter: &dyn Fn() -> S,
    publisher_getter: &dyn Fn() -> P,
    interval: Duration,
    shutdown: &AtomicBool,
) where
    S: RevisionSource,
    P: RevisionPublisher,
{
    let mut state = PollerState::new();
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        poll_once(&mut state, &source_getter(), &publisher_getter());
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct ScriptedSource {
        revisions: Mutex<Vec<Result<HashMap<String, u64>, String>>>,
    }

    impl RevisionSource for ScriptedSource {
        fn stream_revisions(&self) -> Result<HashMap<String, u64>, String> {
            self.revisions
                .lock()
                .expect("source lock")
                .pop()
                .unwrap_or_else(|| Ok(HashMap::new()))
        }
    }

    struct RecordingPublisher {
        publications: Mutex<Vec<(String, String, String)>>,
    }

    impl RevisionPublisher for RecordingPublisher {
        fn publish(
            &self,
            channel: &str,
            event: &str,
            event_id: &str,
            _revisions: &HashMap<String, u64>,
        ) {
            self.publications.lock().expect("publisher lock").push((
                channel.to_owned(),
                event.to_owned(),
                event_id.to_owned(),
            ));
        }
    }

    #[test]
    fn event_id_is_stable_and_short() {
        let mut revisions = HashMap::new();
        revisions.insert("scan".to_owned(), 7u64);
        revisions.insert("activity".to_owned(), 3u64);
        let first = revision_event_id(&revisions);
        assert!(first.starts_with("activity:"));
        assert_eq!(first.len(), "activity:".len() + 16);
        assert_eq!(first, revision_event_id(&revisions));
        revisions.insert("scan".to_owned(), 8u64);
        assert_ne!(first, revision_event_id(&revisions));
    }

    #[test]
    fn poll_publishes_only_on_change_and_survives_errors() {
        let mut first = HashMap::new();
        first.insert("scan".to_owned(), 1u64);
        let mut second = HashMap::new();
        second.insert("scan".to_owned(), 2u64);
        let source = ScriptedSource {
            // Popped from the back: error, first, first, second.
            revisions: Mutex::new(vec![
                Ok(second),
                Ok(first.clone()),
                Ok(first),
                Err("db busy".to_owned()),
            ]),
        };
        let publisher = RecordingPublisher {
            publications: Mutex::new(Vec::new()),
        };
        let mut state = PollerState::new();
        assert!(
            !poll_once(&mut state, &source, &publisher),
            "error publishes nothing"
        );
        assert!(
            poll_once(&mut state, &source, &publisher),
            "first read publishes"
        );
        assert!(
            !poll_once(&mut state, &source, &publisher),
            "repeat publishes nothing"
        );
        assert!(
            poll_once(&mut state, &source, &publisher),
            "change publishes"
        );
        let publications = publisher.publications.lock().expect("lock");
        assert_eq!(publications.len(), 2);
        assert_eq!(publications[0].0, LIBRARY_REVISIONS_CHANNEL);
        assert_eq!(publications[0].1, ACTIVITY_CHANGED_EVENT);
    }
}
