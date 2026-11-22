//! Live acquisition-health probes over config snapshots plus cached
//! verdicts.
//!
//! The imports health traits are sync, but reachability needs HTTP. The
//! split: [`LiveProbes`] implements both traits over a [`ProbeCache`]
//! snapshot, and [`refresh_probes`] recomputes the snapshot with real
//! client health checks. Wiring runs one refresh at boot, again after a
//! client's settings are saved, and on a slow loop, so the
//! status routes always render a fresh verdict without blocking a request
//! on five network calls. A verdict is kept with the client set it probed:
//! until a refresh lands for the current one, a read answers from the
//! saved settings alone.
//!
//! The Deemix check ([`DeemixChecks`]) serves both the probe and the
//! settings page's Test, on the one session DroppedNeedle keeps with
//! Deemix. Only the probe signs that session in.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::clients::LiveClients;
use super::deemix::client::{Bitrate, DeemixClient, DeemixError, parse_base};
use super::deemix::local_path;
use super::deemix::models::{Connect, DeezerAvailable, ItemStatus, Overwrite, Queue};
use super::deemix::pick::{AccountFlags, AddFor, choose_bitrate};
use super::deemix::session::{DeemixSession, NotNow, SignIn, sign_in_if_needed};
use super::folder_overlap::folder_clear;
use super::imports::health::{
    ClientProbe, DeemixProbe, DeemixQuality, DeemixStatusResponse, DeemixWarning, HealthProbes,
    SlskdProbe, UsenetProbe,
};
use super::imports::mount::{self, SlskdMountReport};
use super::landing::confine::{FolderFault, open_beneath};
use super::slskd::{ReqwestSlskdHttp, SlskdRepository};
use super::usenet::nzbget::NzbgetQueue;
use super::usenet::policy::UsenetPolicy;
use super::usenet::queue::UsenetQueue;
use super::usenet::sabnzbd::SabnzbdQueue;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::DownloadClients;
use droppedneedle_kernel::runtime_config::sections::{DeemixConnection, DownloadPolicy};

/// One cached verdict per probe.
#[derive(Debug, Clone, Default)]
pub struct CachedProbes {
    /// slskd client verdict.
    pub slskd: ClientProbe,
    /// slskd downloads mount report.
    pub slskd_mount: Option<SlskdMountReport>,
    /// SABnzbd verdict.
    pub sabnzbd: UsenetVerdict,
    /// NZBGet verdict.
    pub nzbget: UsenetVerdict,
    /// Deemix verdict.
    pub deemix: DeemixStatusResponse,
}

/// One Usenet client's cached verdict.
#[derive(Debug, Clone, Default)]
pub struct UsenetVerdict {
    /// Client verdict.
    pub status: ClientProbe,
    /// Categories, when the probe reached the client.
    pub categories: Vec<String>,
    /// Complete dir, when the probe reached it.
    pub complete_dir: Option<String>,
}

/// The last refresh and the client set it probed.
struct Refreshed {
    inputs: Arc<ProbeInputs>,
    probes: CachedProbes,
}

/// Shared verdict snapshot behind the live probes.
pub struct ProbeCache {
    /// The last refresh, `None` until one lands.
    inner: Mutex<Option<Refreshed>>,
    /// The clients the saved settings build, which a read before the
    /// first refresh answers from.
    clients: Arc<LiveClients>,
}

impl ProbeCache {
    /// Cache over the live clients, with no refresh yet.
    pub fn new(clients: Arc<LiveClients>) -> Self {
        Self {
            inner: Mutex::new(None),
            clients,
        }
    }

    /// Current snapshot: the last refresh of the clients the saved
    /// settings build now, or until one lands, the enabled and configured
    /// flags of those settings. A save is seen at once.
    pub fn snapshot(&self) -> CachedProbes {
        let current = self.clients.current().probe_inputs.clone();
        let guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.as_ref() {
            Some(refreshed) if Arc::ptr_eq(&refreshed.inputs, &current) => refreshed.probes.clone(),
            _ => seed(&current),
        }
    }

    /// Replace the snapshot after a refresh pass over `inputs`.
    fn update(&self, inputs: &Arc<ProbeInputs>, probes: CachedProbes) {
        let next = Some(Refreshed {
            inputs: inputs.clone(),
            probes,
        });
        match self.inner.lock() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }
}

