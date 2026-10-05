//! `scrobbler`, `subscriber` and `publisher`: events out to plugins, and
//! hints from plugins back in.
//!
//! Delivery is fire-and-forget: every notification is its own task under
//! a 5 second budget, so a slow plugin never delays the flow that raised
//! the event. Each plugin has at most one event in flight; an event that
//! arrives while it is busy is skipped and counted.
//!
//! Loop guards for publishing: a plugin may publish while handling an
//! engine event, and the resulting notice reaches the other subscribers
//! once. A plugin handling such a notice cannot publish again (409). Every
//! delivery is remembered by `(causation, subscriber)` for ten minutes, so
//! the same causation never reaches the same plugin twice.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::host::{LoadedPlugin, PluginHost};
use super::super::protocol::methods;
use super::super::runtime::{
    EventKind, EventPayload, NoticeEvent, PluginEvent, PluginPublishResult, PublishPayload,
    ScrobbleEvent, publish_fields,
};
use super::call;

/// Per-plugin event budget.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
/// Per-plugin scrobble budget.
const SCROBBLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Publishes allowed per plugin and principal per minute.
const PUBLISH_RATE_LIMIT: usize = 30;
/// Publish rate window.
const PUBLISH_RATE_WINDOW: Duration = Duration::from_secs(60);
/// Cap on tracked rate buckets.
const PUBLISH_RATE_CAP: usize = 10_000;
/// Cap on remembered deliveries.
const CAUSATION_CAP: usize = 10_000;
/// How long a delivery is remembered.
const CAUSATION_TTL: Duration = Duration::from_secs(10 * 60);
/// Largest `download_note` note (1 KiB).
const NOTE_MAX_BYTES: usize = 1024;
/// Hints kept for the admin view and tests.
const RECENT_PUBLISHED: usize = 100;

/// One accepted publish, as the host recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedRecord {
    /// Publishing plugin, stamped by the host (a plugin cannot spoof it).
    pub source_plugin: String,
    /// Publish kind.
    pub kind: String,
    /// Validated fields.
    pub payload: HashMap<String, String>,
    /// Causation shared with the event that led to it.
    pub causation_id: String,
    /// 1 for a publish outside event handling or while handling an engine
    /// event.
    pub depth: u32,
}

/// Fan-out bookkeeping, owned by the host.
#[derive(Default)]
pub struct EventState {
    /// Plugin name -> (causation, depth) of the event it is handling.
    inflight: Mutex<HashMap<String, (String, u32)>>,
    dropped: Mutex<HashMap<String, u64>>,
    seen: Mutex<HashMap<(String, String), Instant>>,
    publish_rate: Mutex<HashMap<(String, String), Vec<Instant>>>,
    recent: Mutex<VecDeque<PublishedRecord>>,
    published: AtomicU64,
}

