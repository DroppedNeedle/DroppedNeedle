import {
	type Accessor,
	type CreateMutationOptions,
	createMutation,
	type DefaultError,
	type QueryClient,
	type QueryKey
} from '@tanstack/svelte-query';

import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { everyScope, key, userScope } from '$lib/queries/keys';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
import { GenreQueryKeyFactory } from '$lib/queries/genre/GenreQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { DropImportQueryKeyFactory } from '$lib/queries/import/DropImportQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalV3Keys';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { RatingQueryKeyFactory } from '$lib/queries/ratings/RatingQueryKeyFactory';
import { RequestQueryKeyFactory } from '$lib/queries/requests/RequestQueryKeyFactory';
import { SearchQueryKeyFactory } from '$lib/queries/search/SearchQueryKeyFactory';
import { WantedQueryKeyFactory } from '$lib/queries/wanted/WantedQueryKeyFactory';
import { AlbumMatchKeyFactory } from '$lib/queries/AlbumMatchQueryKeyFactory';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';
import { AuthQueryKeyFactory } from '$lib/queries/auth/AuthQueryKeyFactory';
import { ConnectAppsQueryKeyFactory } from '$lib/queries/connect-apps/ConnectAppsQueryKeyFactory';
import { ConnectionsQueryKeyFactory } from '$lib/queries/connections/ConnectionsQueryKeyFactory';
import { LibraryContributionQueryKeyFactory } from '$lib/queries/libraryContributions/LibraryContributionQueryKeyFactory';
import { LibraryManagementQueryKeyFactory } from '$lib/queries/library-management/LibraryManagementQueryKeyFactory';
import { LidarrImportQueryKeyFactory } from '$lib/queries/lidarr-import/LidarrImportQueryKeyFactory';
import { PluginQueryKeyFactory } from '$lib/queries/plugins/PluginQueryKeyFactory';
import { ProfileQueryKeyFactory } from '$lib/queries/profile/ProfileQueryKeyFactory';
import { RemoteQueryKeyFactory } from '$lib/queries/remotes/RemoteQueryKeyFactory';
import { ScrobblePreferencesQueryKeyFactory } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQueryKeyFactory';
import { SessionsQueryKeyFactory } from '$lib/queries/sessions/SessionsQueryKeyFactory';
import { SettingsQueryKeyFactory } from '$lib/queries/settings/SettingsQueryKeyFactory';
import { SourcePlaylistQueryKeyFactory } from '$lib/queries/source-playlists/SourcePlaylistQueryKeyFactory';
import { SpotifyQueryKeyFactory } from '$lib/queries/spotify/SpotifyQueryKeyFactory';
import { SystemQueryKeyFactory } from '$lib/queries/system/SystemQueryKeyFactory';
import {
	FavoriteQueryKeyFactory,
	PlaylistQueryKeyFactory
} from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import type { RemoteSource } from '$lib/queries/remotes/types';

type UserId = string | null | undefined;
type SourcePlaylistSource = Parameters<typeof SourcePlaylistQueryKeyFactory.source>[1];

/**
 * One cached area a change refreshes: every query under `key` (only `key`
 * itself when `exact`), or under `[feature, ...parts]` in every scope.
 * `dropSaved` also removes the saved copies, for a change where painting the
 * old answer first would be wrong; `mark` only marks the queries stale, for a
 * page that refetches them itself.
 */
export type Sweep =
	| {
			readonly key: QueryKey;
			readonly exact?: true;
			readonly dropSaved?: true;
			readonly mark?: true;
	  }
	| { readonly everyScope: readonly [feature: string, ...parts: string[]] };

/** A named fact about what changed, and the cached areas it refreshes. */
export interface Effect {
	readonly name: keyof typeof EFFECTS;
	readonly sweeps: readonly Sweep[];
}

const LIBRARY_MANAGEMENT = 'library-management';

/**
 * What can change, and what each change refreshes. Mutations and server
 * events name a fact here instead of listing keys themselves.
 */