/// All five health traits over one shared cache.
#[derive(Clone)]
pub struct LiveProbes {
    cache: Arc<ProbeCache>,
}

impl LiveProbes {
    /// Probes over a shared cache.
    pub fn new(cache: Arc<ProbeCache>) -> Self {
        Self { cache }
    }

    /// Bundle for the imports deps. One struct serves both traits.
    pub fn bundle(probes: &Arc<LiveProbes>) -> HealthProbes {
        HealthProbes {
            slskd: probes.clone(),
            sabnzbd: Arc::new(LiveUsenetProbe {
                cache: probes.cache.clone(),
                read: |probes| probes.sabnzbd,
            }),
            nzbget: Arc::new(LiveUsenetProbe {
                cache: probes.cache.clone(),
                read: |probes| probes.nzbget,
            }),
            deemix: probes.clone(),
        }
    }
}

impl DeemixProbe for LiveProbes {
    fn status(&self) -> DeemixStatusResponse {
        self.cache.snapshot().deemix
    }
}

impl SlskdProbe for LiveProbes {
    fn status(&self) -> ClientProbe {
        self.cache.snapshot().slskd
    }

    fn mount(&self) -> Option<SlskdMountReport> {
        self.cache.snapshot().slskd_mount
    }
}

/// One Usenet client's probe over the shared cache.
struct LiveUsenetProbe {
    cache: Arc<ProbeCache>,
    read: fn(CachedProbes) -> UsenetVerdict,
}

impl UsenetProbe for LiveUsenetProbe {
    fn status(&self) -> ClientProbe {
        (self.read)(self.cache.snapshot()).status
    }

    fn categories(&self) -> Vec<String> {
        (self.read)(self.cache.snapshot()).categories
    }

    fn complete_dir(&self) -> Option<String> {
        (self.read)(self.cache.snapshot()).complete_dir
    }
}

/// Everything one refresh pass needs: live clients plus config snapshots.
pub struct ProbeInputs {
    /// slskd repository, when the section configures one.
    pub slskd: Option<Arc<SlskdRepository<ReqwestSlskdHttp>>>,
    /// slskd master switch.
    pub slskd_enabled: bool,
    /// `SLSKD_DOWNLOADS_PATH`: the downloads mount.
    pub slskd_mount_base: PathBuf,
    /// The downloads subfolder inside the mount, from settings.
    pub slskd_subpath: String,
    /// Library root folders, to tell whether imports can rename.
    pub library_roots: Vec<PathBuf>,
    /// SABnzbd queue, when the section configures one.
    pub sabnzbd: Option<Arc<SabnzbdQueue>>,
    /// NZBGet queue, when the section configures one.
    pub nzbget: Option<Arc<NzbgetQueue>>,
    /// Usenet clients section, for the switches and mount-independent
    /// fields.
    pub sabnzbd_section: DownloadClients,
    /// The Deemix check and the section it checks.
    pub deemix: DeemixInputs,
}

/// The Deemix check and the saved section a probe pass checks.
pub struct DeemixInputs {
    /// The check, on the session DroppedNeedle keeps with Deemix.
    pub checks: Arc<DeemixChecks>,
    /// The saved Deemix section.
    pub section: DeemixConnection,
}

/// Recompute every verdict with live health checks and store the snapshot.
/// One erroring client never fails the pass: its verdict goes red while
/// the others still refresh.
pub async fn refresh_probes(cache: &ProbeCache, inputs: &Arc<ProbeInputs>) {
    let probes = CachedProbes {
        slskd: refresh_slskd(inputs).await,
        slskd_mount: Some(refresh_slskd_mount(inputs).await),
        sabnzbd: match inputs.sabnzbd.clone() {
            Some(queue) => {
                let enabled = inputs.sabnzbd_section.sabnzbd.enabled;
                refresh_usenet(enabled, UsenetQueue::Sabnzbd(queue)).await
            }
            None => unconfigured_usenet(inputs.sabnzbd_section.sabnzbd.enabled),
        },
        nzbget: match inputs.nzbget.clone() {
            Some(queue) => {
                let enabled = inputs.sabnzbd_section.nzbget.enabled;
                refresh_usenet(enabled, UsenetQueue::Nzbget(queue)).await
            }
            None => unconfigured_usenet(inputs.sabnzbd_section.nzbget.enabled),
        },
        deemix: inputs.deemix.checks.probe(&inputs.deemix.section).await,
    };
    cache.update(inputs, probes);
}

