//! What tests need from the library and production never uses: the
//! statement registry, scripted outside services (identification,
//! contributions, enrichment), a tag reader and a root resolver that read
//! nothing, and the knobs tests turn on publishing, scans and automatic
//! management.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

mod contrib;
mod enrichment;
mod identify;

pub use contrib::*;
pub use enrichment::{ScriptedAcoustId, ScriptedEnrichment};
pub use identify::FakeProviders;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::clock::LibraryClock as _;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::statement::StatementFile;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::jobs::queue::{JobQueue, Outcome};
use droppedneedle_kernel::runtime_config::ConfigStore;

use crate::contrib::error::ContribError;
use crate::contrib::worker::{Claim, Completion, SPEC, Verdict};
use crate::management::automatic::Settle;
use crate::management::models::ManagementError;
use crate::publish::place::{PlacementMode, Seams};
use crate::publish::publisher::{BeforePlace, CrashPoint};
use crate::publish::recovery::{BundleRecovery, reconcile_with};
use crate::publish::{PublishError, Sandbox};
use crate::removal::{MutationError, Removed};
use crate::scan::coordinator::ResolverSource;
use crate::scan::roots::{PolicyResolver, RootRegistry};
use crate::scan::seams::{ScannedTags, TagReadError, TagReader};
use crate::tags::TrackFile;

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::sql::registered(),
        crate::activity::sql::registered(),
        crate::corrections::sql::registered(),
        crate::identify::sql::registered(),
        crate::import::sql::registered(),
        crate::catalog_actions::sql::registered(),
        crate::edition::sql::registered(),
        crate::reidentify::sql::registered(),
        crate::reviews::sql::registered(),
        crate::reconcile::sql::registered(),
        crate::repairs::sql::registered(),
        crate::scan::sql::registered(),
        crate::scan::sqlite_store::sql::registered(),
        crate::identity::sql::registered(),
        crate::contrib::sql::registered(),
        crate::publish::sql::registered(),
        crate::catalog_revision::sql::registered(),
        crate::jobs::sql::registered(),
        crate::management::sql::registered(),
    ]
}

impl crate::settings::LibrarySettings {
    /// The store, for tests that save a section the library then reads.
    pub fn store(&self) -> &ConfigStore {
        &self.store
    }
}

impl crate::contrib::sqlite::SqliteContributions {
    pub(crate) fn db(&self) -> &Db {
        &self.db
    }
}

impl crate::contrib::worker::VerificationWorker {
    /// Claim the next due job and run it, as the runner would, for tests.
    /// The claim's lease ends at once, so a job a retry left running is
    /// taken back by the next call once the clock reaches its retry time:
    /// the runner's put-back is the jobs module's, tested there.
    pub async fn run_once(&self) -> Result<Option<Verdict>, ContribError> {
        let queue = self.store.queue().clone();
        let spec = SPEC.lease(Duration::from_millis(1));
        let claimed = {
            let queue = queue.clone();
            self.store
                .db()
                .write(Lane::Background, "contrib.test_claim", move |tx| {
                    queue.claim_in(tx, &spec)
                })
                .await?
        };
        let Some(claimed) = claimed else {
            return Ok(None);
        };
        let lease = claimed.lease.clone();
        let complete: Completion = {
            let queue = queue.clone();
            let lease = lease.clone();
            Arc::new(move |tx| queue.complete(tx, &lease))
        };
        let claim = Claim {
            contribution_id: claimed.key,
            attempt: claimed.attempt,
            complete: complete.clone(),
        };
        let verdict = self.run_claimed(&claim, self.service.now()).await?;
        if verdict.job == Outcome::Done {
            self.store
                .db()
                .write(Lane::Background, "contrib.test_done", move |tx| {
                    complete(tx).map_err(ContribError::from)
                })
                .await?;
        }
        Ok(Some(verdict))
    }
}

impl crate::removal::Mutations {
    /// [`Self::remove_extras`] without its edition check, for a test that
    /// works on files no edition plans. Tests only.
    pub fn recycle_album_files(
        &self,
        album_id: &str,
        track_ids: &[String],
        actor: &str,
    ) -> Result<Removed, MutationError> {
        self.recycle_extras(album_id, track_ids, actor)
    }
}

impl crate::management::service::Management {
    /// [`Self::automatic_step`] at `now` (unix seconds), so a test can
    /// move time. Blocking.
    pub fn automatic_step_at(
        &self,
        settle: &mut Settle,
        now: f64,
    ) -> Result<bool, ManagementError> {
        self.step_at(settle, now)
    }