export const EFFECTS = {
	/** Catalog rows moved: a scan, an import, a retag, a merge or a removal. */
	catalogChanged: (userId: UserId): Sweep[] => [
		{ everyScope: [LibraryQueryKeyFactory.feature] },
		{ everyScope: [LIBRARY_MANAGEMENT] },
		{ everyScope: [ArtistQueryKeyFactory.feature] },
		{ everyScope: [HomeQueryKeyFactory.feature] },
		{ everyScope: [DiscoverQueryKeyFactory.feature] },
		{ everyScope: [GenreQueryKeyFactory.feature] },
		{ everyScope: [LyricsQueryKeyFactory.feature] },
		{ everyScope: [LOCAL_KEYS.feature] },
		{ everyScope: [RatingQueryKeyFactory.feature] },
		{ everyScope: [DownloadQueryKeyFactory.feature] },
		{ everyScope: [DropImportQueryKeyFactory.feature] },
		{ key: SearchQueryKeyFactory.v3.root(userId) }
	],
	/** Scans, identification or operations moved without touching the catalog. */
	libraryActivityChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.activity(userId) }
	],
	/** The admin's view of library runs, reviews and managed changes. */
	libraryOperationsChanged: (): Sweep[] => [
		{ everyScope: [LibraryQueryKeyFactory.feature, 'operations'] },
		{ key: LibraryQueryKeyFactory.reviewsPrefix() },
		{ everyScope: [LIBRARY_MANAGEMENT] }
	],
	/** The local library lists. */
	localChanged: (): Sweep[] => [{ everyScope: [LOCAL_KEYS.feature] }],
	/** The download queue moved: a task, its status, its source or a held file. */
	downloadsChanged: (userId: UserId): Sweep[] => [
		{ key: DownloadQueryKeyFactory.tasksPrefix(userId) }
	],
	/** The caller's requests. */
	requestsChanged: (userId: UserId): Sweep[] => [{ key: RequestQueryKeyFactory.all(userId) }],
	/** One manual album search moved. */
	searchJobChanged: (userId: UserId, jobId: string): Sweep[] => [
		{ key: DownloadQueryKeyFactory.searchJob(userId, jobId) }
	],
	/** The caller's manual album searches, whichever moved. */
	searchJobsChanged: (userId: UserId): Sweep[] => [
		{ key: key(userScope(userId), DownloadQueryKeyFactory.feature, 'search') }
	],
	/** The caller's drop imports, and the held files they leave. */
	dropImportsChanged: (userId: UserId): Sweep[] => [
		{ everyScope: [DropImportQueryKeyFactory.feature] },
		{ key: DownloadQueryKeyFactory.heldPrefix(userId) }
	],
	/** The caller's playlists, the weekly mix included. */
	playlistsChanged: (userId: UserId): Sweep[] => [{ key: PlaylistQueryKeyFactory.root(userId) }],
	/** New releases from followed artists. */
	newReleasesChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.newReleasesUnseen(userId) }
	],
	/** Concerts for followed artists. */
	concertsChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.concertsUnseen(userId) },
		{ key: FollowQueryKeyFactory.concerts(userId) }
	],
	/** The caller's wanted albums. */
	wantedChanged: (userId: UserId): Sweep[] => [{ key: WantedQueryKeyFactory.list(userId) }],
	/** The caller asked for fewer recommendations like one album. */
	recommendationsChanged: (userId: UserId): Sweep[] => [
		{ key: DiscoverQueryKeyFactory.v3.ignored(userId) },
		{ key: DiscoverQueryKeyFactory.v3.home(userId) }
	],
	/** The admin's artist duplicate groups. */
	artistDuplicatesChanged: (): Sweep[] => [{ everyScope: ['library', 'artist-reconciliation'] }],
	/** The admin's list of accounts. */
	usersChanged: (userId: UserId): Sweep[] => [{ key: AuthQueryKeyFactory.adminUsers(userId) }],
	/** Accounts a media server offers for import. */
	userImportCandidatesChanged: (userId: UserId, provider: string): Sweep[] => [
		{ key: AuthQueryKeyFactory.importCandidates(userId, provider) }
	],
	/** One account's quota. */
	userQuotaChanged: (accountId: string): Sweep[] => [
		{ key: AuthQueryKeyFactory.userQuota(accountId) }
	],
	/** An album read from MusicBrainz again: its tracks, its copies and its library status. */
	albumReread: (userId: UserId, albumId: string): Sweep[] => [
		{ key: AlbumQueryKeyFactory.tracks(userId, albumId) },
		{ key: AlbumMatchKeyFactory.album(userId, albumId) },
		{ key: LibraryQueryKeyFactory.album(userId, albumId) }
	],
	/** An album's header. */
	albumHeaderChanged: (userId: UserId, albumId: string): Sweep[] => [
		{ key: AlbumQueryKeyFactory.basic(userId, albumId) }
	],
	/** The caller's app passwords. */
	appPasswordsChanged: (userId: string): Sweep[] => [
		{ key: ConnectAppsQueryKeyFactory.appPasswords(userId) }
	],
	/** Every account's app passwords, as the admin sees them. */
	appPasswordRosterChanged: (): Sweep[] => [
		{ key: ConnectAppsQueryKeyFactory.adminAppPasswords() }
	],
	/** The connect-apps settings. */
	connectAppsSettingsChanged: (): Sweep[] => [{ key: ConnectAppsQueryKeyFactory.settings() }],
	/** The caller's linked accounts. */
	connectionsChanged: (userId: UserId): Sweep[] => [
		{ key: ConnectionsQueryKeyFactory.list(userId) }
	],
	/** The caller's playlists on one media server. */
	sourcePlaylistsChanged: (userId: UserId, source: SourcePlaylistSource): Sweep[] => [
		{ key: SourcePlaylistQueryKeyFactory.source(userId, source) }
	],
	/** The caller's Spotify playlists. */
	spotifyPlaylistsChanged: (userId: UserId): Sweep[] => [
		{ key: SpotifyQueryKeyFactory.playlists(userId) }
	],
	/** The caller's Discover page. */
	discoverHomeChanged: (userId: UserId): Sweep[] => [
		{ key: DiscoverQueryKeyFactory.v3.home(userId) }
	],
	/** The caller's discovery batches. */
	discoveryBatchesChanged: (userId: UserId): Sweep[] => [
		{ key: DiscoverQueryKeyFactory.v3.batches(userId) }
	],
	/** One discovery batch. */
	discoveryBatchChanged: (userId: UserId, batchId: string): Sweep[] => [
		{ key: DiscoverQueryKeyFactory.v3.batch(userId, batchId) }
	],
	/** The library's counts and recently added albums. */
	libraryCountsChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.stats(userId) },
		{ key: LibraryQueryKeyFactory.catalog.recentlyAddedPrefix(userId) }
	],
	/** Whether each download source is ready, and Home's setup prompts that read it. */
	downloadReadinessChanged: (): Sweep[] => [
		{ key: DownloadQueryKeyFactory.clientStatus() },
		{ key: DownloadQueryKeyFactory.sabnzbdStatus() },
		{ everyScope: [HomeQueryKeyFactory.feature] }
	],
	/** The Soulseek connection settings. */
	slskdSettingsChanged: (): Sweep[] => [{ key: DownloadQueryKeyFactory.clientConfig() }],
	/** The Usenet settings: SABnzbd, Prowlarr, the indexers and the search backend. */
	usenetSettingsChanged: (): Sweep[] => [
		{ key: DownloadQueryKeyFactory.sabnzbd() },
		{ key: DownloadQueryKeyFactory.prowlarr() },
		{ key: DownloadQueryKeyFactory.indexers() }
	],
	/** The order download sources are tried in. */
	sourcePriorityChanged: (): Sweep[] => [{ key: DownloadQueryKeyFactory.sourcePriority() }],
	/** The download policy and its summary. */
	downloadPolicyChanged: (): Sweep[] => [
		{ key: DownloadQueryKeyFactory.policy() },
		{ key: DownloadQueryKeyFactory.policySummary() }
	],
	/** The wanted watcher's settings. */
	wantedSettingsChanged: (): Sweep[] => [{ key: DownloadQueryKeyFactory.wantedSettings() }],
	/** Any download, in the caller's views and the admin's. */
	anyDownloadChanged: (): Sweep[] => [{ everyScope: [DownloadQueryKeyFactory.feature] }],
	/** Any request, in the caller's views and the admin's. */
	anyRequestChanged: (): Sweep[] => [{ everyScope: [RequestQueryKeyFactory.feature] }],
	/** One album's library status. */
	albumStatusChanged: (userId: UserId, albumId: string): Sweep[] => [
		{ key: LibraryQueryKeyFactory.album(userId, albumId) }
	],
	/** Files went into the library: its pages, the local lists and Home. */
	libraryChanged: (): Sweep[] => [
		{ everyScope: [LibraryQueryKeyFactory.feature] },
		{ everyScope: [LOCAL_KEYS.feature] },
		{ everyScope: [HomeQueryKeyFactory.feature] }
	],
	/** The download quarantine. */
	quarantineChanged: (): Sweep[] => [{ key: DownloadQueryKeyFactory.quarantine() }],
	/** Albums below the quality cutoff. */
	upgradesChanged: (): Sweep[] => [{ key: DownloadQueryKeyFactory.cutoffUnmet() }],
	/** Auto-download approvals waiting for an admin, their batches and their count. */
	autoDownloadApprovalsChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.adminApprovals(userId) }
	],
	/** The count of approvals waiting for an admin. */
	pendingApprovalsChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.pendingApprovalCount(userId), exact: true }
	],
	/** The artists the caller follows. */
	followedArtistsChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.artists(userId) }
	],
	/** The caller's library contributions. */
	contributionsChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryContributionQueryKeyFactory.root(userId) }
	],
	/** One library contribution. */
	contributionChanged: (userId: UserId, contributionId: string): Sweep[] => [
		{ key: LibraryContributionQueryKeyFactory.detail(userId, contributionId) }
	],
	/** Wanted albums, in every scope. */
	anyWantedChanged: (): Sweep[] => [{ everyScope: [WantedQueryKeyFactory.feature] }],
	/** The library scan schedule. */
	scanScheduleChanged: (): Sweep[] => [{ key: LibraryQueryKeyFactory.scanSchedule() }],
	/** One album's local copy, as the album page shows it. */
	albumLocalCopyChanged: (userId: UserId, albumId: string): Sweep[] => [
		{ key: AlbumMatchKeyFactory.local(userId, albumId) }
	],
	/** Albums waiting to be removed. */
	albumRemovalsChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.albumRemovalsPrefix(userId) }
	],
	/** Library activity, in the caller's views and the admin's. */
	anyLibraryActivityChanged: (): Sweep[] => [
		{ everyScope: [LibraryQueryKeyFactory.feature, 'activity'] }
	],
	/** Identity preparation runs. */
	identityPreparationsChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.identityPreparationsPrefix(userId) }
	],
	/** Identity repair runs. */
	repairsChanged: (): Sweep[] => [{ key: LibraryQueryKeyFactory.repairsPrefix() }],
	/** The library's review lists. */
	reviewsChanged: (): Sweep[] => [{ key: LibraryQueryKeyFactory.reviewsPrefix() }],
	/** The library folders' policies. */
	libraryPoliciesChanged: (): Sweep[] => [{ key: LibraryQueryKeyFactory.policyPrefix() }],
	/** Album pairs a scan would not join by itself. */
	retagPairsChanged: (): Sweep[] => [{ key: LibraryQueryKeyFactory.retagPairs() }],
	/** The caller's search results. */
	searchResultsChanged: (userId: UserId): Sweep[] => [
		{ key: SearchQueryKeyFactory.v3.root(userId) }
	],
	/** The recycle bin's contents, summary and status. */
	recycleBinChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.recycleBinPrefix(userId) }
	],
	/** Whether the recycle bin is being emptied. */
	recycleBinStatusChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.recycleBinStatus(userId) }
	],
	/** Whether each library folder can take managed file moves. */
	libraryPlacementChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryManagementQueryKeyFactory.placement(userId) }
	],
	/** The Lidarr connection. */
	lidarrSettingsChanged: (): Sweep[] => [{ key: LidarrImportQueryKeyFactory.config() }],
	/** Artists Lidarr offers for import. */
	lidarrCandidatesChanged: (userId: UserId): Sweep[] => [
		{ key: LidarrImportQueryKeyFactory.candidates(userId) }
	],
	/** Follows, new releases and approvals, in every scope. */
	anyFollowChanged: (): Sweep[] => [{ everyScope: ['following'] }],
	/** Home, in every scope. */
	homeChanged: (): Sweep[] => [{ everyScope: [HomeQueryKeyFactory.feature] }],
	/** The caller's playlist list. */
	playlistListChanged: (userId: UserId): Sweep[] => [{ key: PlaylistQueryKeyFactory.list(userId) }],
	/** One playlist. */
	playlistChanged: (userId: UserId, playlistId: string): Sweep[] => [
		{ key: PlaylistQueryKeyFactory.detail(userId, playlistId) }
	],
	/** Installed plugins, in every scope. */
	pluginsChanged: (): Sweep[] => [{ everyScope: [PluginQueryKeyFactory.feature] }],
	/** Files one plugin added. */
	pluginFilesChanged: (name: string): Sweep[] => [{ key: PluginQueryKeyFactory.files(name) }],
	/** One account's profile. */
	profileChanged: (userId: string): Sweep[] => [{ key: ProfileQueryKeyFactory.profile(userId) }],
	/** The caller's ratings. */
	ratingsChanged: (userId: UserId): Sweep[] => [{ key: RatingQueryKeyFactory.user(userId) }],
	/** Which copies the caller's media servers hold of each album. */
	albumMatchesChanged: (userId: UserId): Sweep[] => [{ key: AlbumMatchKeyFactory.user(userId) }],
	/** Everything shown for one of the caller's media servers. */
	remoteSourceChanged: (userId: UserId, source: RemoteSource): Sweep[] => [
		{ key: RemoteQueryKeyFactory.source(userId, source) }
	],
	/** The library, in every scope. */
	anyLibraryChanged: (): Sweep[] => [{ everyScope: [LibraryQueryKeyFactory.feature] }],
	/** Weekly Mix auto-requests waiting for an admin. */
	personalMixApprovalsChanged: (): Sweep[] => [
		{ key: ScrobblePreferencesQueryKeyFactory.personalMixApprovals() }
	],
	/** The caller's signed-in sessions. */
	sessionsChanged: (userId: UserId): Sweep[] => [{ key: SessionsQueryKeyFactory.list(userId) }],
	/** The database backups and their checks. */
	backupsChanged: (): Sweep[] => [{ key: SystemQueryKeyFactory.backups() }],
	/** The metadata cache's sizes. */
	cacheStatsChanged: (userId: UserId): Sweep[] => [
		{ key: SettingsQueryKeyFactory.cacheStats(userId) }
	],
	/** Artist pages, in every scope. */
	anyArtistChanged: (): Sweep[] => [{ everyScope: [ArtistQueryKeyFactory.feature] }],
	/** Lyrics, in every scope. */
	lyricsChanged: (): Sweep[] => [{ everyScope: [LyricsQueryKeyFactory.feature] }],
	/** One album's YouTube video. */
	albumVideoChanged: (albumId: string): Sweep[] => [
		{ key: AlbumQueryKeyFactory.youtubeLink(albumId) }
	],
	/** Discover, in every scope. */
	anyDiscoverChanged: (): Sweep[] => [{ everyScope: [DiscoverQueryKeyFactory.feature] }],
	/**
	 * One library album's edition: its status and plans, its detail, tracks
	 * and copies, the library status the album pages read, and the waiting list.
	 */
	editionChanged: (userId: UserId, localId: string): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.edition(userId, localId) },
		{ key: LibraryQueryKeyFactory.catalog.albumDetail(userId, localId) },
		{ key: LibraryQueryKeyFactory.catalog.albumTracksPrefix(userId, localId) },
		{ key: LibraryQueryKeyFactory.catalog.albumCopiesPrefix(userId) },
		{ key: LibraryQueryKeyFactory.albumPrefix(userId) },
		{ key: LibraryQueryKeyFactory.catalog.unconfirmedPrefix(userId) }
	],
	/** One release group's album page: header, tracklist, editions and where to buy it. */
	albumGroupChanged: (userId: UserId, group: string): Sweep[] => [
		{ key: AlbumQueryKeyFactory.basic(userId, group) },
		{ key: AlbumQueryKeyFactory.tracks(userId, group) },
		{ key: AlbumQueryKeyFactory.editions(userId, group) },
		{ key: AlbumQueryKeyFactory.purchaseOptions(group) }
	],
	/** What taking each edition would do to one album's files. */
	editionPlansChanged: (userId: UserId, localId: string): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.editionPlanPrefix(userId, localId) }
	],
	/** One library album's history. */
	albumHistoryChanged: (userId: UserId, localId: string): Sweep[] => [
		{ key: LibraryQueryKeyFactory.catalog.albumHistory(userId, localId) }
	],
	/** The caller's favourites. */
	favoritesChanged: (userId: UserId): Sweep[] => [{ key: FavoriteQueryKeyFactory.user(userId) }],
	/** The caller's task list itself, not the lists nested under it. */
	downloadTaskListChanged: (userId: UserId): Sweep[] => [
		{ key: DownloadQueryKeyFactory.tasks(userId), exact: true }
	],
	/** The caller's download activity summary. */
	downloadActivityChanged: (userId: UserId): Sweep[] => [
		{ key: DownloadQueryKeyFactory.activity(userId), exact: true }
	],
	/** The caller's held files. */
	heldFilesChanged: (userId: UserId): Sweep[] => [
		{ key: DownloadQueryKeyFactory.heldPrefix(userId) }
	],
	/** Which services are set up, as the setup dots read it. */
	integrationStatusChanged: (userId: UserId): Sweep[] => [
		{ key: HomeQueryKeyFactory.integrationStatus(userId) }
	],
	/** Which copies the caller's media servers hold of one album. */
	albumCopiesChanged: (userId: UserId, albumId: string): Sweep[] => [
		{ key: AlbumMatchKeyFactory.album(userId, albumId) }
	],
	/** The same, marked stale only: read again on the next visit. */
	albumCopiesStale: (userId: UserId, albumId: string): Sweep[] => [
		{ key: AlbumMatchKeyFactory.album(userId, albumId), mark: true }
	],
	/** One artist's header. */
	artistChanged: (artistId: string): Sweep[] => [{ key: ArtistQueryKeyFactory.basic(artistId) }],
	/** One playlist's kept source answer, dropped with its saved copy. */
	playlistSourcesDropped: (userId: UserId, playlistId: string): Sweep[] => [
		{ key: PlaylistQueryKeyFactory.sources(userId, playlistId), exact: true, dropSaved: true }
	],
	/** One radio station on Discover. */
	discoverRadioChanged: (userId: UserId, seedType: string, seedId: string): Sweep[] => [
		{ key: DiscoverQueryKeyFactory.v3.radio(userId, seedType, seedId, {}) }
	]
};