/// Verdicts from the settings alone (no network): enabled and configured
/// read correctly while reachability waits for the first refresh pass.
fn seed(inputs: &ProbeInputs) -> CachedProbes {
    let slskd_configured = inputs.slskd.is_some();
    let sab = &inputs.sabnzbd_section.sabnzbd;
    let sab_configured = !sab.url.is_empty() && !sab.api_key.expose().is_empty();
    let nzbget = &inputs.sabnzbd_section.nzbget;
    CachedProbes {
        slskd: ClientProbe {
            enabled: inputs.slskd_enabled,
            configured: slskd_configured,
            reachable: false,
            version: None,
            message: if slskd_configured {
                "Probe pending".to_owned()
            } else {
                "Not configured".to_owned()
            },
        },
        sabnzbd: UsenetVerdict {
            status: pending(sab.enabled, sab_configured),
            ..UsenetVerdict::default()
        },
        nzbget: UsenetVerdict {
            status: pending(nzbget.enabled, !nzbget.url.is_empty()),
            ..UsenetVerdict::default()
        },
        deemix: {
            let section = &inputs.deemix.section;
            let status = pending(section.enabled, !section.url.is_empty());
            DeemixStatusResponse {
                enabled: status.enabled,
                configured: status.configured,
                message: status.message,
                ..DeemixStatusResponse::default()
            }
        },
        ..CachedProbes::default()
    }
}

async fn refresh_slskd(inputs: &ProbeInputs) -> ClientProbe {
    let configured = inputs
        .slskd
        .as_ref()
        .is_some_and(|repo| repo.is_configured());
    if !configured {
        return ClientProbe {
            enabled: inputs.slskd_enabled,
            configured: false,
            reachable: false,
            version: None,
            message: "Not configured".to_owned(),
        };
    }
    let Some(repo) = &inputs.slskd else {
        return ClientProbe::default();
    };
    let verdict = repo.health_check().await;
    ClientProbe {
        enabled: inputs.slskd_enabled,
        configured: true,
        reachable: verdict.ok,
        version: verdict.version,
        message: verdict.message,
    }
}

/// Shown when the downloads subfolder is a link: nothing is read through
/// it, so the subfolder hint the other advisories give would mislead.
const SUBFOLDER_LINK_ADVISORY: &str = "The downloads subfolder is a link, and DroppedNeedle \
     never reads through one. Point the mount at slskd's folder and clear the subfolder, or \
     make the subfolder a real folder.";

/// Check the downloads mount and, when it looks usable and slskd is set
/// up, whether slskd's finished downloads can be found in it.
async fn refresh_slskd_mount(inputs: &ProbeInputs) -> SlskdMountReport {
    let base = inputs.slskd_mount_base.clone();
    let roots = inputs.library_roots.clone();
    let check = match tokio::task::spawn_blocking(move || {
        mount::check_downloads_mount(&base, &roots)
    })
    .await
    {
        Ok(check) => check,
        Err(error) => {
            tracing::warn!(%error, "downloads mount check did not finish; reporting it unchecked");
            mount::MountCheck {
                ok: false,
                move_supported: false,
                reason: mount::MountReason::StatError,
                path: inputs.slskd_mount_base.display().to_string(),
            }
        }
    };
    let effective = mount::effective_path(&inputs.slskd_mount_base, &inputs.slskd_subpath)
        .display()
        .to_string();
    let (advisory, client_downloads_dir) = match &inputs.slskd {
        Some(repo) if repo.is_configured() && check.ok => {
            let folder = repo.downloads_folder().clone();
            let linked = match tokio::task::spawn_blocking(move || {
                matches!(folder.resolve(), Err(FolderFault::Link))
            })
            .await
            {
                Ok(linked) => linked,
                Err(error) => {
                    tracing::warn!(%error, "downloads subfolder check did not finish");
                    false
                }
            };
            let diagnosis = repo.diagnose_downloads_mount().await;
            let advisory = if linked {
                Some(SUBFOLDER_LINK_ADVISORY.to_owned())
            } else {
                mount::advisory(&check, &diagnosis, &inputs.slskd_subpath, &effective)
            };
            (advisory, diagnosis.client_downloads_dir)
        }
        _ => (None, None),
    };
    SlskdMountReport {
        mount: check,
        advisory,
        client_downloads_dir,
        effective_path: effective,
    }
}