impl PluginHost {
    /// Events skipped per plugin because the previous one was still running.
    pub fn dropped_event_counts(&self) -> HashMap<String, u64> {
        self.events
            .dropped
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Accepted publishes since start.
    pub fn published_count(&self) -> u64 {
        self.events.published.load(Ordering::Relaxed)
    }

    /// The most recent accepted publishes, oldest first.
    pub fn recent_published(&self) -> Vec<PublishedRecord> {
        self.events
            .recent
            .lock()
            .map(|guard| guard.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Send one accepted play to every `scrobbler` plugin, in the
    /// background. A failing plugin is logged, never propagated.
    pub fn dispatch_scrobble(self: &Arc<Self>, event: &ScrobbleEvent) {
        let plugins = self.serving("scrobbler");
        if plugins.is_empty() {
            return;
        }
        let Ok(params) = serde_json::to_value(event) else {
            return;
        };
        for plugin in plugins {
            let params = params.clone();
            tokio::spawn(async move {
                let _ = call(&plugin, methods::SCROBBLE, params, SCROBBLE_TIMEOUT).await;
            });
        }
    }

    /// Fan one engine event out to every subscriber without waiting on
    /// any of them. `scrobble` events also reach `scrobbler` plugins.
    pub fn dispatch_event(self: &Arc<Self>, event: PluginEvent) {
        if event.kind == EventKind::Scrobble
            && let EventPayload::Scrobble(scrobble) = &event.payload
        {
            self.dispatch_scrobble(scrobble);
        }
        self.deliver(event, 0);
    }

    fn deliver(self: &Arc<Self>, mut event: PluginEvent, depth: u32) {
        self.sweep_seen();
        if event.causation_id.is_empty() {
            event.causation_id = uuid::Uuid::new_v4().simple().to_string();
        }
        for plugin in self.serving("subscriber") {
            let name = plugin.manifest.name.clone();
            if self.seen_before(&event.causation_id, &name) {
                continue;
            }
            if !self.mark_inflight(&name, &event.causation_id, depth) {
                tracing::warn!(plugin = %name, kind = event.kind.as_str(), "plugin event skipped: previous one still running");
                if let Ok(mut dropped) = self.events.dropped.lock() {
                    *dropped.entry(name).or_insert(0) += 1;
                }
                continue;
            }
            self.remember(&event.causation_id, &name);
            let host = Arc::clone(self);
            let event = event.clone();
            tokio::spawn(async move { host.notify_one(&plugin, &event).await });
        }
    }

    async fn notify_one(&self, plugin: &LoadedPlugin, event: &PluginEvent) {
        let name = &plugin.manifest.name;
        let started = Instant::now();
        if let Ok(params) = serde_json::to_value(event)
            && call(plugin, methods::EVENT, params, EVENT_TIMEOUT)
                .await
                .is_ok()
        {
            tracing::debug!(
                plugin = %name,
                kind = event.kind.as_str(),
                duration_ms = started.elapsed().as_millis() as u64,
                "plugin event delivered"
            );
        }
        if let Ok(mut inflight) = self.events.inflight.lock() {
            inflight.remove(name);
        }
    }

    /// Mark a plugin busy with one event. False when it already is.
    fn mark_inflight(&self, name: &str, causation: &str, depth: u32) -> bool {
        self.events
            .inflight
            .lock()
            .map(|mut inflight| {
                if inflight.contains_key(name) {
                    return false;
                }
                inflight.insert(name.to_owned(), (causation.to_owned(), depth));
                true
            })
            .unwrap_or(false)
    }

    fn seen_before(&self, causation: &str, subscriber: &str) -> bool {
        self.events
            .seen
            .lock()
            .map(|seen| seen.contains_key(&(causation.to_owned(), subscriber.to_owned())))
            .unwrap_or(false)
    }

    fn remember(&self, causation: &str, subscriber: &str) {
        if let Ok(mut seen) = self.events.seen.lock() {
            seen.insert(
                (causation.to_owned(), subscriber.to_owned()),
                Instant::now(),
            );
        }
    }

    fn sweep_seen(&self) {
        let now = Instant::now();
        if let Ok(mut seen) = self.events.seen.lock() {
            if seen.len() < CAUSATION_CAP / 2 {
                return;
            }
            seen.retain(|_, at| now.duration_since(*at) < CAUSATION_TTL);
            while seen.len() >= CAUSATION_CAP {
                let Some(oldest) = seen
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                seen.remove(&oldest);
            }
        }
    }

    /// Validate and act on one plugin publish. Unknown kinds are an error
    /// (the plugin's bug); disabled, invalid, too deep and rate-limited
    /// publishes come back as a result with the matching status. The host
    /// stamps the source plugin; `principal` keys the rate limit.
    pub fn publish_from_plugin(
        self: &Arc<Self>,
        plugin_name: &str,
        kind: &str,
        payload: &PublishPayload,
        principal: &str,
        causation_id: Option<&str>,
    ) -> Result<PluginPublishResult, String> {
        let refused = |status: u16, error: &str, retry_after: u64| PluginPublishResult {
            ok: false,
            status,
            retry_after,
            error: Some(error.to_owned()),
        };
        if !self
            .get(plugin_name)
            .is_some_and(|plugin| plugin.serves("publisher"))
        {
            tracing::warn!(plugin = %plugin_name, %kind, "publish refused: unknown or disabled publisher");
            return Ok(refused(404, "unknown or disabled publisher", 0));
        }
        if publish_fields(kind).is_none() {
            return Err(format!("unknown publish kind '{kind}'"));
        }
        let fields = match validate_payload(kind, payload) {
            Ok(fields) => fields,
            Err(reason) => {
                tracing::warn!(plugin = %plugin_name, %kind, %reason, "publish refused");
                return Ok(refused(422, &reason, 0));
            }
        };
        let handling = self
            .events
            .inflight
            .lock()
            .ok()
            .and_then(|inflight| inflight.get(plugin_name).cloned());
        if let Some((_, depth)) = &handling
            && *depth >= 1
        {
            tracing::warn!(plugin = %plugin_name, %kind, "publish dropped: it would answer a plugin notice");
            return Ok(refused(409, "max publish depth exceeded", 0));
        }
        if let Some(retry_after) = self.publish_rate_hit(plugin_name, principal) {
            tracing::warn!(plugin = %plugin_name, %kind, retry_after, "publish rate limited");
            return Ok(refused(429, "rate_limited", retry_after));
        }
        let causation = match handling {
            Some((parent, _)) => parent,
            None => causation_id
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
        };
        let record = PublishedRecord {
            source_plugin: plugin_name.to_owned(),
            kind: kind.to_owned(),
            payload: fields,
            causation_id: causation,
            depth: 1,
        };
        self.act_on(&record);
        if let Ok(mut recent) = self.events.recent.lock() {
            if recent.len() >= RECENT_PUBLISHED {
                recent.pop_front();
            }
            recent.push_back(record);
        }
        self.events.published.fetch_add(1, Ordering::Relaxed);
        Ok(PluginPublishResult {
            ok: true,
            status: 200,
            retry_after: 0,
            error: None,
        })
    }

    /// What each publish kind does. Notices reach the other subscribers;
    /// notes and invalidation hints are hints only and go to the log,
    /// tagged with the plugin, for the admin to read.
    fn act_on(self: &Arc<Self>, record: &PublishedRecord) {
        let field = |key: &str| record.payload.get(key).cloned().unwrap_or_default();
        match record.kind.as_str() {
            "plugin_notice" => {
                let event = PluginEvent {
                    kind: EventKind::PluginNotice,
                    payload: EventPayload::Notice(NoticeEvent {
                        source_plugin: record.source_plugin.clone(),
                        title: field("title"),
                        body: field("body"),
                    }),
                    causation_id: record.causation_id.clone(),
                };
                tracing::info!(plugin = %record.source_plugin, title = %field("title"), "plugin notice");
                self.deliver(event, record.depth);
            }
            "download_note" => {
                tracing::info!(
                    plugin = %record.source_plugin,
                    task_id = %field("task_id"),
                    note = %field("note"),
                    "plugin note on a download"
                );
            }
            "indexer_invalidate" => {
                tracing::info!(
                    plugin = %record.source_plugin,
                    target_source = %field("target_source"),
                    "plugin asked for a fresh search; the next search for that source asks it again"
                );
            }
            _ => {}
        }
    }

    /// Record a publish; `None` when allowed, else Retry-After seconds.
    fn publish_rate_hit(&self, plugin_name: &str, principal: &str) -> Option<u64> {
        let now = Instant::now();
        let mut guard = self.events.publish_rate.lock().ok()?;
        let key = (plugin_name.to_owned(), principal.to_owned());
        if !guard.contains_key(&key) && guard.len() >= PUBLISH_RATE_CAP {
            guard.retain(|_, hits| {
                hits.iter()
                    .any(|hit| now.duration_since(*hit) < PUBLISH_RATE_WINDOW)
            });
            if guard.len() >= PUBLISH_RATE_CAP {
                return Some(PUBLISH_RATE_WINDOW.as_secs());
            }
        }
        let hits = guard.entry(key).or_default();
        hits.retain(|hit| now.duration_since(*hit) < PUBLISH_RATE_WINDOW);
        if hits.len() >= PUBLISH_RATE_LIMIT {
            let retry_after = hits
                .first()
                .and_then(|oldest| oldest.checked_add(PUBLISH_RATE_WINDOW))
                .and_then(|reset| reset.checked_duration_since(now))
                .map(|wait| wait.as_secs() + 1)
                .unwrap_or(1)
                .max(1);
            return Some(retry_after);
        }
        hits.push(now);
        None
    }
}

/// Accepted plays from playback, as plugin events: a `scrobble` event
/// (which also reaches `scrobbler` plugins) and a `playback_started`
/// event, from the one point where v2 dispatched both.
pub struct PluginPlayEvents(pub Arc<PluginHost>);

impl crate::playback::ports::PlayEvents for PluginPlayEvents {
    fn play_accepted(
        &self,
        user_id: &str,
        report: &crate::playback::ports::ReportTrack,
        played_at: i64,
    ) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let causation = uuid::Uuid::new_v4().simple().to_string();
        self.0.dispatch_event(PluginEvent {
            kind: EventKind::Scrobble,
            payload: EventPayload::Scrobble(ScrobbleEvent {
                artist: report.artist_name.clone(),
                track: report.track_name.clone(),
                album: report.album_name.clone(),
                timestamp: played_at,
                duration_ms: Some(report.duration_ms).filter(|duration| *duration > 0),
                recording_mbid: report.mbid.clone(),
            }),
            causation_id: causation.clone(),
        });
        self.0.dispatch_event(PluginEvent {
            kind: EventKind::PlaybackStarted,
            payload: EventPayload::Playback(super::super::runtime::PlaybackEvent {
                artist: report.artist_name.clone(),
                track: report.track_name.clone(),
                album: report.album_name.clone(),
                user_id: user_id.to_owned(),
            }),
            causation_id: format!("{causation}-playback"),
        });
    }
}

/// Validate one publish payload into its stored field map: no unknown
/// fields, references non-empty, `note` and `body` default to empty.
fn validate_payload(
    kind: &str,
    payload: &PublishPayload,
) -> Result<HashMap<String, String>, String> {
    let Some(allowed) = publish_fields(kind) else {
        return Err(format!("unknown publish kind '{kind}'"));
    };
    let data = match payload {
        PublishPayload::Empty => HashMap::new(),
        PublishPayload::Fields(fields) => fields.clone(),
    };
    let mut unknown: Vec<&str> = data
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(String::as_str)
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown fields for kind '{kind}': [{}]",
            unknown.join(", ")
        ));
    }
    let mut stored = HashMap::new();
    for field in allowed {
        let value = data.get(*field).cloned().unwrap_or_default();
        let optional = matches!(*field, "note" | "body");
        if !optional && value.trim().is_empty() {
            return Err(format!("missing fields for kind '{kind}': ['{field}']"));
        }
        stored.insert((*field).to_owned(), value);
    }
    if kind == "download_note"
        && stored
            .get("note")
            .is_some_and(|note| note.len() > NOTE_MAX_BYTES)
    {
        return Err("note exceeds 1 KiB".to_owned());
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> PublishPayload {
        PublishPayload::Fields(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn payloads_refuse_unknown_and_missing_fields() {
        assert!(validate_payload("download_note", &fields(&[("task_id", "t")])).is_ok());
        let unknown = validate_payload("plugin_notice", &fields(&[("title", "t"), ("url", "x")]));
        assert_eq!(
            unknown.unwrap_err(),
            "unknown fields for kind 'plugin_notice': [url]"
        );
        assert!(validate_payload("indexer_invalidate", &PublishPayload::Empty).is_err());
        let long = "x".repeat(NOTE_MAX_BYTES + 1);
        assert!(
            validate_payload(
                "download_note",
                &fields(&[("task_id", "t"), ("note", &long)])
            )
            .is_err()
        );
    }
}