type Facts = typeof EFFECTS;

function fact<N extends keyof Facts>(name: N) {
	return (...args: Parameters<Facts[N]>): Effect => ({
		name,
		sweeps: (EFFECTS[name] as (...a: Parameters<Facts[N]>) => Sweep[])(...args)
	});
}

export const catalogChanged = fact('catalogChanged');
export const libraryActivityChanged = fact('libraryActivityChanged');
export const libraryOperationsChanged = fact('libraryOperationsChanged');
export const localChanged = fact('localChanged');
export const downloadsChanged = fact('downloadsChanged');
export const requestsChanged = fact('requestsChanged');
export const searchJobChanged = fact('searchJobChanged');
export const searchJobsChanged = fact('searchJobsChanged');
export const dropImportsChanged = fact('dropImportsChanged');
export const playlistsChanged = fact('playlistsChanged');
export const newReleasesChanged = fact('newReleasesChanged');
export const concertsChanged = fact('concertsChanged');
export const wantedChanged = fact('wantedChanged');
export const recommendationsChanged = fact('recommendationsChanged');
export const artistDuplicatesChanged = fact('artistDuplicatesChanged');
export const usersChanged = fact('usersChanged');
export const userImportCandidatesChanged = fact('userImportCandidatesChanged');
export const userQuotaChanged = fact('userQuotaChanged');
export const albumReread = fact('albumReread');
export const albumHeaderChanged = fact('albumHeaderChanged');
export const appPasswordsChanged = fact('appPasswordsChanged');
export const appPasswordRosterChanged = fact('appPasswordRosterChanged');
export const connectAppsSettingsChanged = fact('connectAppsSettingsChanged');
export const connectionsChanged = fact('connectionsChanged');
export const sourcePlaylistsChanged = fact('sourcePlaylistsChanged');
export const spotifyPlaylistsChanged = fact('spotifyPlaylistsChanged');
export const discoverHomeChanged = fact('discoverHomeChanged');
export const discoveryBatchesChanged = fact('discoveryBatchesChanged');
export const discoveryBatchChanged = fact('discoveryBatchChanged');
export const libraryCountsChanged = fact('libraryCountsChanged');
export const downloadReadinessChanged = fact('downloadReadinessChanged');
export const slskdSettingsChanged = fact('slskdSettingsChanged');
export const usenetSettingsChanged = fact('usenetSettingsChanged');
export const sourcePriorityChanged = fact('sourcePriorityChanged');
export const downloadPolicyChanged = fact('downloadPolicyChanged');
export const wantedSettingsChanged = fact('wantedSettingsChanged');
export const anyDownloadChanged = fact('anyDownloadChanged');
export const anyRequestChanged = fact('anyRequestChanged');
export const albumStatusChanged = fact('albumStatusChanged');
export const libraryChanged = fact('libraryChanged');
export const quarantineChanged = fact('quarantineChanged');
export const upgradesChanged = fact('upgradesChanged');
export const autoDownloadApprovalsChanged = fact('autoDownloadApprovalsChanged');
export const pendingApprovalsChanged = fact('pendingApprovalsChanged');
export const followedArtistsChanged = fact('followedArtistsChanged');
export const contributionsChanged = fact('contributionsChanged');
export const contributionChanged = fact('contributionChanged');
export const anyWantedChanged = fact('anyWantedChanged');
export const scanScheduleChanged = fact('scanScheduleChanged');
export const albumLocalCopyChanged = fact('albumLocalCopyChanged');
export const albumRemovalsChanged = fact('albumRemovalsChanged');
export const libraryPoliciesChanged = fact('libraryPoliciesChanged');
export const anyLibraryActivityChanged = fact('anyLibraryActivityChanged');
export const identityPreparationsChanged = fact('identityPreparationsChanged');
export const repairsChanged = fact('repairsChanged');
export const reviewsChanged = fact('reviewsChanged');
export const retagPairsChanged = fact('retagPairsChanged');
export const searchResultsChanged = fact('searchResultsChanged');
export const recycleBinChanged = fact('recycleBinChanged');
export const recycleBinStatusChanged = fact('recycleBinStatusChanged');
export const libraryPlacementChanged = fact('libraryPlacementChanged');
export const lidarrSettingsChanged = fact('lidarrSettingsChanged');
export const lidarrCandidatesChanged = fact('lidarrCandidatesChanged');
export const anyFollowChanged = fact('anyFollowChanged');
export const homeChanged = fact('homeChanged');
export const playlistListChanged = fact('playlistListChanged');
export const playlistChanged = fact('playlistChanged');
export const pluginsChanged = fact('pluginsChanged');
export const pluginFilesChanged = fact('pluginFilesChanged');
export const profileChanged = fact('profileChanged');
export const ratingsChanged = fact('ratingsChanged');
export const albumMatchesChanged = fact('albumMatchesChanged');
export const remoteSourceChanged = fact('remoteSourceChanged');
export const anyLibraryChanged = fact('anyLibraryChanged');
export const personalMixApprovalsChanged = fact('personalMixApprovalsChanged');
export const sessionsChanged = fact('sessionsChanged');
export const backupsChanged = fact('backupsChanged');
export const cacheStatsChanged = fact('cacheStatsChanged');
export const anyArtistChanged = fact('anyArtistChanged');
export const lyricsChanged = fact('lyricsChanged');
export const albumVideoChanged = fact('albumVideoChanged');
export const anyDiscoverChanged = fact('anyDiscoverChanged');
export const editionChanged = fact('editionChanged');
export const albumGroupChanged = fact('albumGroupChanged');
export const editionPlansChanged = fact('editionPlansChanged');
export const albumHistoryChanged = fact('albumHistoryChanged');
export const favoritesChanged = fact('favoritesChanged');
export const downloadTaskListChanged = fact('downloadTaskListChanged');
export const downloadActivityChanged = fact('downloadActivityChanged');
export const heldFilesChanged = fact('heldFilesChanged');
export const integrationStatusChanged = fact('integrationStatusChanged');
export const albumCopiesChanged = fact('albumCopiesChanged');
export const albumCopiesStale = fact('albumCopiesStale');
export const artistChanged = fact('artistChanged');
export const playlistSourcesDropped = fact('playlistSourcesDropped');
export const discoverRadioChanged = fact('discoverRadioChanged');

