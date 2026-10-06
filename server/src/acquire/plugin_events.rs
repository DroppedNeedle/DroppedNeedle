//! Acquisition events for `subscriber` plugins and the live event stream.
//!
//! The download worker reports `download_started`, `download_completed`
//! and `download_failed`; request intake reports `request_created`; the
//! flow ticks report `request_fulfilled` and `import_finished`. Each goes
//! to the plugin host when boot attached one, in the background: a slow
//! plugin never delays the flow that raised the event. Flow notices for a
//! user (`wanted_*`, `auto_download_enqueued`, `request_imported`) go to
//! the event hub once one is attached.

use super::flows::seams::{FlowEvent, TickSink};
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

/// Flow ticks: logged as before, the outcomes plugins care about go to
/// them, and user notices go to the live event stream.
pub struct FlowTicks {
    plugins: PluginSlot,
    events: crate::events::EventSink,
}

impl FlowTicks {
    /// Tick sink over the plugin slot and the event hub handle.
    pub fn new(plugins: PluginSlot, events: crate::events::EventSink) -> Self {
        Self { plugins, events }
    }
}

impl TickSink for FlowTicks {
    fn emit(&self, kind: &str, detail: &str, at: i64) {
        tracing::debug!(kind, detail, at, "flow tick");
    }

    fn announce(&self, event: FlowEvent) {
        match event {
            FlowEvent::Notify { user_id, notice } => self.events.notify(&user_id, notice),
            FlowEvent::RequestFulfilled {
                request_id,
                user_id,
                release_group_mbid,
            } => announce(
                &self.plugins,
                EventKind::RequestFulfilled,
                EventPayload::Request(RequestEvent {
                    request_id,
                    user_id,
                    release_group_mbid,
                    status: "imported".to_owned(),
                }),
            ),
            FlowEvent::ImportFinished {
                release_group_mbid,
                track_count,
                source,
            } => announce(
                &self.plugins,
                EventKind::ImportFinished,
                EventPayload::Import(ImportEvent {
                    release_group_mbid,
                    track_count,
                    source,
                }),
            ),
        }
    }
}