/// A verdict from the settings alone, before the first probe.
fn pending(enabled: bool, configured: bool) -> ClientProbe {
    ClientProbe {
        enabled,
        configured,
        reachable: false,
        version: None,
        message: if configured {
            "Probe pending".to_owned()
        } else {
            "Not configured".to_owned()
        },
    }
}

/// The verdict for a Usenet client the section does not set up.
fn unconfigured_usenet(enabled: bool) -> UsenetVerdict {
    UsenetVerdict {
        status: pending(enabled, false),
        ..UsenetVerdict::default()
    }
}

async fn refresh_usenet(enabled: bool, queue: UsenetQueue) -> UsenetVerdict {
    if !queue.is_configured() {
        return unconfigured_usenet(enabled);
    }
    let verdict = queue.health_check().await;
    UsenetVerdict {
        status: ClientProbe {
            enabled,
            configured: true,
            reachable: verdict.status == "ok",
            version: verdict.version,
            message: verdict.message,
        },
        categories: queue.get_categories().await.unwrap_or_default(),
        complete_dir: queue.get_complete_dir().await.ok(),
    }
}

/// Health-probe refresh cadence.
const PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Job name of the probe refresh loop.
pub const PROBE_JOB: &str = "acquire-probe-refresh";

/// Probe refresh: one pass now, then one on every wake and at least
/// every five minutes, each over the clients the current settings
/// build. A pass waiting on a slow client ends at the stop; it only
/// reads, and the cache keeps the last pass.
pub(crate) fn spawn_probe_loop(
    supervisor: &Supervisor,
    cache: Arc<ProbeCache>,
    clients: Arc<LiveClients>,
    wake: &watch::Sender<u64>,
) -> Result<(), AlreadyRunning> {
    let mut wake = wake.subscribe();
    wake.mark_changed();
    supervisor.on_wakeup(
        PROBE_JOB,
        wake,
        Schedule::new(PROBE_INTERVAL),
        FnTick(move |ctx: TickCtx| {
            let cache = cache.clone();
            let inputs = clients.current().probe_inputs.clone();
            async move {
                tokio::select! {
                    () = ctx.token().cancelled() => {}
                    () = refresh_probes(&cache, &inputs) => {}
                }
                Ok(Next::Continue)
            }
        }),
    )
}

/// How long one Deemix call in a check may take.
const DEEMIX_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest version text passed on from Deemix.
const MAX_VERSION_LEN: usize = 32;

/// The folders Deemix's downloads folder must stay clear of, read live:
/// the library folders, the recycle bin, the import folder, the app's own
/// folders and the other download folders. `None` while a setting they
/// come from cannot be read.
pub type DeemixFolders = Arc<dyn Fn() -> Option<Vec<PathBuf>> + Send + Sync>;

/// Checks a Deemix for the settings card: whether it answers, its Deezer
/// sign-in and the quality that gives, and what to change. Built once over
/// the session DroppedNeedle keeps with Deemix, which the probe and Test
/// share.
pub struct DeemixChecks {
    http: reqwest::Client,
    session: Arc<DeemixSession>,
    clock: Arc<dyn Clock>,
    policy: Live<DownloadPolicy>,
    folders: DeemixFolders,
    /// The Deemix settings, read after a save for the address they name.
    section: Live<DeemixConnection>,
    /// Saves of the Deemix settings not acted on yet.
    saves: Mutex<watch::Receiver<i64>>,
}

/// Which session a check reads on, and whether it may sign it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckOn {
    /// The kept session; signs it in when it is signed out and Deemix
    /// offers its saved sign-in, within the sign-in limits.
    KeptSigningIn,
    /// The kept session, read only.
    Kept,
    /// A new session for an address not saved yet: reads `/connect` only.
    Fresh,
}

/// How a sign-in the probe tried went, when it did not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tried {
    Waiting,
    DidNotTake,
    Lost,
}

/// One `/connect` read, the saved sign-in it offered taken out.
struct Reading {
    connect: Connect,
    arl_offered: bool,
    tried: Option<Tried>,
}

