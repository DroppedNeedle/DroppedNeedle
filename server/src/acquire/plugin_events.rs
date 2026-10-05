//! Acquisition events for `subscriber` plugins.
//!
//! The download worker reports `download_started`, `download_completed`
//! and `download_failed`; request intake reports `request_created`; the
//! flow ticks report `request_fulfilled` and `import_finished`. Each goes
//! to the plugin host when boot attached one, in the background: a slow
//! plugin never delays the flow that raised the event.

use super::flows::seams::TickSink;
use super::wiring::PluginSlot;
use crate::plugins::runtime::{
    DownloadTaskEvent, EventKind, EventPayload, ImportEvent, PluginEvent, RequestEvent,
};

/// Send one event when a plugin host is attached.
pub fn announce(plugins: &PluginSlot, kind: EventKind, payload: EventPayload) {
    if let Some(host) = plugins.get() {
        host.dispatch_event(PluginEvent {
            kind,
            payload,
            causation_id: String::new(),
        });
    }
}

/// A download lifecycle event.
pub fn download_event(
    plugins: &PluginSlot,
    kind: EventKind,
    task: &super::downloads::store::TaskRow,
    source: &str,
    outcome: &str,
) {
    announce(
        plugins,
        kind,
        EventPayload::Download(DownloadTaskEvent {
            task_id: task.id.clone(),
            user_id: task.user_id.clone(),
            release_group_mbid: task.release_group_mbid.clone(),
            source: source.to_owned(),
            outcome: outcome.to_owned(),
        }),
    );
}

/// Flow ticks: logged as before, and the ones plugins care about are also
/// sent to them.
pub struct PluginTicks {
    plugins: PluginSlot,
}

impl PluginTicks {
    /// Tick sink over the plugin slot.
    pub fn new(plugins: PluginSlot) -> Self {
        Self { plugins }
    }
}

impl TickSink for PluginTicks {
    fn emit(&self, kind: &str, detail: &str, at: i64) {
        tracing::debug!(kind, detail, at, "flow tick");
    }

    fn emit_about(&self, kind: &str, release_group_mbid: &str, detail: &str, at: i64) {
        self.emit(kind, detail, at);
        match kind {
            "request_fulfilled" => announce(
                &self.plugins,
                EventKind::RequestFulfilled,
                EventPayload::Request(RequestEvent {
                    request_id: release_group_mbid.to_owned(),
                    user_id: String::new(),
                    release_group_mbid: release_group_mbid.to_owned(),
                    status: "imported".to_owned(),
                }),
            ),
            "drop_import.resolved" => announce(
                &self.plugins,
                EventKind::ImportFinished,
                EventPayload::Import(ImportEvent {
                    release_group_mbid: release_group_mbid.to_owned(),
                    track_count: 1,
                    source: "drop_import".to_owned(),
                }),
            ),
            _ => {}
        }
    }
}