function filterFor(sweep: Sweep) {
	if ('everyScope' in sweep) return everyScope(...sweep.everyScope);
	return {
		queryKey: sweep.key,
		...(sweep.exact ? { exact: true } : {}),
		...(sweep.mark ? { refetchType: 'none' as const } : {})
	};
}

/**
 * Refreshes every area the given facts sweep, each area once, in `client`
 * when one is given (a page's own client from context), else the app's.
 */
export async function applyEffectsIn(
	client: QueryClient | undefined,
	effects: readonly Effect[]
): Promise<void> {
	const sweeps = new Map<string, Sweep>();
	for (const { sweeps: list } of effects) {
		for (const sweep of list) sweeps.set(JSON.stringify(sweep), sweep);
	}
	await Promise.all(
		[...sweeps.values()].map((sweep) => {
			const dropSaved = 'key' in sweep && sweep.dropSaved;
			if (!client && !dropSaved) return invalidateQueriesWithPersister(filterFor(sweep));
			return invalidateQueriesWithPersister(filterFor(sweep), undefined, {
				...(client ? { client } : {}),
				...(dropSaved ? { removePersisted: true } : {})
			});
		})
	);
}

/** Refreshes every area the given facts sweep, each area once. */
export function applyEffects(...effects: Effect[]): Promise<void> {
	return applyEffectsIn(undefined, effects);
}