impl DeemixChecks {
    /// Checks over `http` (the factory's client that follows no redirect)
    /// on `session`, the quality settings in `policy`, and the folders
    /// Deemix's downloads folder stays clear of. `section` is the Deemix
    /// settings: a save that changes their address drops the session.
    pub fn new(
        http: reqwest::Client,
        session: Arc<DeemixSession>,
        clock: Arc<dyn Clock>,
        policy: Live<DownloadPolicy>,
        folders: DeemixFolders,
        section: Live<DeemixConnection>,
    ) -> Self {
        let saves = section.subscribe();
        Self {
            http,
            session,
            clock,
            policy,
            folders,
            section,
            saves: Mutex::new(saves),
        }
    }

    /// After a save of the Deemix settings, drop the kept session unless
    /// it belongs to the address they name now, so a new address gets a
    /// new session. A save of the folder or the switch alone keeps the
    /// signed-in session; settings that cannot be read keep none. The
    /// clients and every check look first.
    pub(crate) fn after_saves(&self) {
        let mut saves = self
            .saves
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if saves.has_changed().unwrap_or(false) {
            saves.mark_unchanged();
            let base = self
                .section
                .read()
                .and_then(|section| parse_base(&section.url).ok());
            self.session.keep_only_for(base.as_ref());
        }
    }

    /// The probe's check of the saved section. While the section is on, a
    /// signed-out session is signed in when Deemix offers its saved
    /// sign-in and the sign-in limits allow, so the card shows the
    /// account's quality before the first download.
    pub async fn probe(&self, section: &DeemixConnection) -> DeemixStatusResponse {
        let on = if section.enabled {
            CheckOn::KeptSigningIn
        } else {
            CheckOn::Kept
        };
        self.check(section, on).await
    }

    /// Test's check of the submitted values, which never signs in. When
    /// the address is the saved one it reads the kept session; any other
    /// address gets a new session that only reads `/connect`. `incoming`
    /// is normalized as a save would.
    pub async fn test(
        &self,
        incoming: &DeemixConnection,
        saved_url: Option<&str>,
    ) -> DeemixStatusResponse {
        let on = if saved_url == Some(incoming.url.as_str()) {
            CheckOn::Kept
        } else {
            CheckOn::Fresh
        };
        self.check(incoming, on).await
    }

    async fn check(&self, section: &DeemixConnection, on: CheckOn) -> DeemixStatusResponse {
        self.after_saves();
        let mut status = DeemixStatusResponse {
            enabled: section.enabled,
            configured: !section.url.is_empty(),
            ..DeemixStatusResponse::default()
        };
        if !status.configured {
            return status;
        }
        let (real_mount, mut warnings) = self.mount_state(&section.downloads_mount).await;
        let session = match on {
            CheckOn::Fresh => Arc::new(DeemixSession::new()),
            CheckOn::KeptSigningIn | CheckOn::Kept => self.session.clone(),
        };
        let client = match DeemixClient::new(
            self.http.clone(),
            &section.url,
            session,
            DEEMIX_CHECK_TIMEOUT,
        ) {
            Ok(client) => client,
            Err(error) => {
                status.message = failure_message(&error);
                status.warnings = warnings;
                return status;
            }
        };
        let reading = match self.read(&client, on).await {
            Ok(reading) => reading,
            Err(error) => {
                tracing::debug!(%error, "Deemix check found no answer");
                status.message = failure_message(&error);
                status.warnings = warnings;
                return status;
            }
        };
        let connect = &reading.connect;
        let lost = !reading.arl_offered && client.session().sign_in_lost(client.base_url());
        status.reachable = true;
        status.webui_version = version_text(&connect.update.webui_version);
        status.deemix_version = version_text(&connect.update.deemix_version);
        status.deezer_reachable = connect.deezer_available == DeezerAvailable::Yes;
        status.signed_in = connect.signed_in();
        if status.signed_in {
            status.account_quality = account_quality(connect);
            if let Some(best) = self.best_quality(connect) {
                status.best_quality = best;
                if best.is_none() && status.account_quality.is_some() {
                    warnings.push(DeemixWarning::QualityNotTaken);
                }
            }
        }
        let settings = connect.settings();
        if settings.overwrite_file != Overwrite::Yes {
            warnings.push(DeemixWarning::OverwriteOff);
        }
        if settings.fallback_bitrate || settings.fallback_search || settings.fallback_isrc {
            warnings.push(DeemixWarning::FallbackOn);
        }
        status.sign_in_offered = !status.signed_in && reading.arl_offered;
        if !status.signed_in && !connect.single_user_mode() {
            warnings.push(DeemixWarning::SingleUserOff);
        }
        if settings.execute_command {
            warnings.push(DeemixWarning::CommandSet);
        }
        if on != CheckOn::Fresh
            && let Some(mount) = real_mount
            && !self
                .mapping_holds(&client, &settings.download_location, mount)
                .await
        {
            warnings.push(DeemixWarning::MappingWrong);
        }
        warnings.sort_unstable();
        warnings.dedup();
        status.warnings = warnings;
        status.message = reading_message(&status, &reading, lost, on).to_owned();
        status
    }

