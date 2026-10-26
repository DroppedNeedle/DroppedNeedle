//! Settings-save side effects.
//!
//! Each section declares on its type what a change to it sets off
//! ([`Section::EFFECTS`](droppedneedle_kernel::runtime_config::Section::EFFECTS)):
//! the events sweep to kick, the download worker to wake. No save clears a
//! cache: a cached provider answer's key carries every value that changes
//! it. The [`EffectsRunner`] runs them on
//! the section's change notice, so every committed save runs them, whoever
//! made it. A settings route marks the section before its save and
//! waits in [`Effects::saved`] before it answers, so the next read after a
//! save sees what the save set off. A route save that left the row as it was
//! runs the section's effects itself, as an explicit save always has.

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle_kernel::jobs::CancellationToken;
use futures_util::future::{BoxFuture, select_all};
use tokio::sync::watch;

use droppedneedle_kernel::jobs::events_kick::EventsKick;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::effects::Effect;
use droppedneedle_kernel::runtime_config::registry;

/// Carries out one effect. Production wires the events sweep and the
/// download worker; tests record.
pub trait SaveEffects: Send + Sync {
    /// Carry out `effect`. It cannot fail: kicks collapse, a wake only
    /// signals.
    fn run(&self, effect: Effect) -> BoxFuture<'_, ()>;
}

/// Asks the download worker for a pass now. It only signals: it never
/// waits for the pass and cannot fail.
pub type DownloadWake = Arc<dyn Fn() + Send + Sync>;

/// Takes the saved library roots in and asks for the scans new or changed
/// roots need. Blocking; it logs its own failures.
pub type LibraryRootsSaved = Arc<dyn Fn() + Send + Sync>;

/// Production effects: the events kick, the download worker's wake and the
/// library's roots refresh.
pub struct LiveSaveEffects {
    /// Events sweep kick.
    pub kick: Arc<dyn EventsKick>,
    /// Download worker wake, where a worker runs.
    pub download_wake: Option<DownloadWake>,
    /// The library's roots refresh, where a library runs.
    pub roots_saved: Option<LibraryRootsSaved>,
}

impl LiveSaveEffects {
    /// Build over a kick.
    pub fn new(kick: Arc<dyn EventsKick>) -> Self {
        Self {
            kick,
            download_wake: None,
            roots_saved: None,
        }
    }

    /// Wake the download worker when a download source is saved.
    #[must_use]
    pub fn with_download_wake(mut self, wake: DownloadWake) -> Self {
        self.download_wake = Some(wake);
        self
    }

    /// Refresh the library's roots when the library settings are saved.
    #[must_use]
    pub fn with_library_roots(mut self, roots_saved: LibraryRootsSaved) -> Self {
        self.roots_saved = Some(roots_saved);
        self
    }
}

impl SaveEffects for LiveSaveEffects {
    fn run(&self, effect: Effect) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            match effect {
                Effect::KickEventsSweep => self.kick.kick(),
                Effect::WakeDownloads => {
                    if let Some(wake) = &self.download_wake {
                        wake();
                    }
                }
                Effect::RefreshLibraryRoots => {
                    if let Some(roots_saved) = self.roots_saved.clone()
                        && let Err(error) = tokio::task::spawn_blocking(move || roots_saved()).await
                    {
                        tracing::error!(%error, "the library roots refresh did not finish");
                    }
                }
            }
        })
    }
}

/// The revision of each section whose effects have run.
type Applied = HashMap<&'static str, i64>;

/// Says when a section's effects have run. Cheap to clone.
#[derive(Clone)]
pub struct Effects {
    store: Arc<ConfigStore>,
    sink: Arc<dyn SaveEffects>,
    declared: Arc<HashMap<&'static str, &'static [Effect]>>,
    applied: watch::Receiver<Applied>,
}

/// A section's revision just before a route saved it.
#[derive(Debug, Clone, Copy)]
pub struct SaveMark {
    key: &'static str,
    revision: i64,
}

impl Effects {
    /// Watch every section that declares effects. The runner must be
    /// started for them to run; while none runs, [`Effects::settled`]
    /// returns at once.
    pub fn new(store: Arc<ConfigStore>, sink: Arc<dyn SaveEffects>) -> (Self, EffectsRunner) {
        let mut watched = Vec::new();
        let mut applied = Applied::new();
        let declared = Arc::new(registry::effects().into_iter().collect());
        for (key, effects) in registry::effects() {
            let notice = store.subscribe(key);
            applied.insert(key, *notice.borrow());
            watched.push(Watched {
                key,
                effects,
                notice,
            });
        }
        let (sender, receiver) = watch::channel(applied);
        (
            Self {
                store,
                sink: sink.clone(),
                declared,
                applied: receiver,
            },
            EffectsRunner {
                sink,
                watched,
                applied: sender,
            },
        )
    }

    /// Mark the section under `key` before a route saves it.
    #[must_use]
    pub fn mark(&self, key: &'static str) -> SaveMark {
        SaveMark {
            key,
            revision: self.store.revision(key),
        }
    }

    /// After a route's save: when the row moved, wait for its effects to
    /// run; when the save left it as it was, run them here, since no notice
    /// fires for an unchanged row.
    pub async fn saved(&self, mark: SaveMark) {
        if self.store.revision(mark.key) != mark.revision {
            self.settled(mark.key).await;
            return;
        }
        let effects = self.declared.get(mark.key).copied().unwrap_or_default();
        for effect in effects {
            self.sink.run(*effect).await;
        }
    }

    /// Wait until the effects of every committed change to the section
    /// under `key` have run. Returns at once for a section without
    /// effects, or when the runner has stopped.
    async fn settled(&self, key: &str) {
        if !self.applied.borrow().contains_key(key) {
            return;
        }
        let target = self.store.revision(key);
        let mut applied = self.applied.clone();
        let _ = applied
            .wait_for(|applied| applied.get(key).is_none_or(|revision| *revision >= target))
            .await;
    }
}

/// One section the runner follows.
struct Watched {
    key: &'static str,
    effects: &'static [Effect],
    notice: watch::Receiver<i64>,
}

/// Runs each section's effects after each committed change to it.
pub struct EffectsRunner {
    sink: Arc<dyn SaveEffects>,
    watched: Vec<Watched>,
    applied: watch::Sender<Applied>,
}

impl EffectsRunner {
    /// Run until `stop` is cancelled or the store is gone.
    pub async fn run(mut self, stop: CancellationToken) {
        if self.watched.is_empty() {
            return;
        }
        loop {
            self.catch_up().await;
            let changed = select_all(
                self.watched
                    .iter_mut()
                    .map(|watched| Box::pin(watched.notice.changed())),
            );
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                (changed, _, _) = changed => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Run the effects of every section that changed since its effects
    /// last ran.
    async fn catch_up(&mut self) {
        for watched in &self.watched {
            let revision = *watched.notice.borrow();
            let done = self
                .applied
                .borrow()
                .get(watched.key)
                .is_some_and(|applied| *applied >= revision);
            if done {
                continue;
            }
            for effect in watched.effects {
                self.sink.run(*effect).await;
            }
            self.applied.send_modify(|applied| {
                applied.insert(watched.key, revision);
            });
        }
    }
}