/** A mutation's options, plus what a success changes. */
export type AppMutationOptions<TData, TError, TVariables, TOnMutateResult> = CreateMutationOptions<
	TData,
	TError,
	TVariables,
	TOnMutateResult
> & {
	/**
	 * The facts a success makes true, applied after `onSuccess`. A mutation
	 * that changes nothing cached says so with an empty list.
	 */
	effects: (data: TData, variables: TVariables, onMutateResult: TOnMutateResult) => Effect[];
};

/**
 * A mutation that declares what it changes. On success it runs `onSuccess`,
 * then refreshes what `effects` names, in `client` when one is given.
 */
export function createAppMutation<
	TData = unknown,
	TError = DefaultError,
	TVariables = void,
	TOnMutateResult = unknown
>(
	options: Accessor<AppMutationOptions<TData, TError, TVariables, TOnMutateResult>>,
	client?: QueryClient
) {
	return createMutation<TData, TError, TVariables, TOnMutateResult>(
		() => {
			const { effects, onSuccess, ...rest } = options();
			return {
				...rest,
				onSuccess: async (data, variables, onMutateResult, context) => {
					await onSuccess?.(data, variables, onMutateResult, context);
					// A refresh that fails leaves the change itself done.
					await applyEffectsIn(client, effects(data, variables, onMutateResult)).catch(
						(error: unknown) => console.error('Refreshing after a change failed', error)
					);
				}
			};
		},
		client ? () => client : undefined
	);
}