    /// Read `/connect`, and on [`CheckOn::KeptSigningIn`] sign the session
    /// in when it is signed out and the saved sign-in is offered.
    async fn read(&self, client: &DeemixClient, on: CheckOn) -> Result<Reading, DeemixError> {
        let mut connect = client.connect().await?;
        let arl_offered = connect.take_arl().is_some();
        let wants_sign_in = on == CheckOn::KeptSigningIn
            && !connect.signed_in()
            && arl_offered
            && connect.deezer_available == DeezerAvailable::Yes;
        if !wants_sign_in {
            return Ok(Reading {
                connect,
                arl_offered,
                tried: None,
            });
        }
        let tried = match sign_in_if_needed(client, self.clock.as_ref()).await {
            Ok(SignIn::SignedIn(signed_in)) => {
                return Ok(Reading {
                    connect: *signed_in,
                    arl_offered: false,
                    tried: None,
                });
            }
            Ok(SignIn::Lost) => Some(Tried::Lost),
            Ok(SignIn::NotNow(NotNow::Waiting)) => Some(Tried::Waiting),
            Ok(SignIn::NotNow(NotNow::DidNotTake)) => Some(Tried::DidNotTake),
            Ok(SignIn::NotNow(NotNow::NoArlOffered | NotNow::DeezerUnavailable)) => None,
            Err(error) => {
                tracing::warn!(%error, "the Deemix sign-in did not finish");
                Some(Tried::DidNotTake)
            }
        };
        Ok(Reading {
            connect,
            arl_offered: tried != Some(Tried::Lost),
            tried,
        })
    }

    /// The best quality the account gives that the saved quality settings
    /// take, as an add for a request would choose it: `None` while those
    /// settings cannot be read, so nothing is known.
    fn best_quality(&self, connect: &Connect) -> Option<Option<DeemixQuality>> {
        let saved = self.policy.read()?;
        let policy = UsenetPolicy::from_saved(&saved);
        let account = AccountFlags {
            can_stream_lossless: connect.can_stream_lossless(),
            can_stream_hq: connect.can_stream_hq(),
        };
        Some(choose_bitrate(account, &policy, AddFor::default()).map(quality_of))
    }

    /// The downloads folder's real path when it is usable, and the warnings
    /// it raises.
    async fn mount_state(&self, mount: &str) -> (Option<PathBuf>, Vec<DeemixWarning>) {
        let mount = PathBuf::from(mount);
        let folders = self.folders.clone();
        match tokio::task::spawn_blocking(move || mount_state(&mount, &folders)).await {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(%error, "the Deemix downloads folder check did not finish");
                (None, vec![DeemixWarning::MountUnusable])
            }
        }
    }

    /// Whether the first file of a finished item in Deemix's queue maps
    /// into the downloads folder as landings map it. Wrong when its path
    /// lies outside Deemix's own folder, or the file is there but does not
    /// open beneath the downloads folder (a link, or a path leading out).
    /// A queue that cannot be read, no finished file, or a file that is
    /// gone proves nothing wrong: admins move their own Deemix downloads
    /// away, and an imported item stays listed for a few seconds.
    async fn mapping_holds(&self, client: &DeemixClient, location: &str, mount: PathBuf) -> bool {
        let queue = match client.queue().await {
            Ok(queue) => queue,
            Err(error) => {
                tracing::debug!(%error, "the Deemix queue did not read for the folder check");
                return true;
            }
        };
        let Some(reported) = first_finished_file(&queue) else {
            return true;
        };
        let Some(local) = local_path(&reported, location, &mount) else {
            return false;
        };
        tokio::task::spawn_blocking(move || {
            match open_beneath(std::slice::from_ref(&mount), &local) {
                Ok(_) => true,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            }
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "the Deemix file check did not finish");
            true
        })
    }
}