    /// Look at the next waiting album that is due, settled or not, so a
    /// test need not wait out the settle window. Blocking.
    pub fn automatic_next(&self) -> Result<bool, ManagementError> {
        self.look_at_due(self.parts.clock.now_secs(), None)
    }
}

impl crate::publish::paths::Sandbox {
    /// Fix a root's placement mode without probing. Tests only.
    pub fn set_placement_mode(&self, root_id: &str, mode: PlacementMode) {
        self.store_placement_mode(false, root_id, mode);
    }

    /// Fix a bin's placement mode without probing. Tests only.
    pub fn set_bin_placement_mode(&self, bin_id: &str, mode: PlacementMode) {
        self.store_placement_mode(true, bin_id, mode);
    }
}

impl<P: crate::publish::SpaceProbe> crate::publish::publisher::Publisher<P> {
    /// Arm one crash-injection point. Tests only; production never calls this.
    pub fn set_crash_point(&mut self, point: Option<CrashPoint>) {
        self.injector = point;
    }

    /// Make placement behave like a filesystem without one of its steps.
    /// Tests only; production never calls this.
    pub fn set_place_seams(&mut self, seams: Seams) {
        self.place_seams = seams;
    }

    /// Make the move of a replaced file into its recycle bin behave like a
    /// filesystem without one of its steps. Tests only; production never
    /// calls this.
    pub fn set_bin_seams(&mut self, seams: Seams) {
        self.bin_seams = seams;
    }

    /// Run `hook` between the last destination check and the placement.
    /// Tests only; production never calls this.
    pub fn set_before_place(&mut self, hook: Option<BeforePlace>) {
        self.before_place = hook;
    }

    /// Run `hook` with the destination while the file there is being set
    /// aside, just before it moves. Tests only; production never calls
    /// this.
    pub fn set_before_set_aside(&mut self, hook: Option<BeforePlace>) {
        self.before_set_aside = hook;
    }
}

impl crate::publish::session::RootPlacement {
    /// Fix a root's mode without probing. Tests only.
    pub fn set_mode(&self, root_id: &str, mode: PlacementMode) {
        let open = self
            .open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sandbox) = open.as_ref() {
            sandbox.set_placement_mode(root_id, mode);
        }
    }
}

impl crate::publish::session::Publishing {
    /// Arm (or clear) a crash point on the open publisher, so a test can
    /// stop a publish part way as a killed process would. Tests only.
    pub fn arm_publish_crash(&self, point: Option<crate::publish::CrashPoint>) {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.core.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_crash_point(point);
        }
    }

    /// Make placement on the open publisher behave like a filesystem
    /// without one of its steps. Tests only.
    pub fn set_publish_seams(&self, seams: crate::publish::place::Seams) {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.core.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_place_seams(seams);
        }
    }

    /// Run `before_place` on the open publisher just before each file is
    /// placed, and `before_set_aside` just before each file is set aside,
    /// so a test can put something in the way. Tests only.
    pub fn set_publish_hooks(
        &self,
        before_place: Option<crate::publish::publisher::BeforePlace>,
        before_set_aside: Option<crate::publish::publisher::BeforePlace>,
    ) {
        let registry = self.core.live_registry();
        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cell.refresh(&registry, &self.core.root_dirs).is_ok()
            && let Ok(open) = cell.open()
        {
            open.publisher.set_before_place(before_place);
            open.publisher.set_before_set_aside(before_set_aside);
        }
    }
}

/// [`reconcile`] on a filesystem made to lack a placement step. Tests
/// only; production never calls this.
pub fn reconcile_with_seams(
    db: &Db,
    sandbox: &Sandbox,
    jobs: &JobQueue,
    clock: &dyn Clock,
    seams: Seams,
) -> Result<Vec<BundleRecovery>, PublishError> {
    reconcile_with(db, sandbox, jobs, clock, seams)
}

/// Fixed resolver for tests and single-config runtimes.
pub struct StaticResolver {
    resolver: PolicyResolver,
}

impl StaticResolver {
    pub fn new(registry: RootRegistry) -> Self {
        Self {
            resolver: PolicyResolver::new(registry),
        }
    }
}

impl ResolverSource for StaticResolver {
    fn resolver(&self) -> PolicyResolver {
        self.resolver.clone()
    }
}

/// Null tag reader: every file reads clean with empty tags, so tests can
/// measure discovery and indexing without a tag stack.
#[derive(Debug, Default)]
pub struct NullTagReader {
    reads: Mutex<u64>,
}

impl NullTagReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reads(&self) -> u64 {
        *self
            .reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl TagReader for NullTagReader {
    fn read_tags(&self, _file: TrackFile<'_>) -> Result<ScannedTags, TagReadError> {
        *self
            .reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        Ok(ScannedTags::default())
    }
}