/// The first file of the first finished item of the shape DroppedNeedle
/// adds.
fn first_finished_file(queue: &Queue) -> Option<String> {
    queue
        .items()
        .filter(|item| {
            matches!(
                item.status,
                ItemStatus::Completed | ItemStatus::WithErrors | ItemStatus::Failed
            )
        })
        .find_map(|item| item.files.as_ref()?.first().map(|file| file.path.clone()))
}

/// The folder's real path when it is there and writable, and its warnings.
/// Blocking.
fn mount_state(mount: &Path, folders: &DeemixFolders) -> (Option<PathBuf>, Vec<DeemixWarning>) {
    let Ok(real) = std::fs::canonicalize(mount) else {
        return (None, vec![DeemixWarning::MountUnusable]);
    };
    if !real.is_dir() || !crate::library_ready::writable(&real) {
        return (None, vec![DeemixWarning::MountUnusable]);
    }
    let Some(others) = folders() else {
        tracing::debug!(
            "the folders the Deemix downloads folder stays clear of are unknown; not checked"
        );
        return (Some(real), Vec::new());
    };
    match folder_clear(&real, [((), others)]) {
        Ok(()) => (Some(real), Vec::new()),
        Err(((), _)) => (Some(real), vec![DeemixWarning::MountOverlaps]),
    }
}

/// The account's best quality by Deemix's flags. A flag Deemix leaves out
/// is not known, so the quality is not either.
fn account_quality(connect: &Connect) -> Option<DeemixQuality> {
    match (connect.can_stream_lossless(), connect.can_stream_hq()) {
        (Some(true), _) => Some(DeemixQuality::Flac),
        (_, Some(true)) => Some(DeemixQuality::Mp3At320),
        (_, Some(false)) => Some(DeemixQuality::Mp3At128),
        _ => None,
    }
}

fn quality_of(bitrate: Bitrate) -> DeemixQuality {
    match bitrate {
        Bitrate::Flac => DeemixQuality::Flac,
        Bitrate::Mp3At320 => DeemixQuality::Mp3At320,
        Bitrate::Mp3At128 => DeemixQuality::Mp3At128,
    }
}

/// A version Deemix reported, when it reads as one.
fn version_text(raw: &str) -> Option<String> {
    let text = raw.trim();
    let plain = text
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    (!text.is_empty() && text.len() <= MAX_VERSION_LEN && plain).then(|| text.to_owned())
}

/// The summary for a check Deemix answered.
fn reading_message(
    status: &DeemixStatusResponse,
    reading: &Reading,
    lost: bool,
    on: CheckOn,
) -> &'static str {
    if !status.deezer_reachable {
        "Deemix can't reach Deezer: restart it."
    } else if on == CheckOn::Fresh {
        "Save to see your Deezer account."
    } else if status.signed_in {
        "Connected"
    } else if lost || reading.tried == Some(Tried::Lost) {
        "Deemix lost its Deezer sign-in. Sign in on Deemix's page."
    } else if !reading.arl_offered {
        "Not signed in to Deezer. Sign in on Deemix's page."
    } else {
        match reading.tried {
            Some(Tried::Waiting) => "Waiting to sign in to Deezer.",
            Some(Tried::DidNotTake | Tried::Lost) => {
                "Deemix didn't sign in to Deezer. Sign in on Deemix's page."
            }
            None => "Deemix has a saved Deezer sign-in; DroppedNeedle signs in when it needs to.",
        }
    }
}

/// The summary for a check Deemix did not answer.
fn failure_message(error: &DeemixError) -> String {
    match error {
        DeemixError::BadUrl => "The Deemix URL is not a usable address.".to_owned(),
        DeemixError::Unavailable(_) => "Could not reach Deemix.".to_owned(),
        DeemixError::Status { status, .. } => format!("Deemix answered HTTP {status}."),
        DeemixError::NotDeemix { .. } => "This address isn't Deemix.".to_owned(),
        DeemixError::Shape { .. } => {
            "DroppedNeedle can't read this Deemix version's answers.".to_owned()
        }
        DeemixError::TooLarge { .. } => "Clear finished downloads in Deemix.".to_owned(),
        DeemixError::InvalidRequest(_)
        | DeemixError::NotSignedIn
        | DeemixError::CantStream
        | DeemixError::Refused { .. } => "Deemix refused the check.".to_owned(),
    }
}
