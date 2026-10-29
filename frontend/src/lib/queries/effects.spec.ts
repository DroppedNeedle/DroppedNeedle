import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: vi.fn() }));

import { EFFECTS, type Sweep } from './effects';

const every = (feature: string, ...parts: string[]): Sweep => ({
	everyScope: [feature, ...parts]
});
const key = (...parts: string[]): Sweep => ({ key: parts });
const U = 'user:u';

// What each fact refreshes. A change here changes what the app reloads after
// a mutation or a server event.
const EXPECTED: Record<keyof typeof EFFECTS, Sweep[]> = {
	catalogChanged: [
		every('library'),
		every('library-management'),
		every('artist'),
		every('home'),
		every('discover'),
		every('genre'),
		every('lyrics'),
		every('local'),
		every('ratings'),
		every('downloads'),
		every('drop-import'),
		key(U, 'search')
	],
	libraryActivityChanged: [key(U, 'library', 'activity')],
	libraryOperationsChanged: [
		every('library', 'operations'),
		key('admin', 'library', 'reviews'),
		every('library-management')
	],
	localChanged: [every('local')],
	downloadsChanged: [key(U, 'downloads', 'tasks')],
	requestsChanged: [key(U, 'requests')],
	searchJobChanged: [key(U, 'downloads', 'search', 'job')],
	searchJobsChanged: [key(U, 'downloads', 'search')],
	dropImportsChanged: [every('drop-import'), { key: [U, 'downloads', 'tasks', 'anon', 0, 'held'] }],
	playlistsChanged: [key(U, 'playlists')],
	newReleasesChanged: [key(U, 'following', 'new-releases-unseen')],
	concertsChanged: [key(U, 'following', 'concerts-unseen'), key(U, 'following', 'concerts')],
	wantedChanged: [key(U, 'wanted', 'list')],
	recommendationsChanged: [key(U, 'discover', 'ignored'), key(U, 'discover', 'home')],
	artistDuplicatesChanged: [every('library', 'artist-reconciliation')],
	usersChanged: [key(U, 'auth', 'admin-users')],
	userImportCandidatesChanged: [key(U, 'auth', 'import', 'job')],
	userQuotaChanged: [key('admin', 'auth', 'user-quota', 'u')],
	albumReread: [
		key(U, 'albums', 'tracks', 'job'),
		key(U, 'album-match', 'job'),
		key(U, 'library', 'album', 'job')
	],
	albumHeaderChanged: [key(U, 'albums', 'basic', 'job')],
	appPasswordsChanged: [key(U, 'connect-apps', 'app-passwords')],
	appPasswordRosterChanged: [key('admin', 'connect-apps', 'app-passwords')],
	connectAppsSettingsChanged: [key('shared', 'connect-apps', 'settings')],
	connectionsChanged: [key(U, 'me', 'connections')],
	sourcePlaylistsChanged: [key(U, 'source-playlists', 'job')],
	spotifyPlaylistsChanged: [key(U, 'spotify', 'playlists')],
	discoverHomeChanged: [key(U, 'discover', 'home')],
	discoveryBatchesChanged: [key(U, 'discover', 'batches')],
	discoveryBatchChanged: [key(U, 'discover', 'batch', 'job')],
	libraryCountsChanged: [
		key(U, 'library', 'catalog', 'stats'),
		key(U, 'library', 'catalog', 'recently-added')
	],
	downloadReadinessChanged: [
		key('shared', 'downloads', 'client-status'),
		key('admin', 'downloads', 'sabnzbd-status'),
		every('home')
	],
	slskdSettingsChanged: [key('admin', 'downloads', 'client-config')],
	usenetSettingsChanged: [
		key('admin', 'downloads', 'sabnzbd'),
		key('admin', 'downloads', 'prowlarr'),
		key('admin', 'downloads', 'indexers')
	],
	sourcePriorityChanged: [key('admin', 'downloads', 'source-priority')],
	downloadPolicyChanged: [
		key('admin', 'downloads', 'policy'),
		key('shared', 'downloads', 'policy-summary')
	],
	wantedSettingsChanged: [key('admin', 'downloads', 'wanted-settings')],
	anyDownloadChanged: [every('downloads')],
	anyRequestChanged: [every('requests')],
	albumStatusChanged: [key(U, 'library', 'album', 'job')],
	libraryChanged: [every('library'), every('local'), every('home')],
	quarantineChanged: [key('admin', 'downloads', 'quarantine')],
	upgradesChanged: [key('admin', 'downloads', 'cutoff-unmet')],
	autoDownloadApprovalsChanged: [key(U, 'following', 'admin-approvals')],
	pendingApprovalsChanged: [{ key: [U, 'following', 'admin-approvals', 'count'], exact: true }],
	followedArtistsChanged: [key(U, 'following', 'artists')],
	contributionsChanged: [key(U, 'library-contributions')],
	contributionChanged: [key(U, 'library-contributions', 'detail', 'job')],
	anyWantedChanged: [every('wanted')],
	scanScheduleChanged: [key('admin', 'library', 'scan-schedule')],
	albumLocalCopyChanged: [key(U, 'album-match', 'job', 'local')],
	albumRemovalsChanged: [key(U, 'library', 'catalog', 'album-removals')],
	anyLibraryActivityChanged: [every('library', 'activity')],
	identityPreparationsChanged: [key(U, 'library', 'operations', 'identity-preparations')],
	repairsChanged: [key('admin', 'library', 'operations', 'repairs')],
	reviewsChanged: [key('admin', 'library', 'reviews')],
	libraryPoliciesChanged: [key('admin', 'library', 'policy')],
	retagPairsChanged: [key('admin', 'library', 'retag-pairs')],
	searchResultsChanged: [key(U, 'search')],
	recycleBinChanged: [key(U, 'library', 'catalog', 'recycle-bin')],
	recycleBinStatusChanged: [key(U, 'library', 'catalog', 'recycle-bin', 'status')],
	libraryPlacementChanged: [key(U, 'library-management', 'placement')],
	lidarrSettingsChanged: [key('admin', 'lidarr-import', 'config')],
	lidarrCandidatesChanged: [key(U, 'lidarr-import', 'candidates')],
	anyFollowChanged: [every('following')],
	homeChanged: [every('home')],
	playlistListChanged: [key(U, 'playlists', 'list')],
	playlistChanged: [key(U, 'playlists', 'detail', 'job')],
	pluginsChanged: [every('plugins')],
	pluginFilesChanged: [key('admin', 'plugins', 'files', 'u')],
	profileChanged: [key(U, 'profile')],
	ratingsChanged: [key(U, 'ratings')],
	albumMatchesChanged: [key(U, 'album-match')],
	remoteSourceChanged: [key(U, 'remotes', 'job')],
	anyLibraryChanged: [every('library')],
	personalMixApprovalsChanged: [
		key('admin', 'me', 'scrobble-preferences', 'personal-mix-approvals')
	],
	sessionsChanged: [key(U, 'auth', 'sessions', 'list')],
	backupsChanged: [key('admin', 'system', 'backups')],
	cacheStatsChanged: [key(U, 'admin', 'cache', 'stats')],
	anyArtistChanged: [every('artist')],
	lyricsChanged: [every('lyrics')],
	albumVideoChanged: [key('shared', 'albums', 'youtube-link', 'u')],
	anyDiscoverChanged: [every('discover')],
	editionChanged: [
		key(U, 'library', 'catalog', 'edition', 'job'),
		key(U, 'library', 'catalog', 'album-detail', 'job'),
		key(U, 'library', 'catalog', 'album-tracks', 'job'),
		key(U, 'library', 'catalog', 'album-copies'),
		key(U, 'library', 'album'),
		key(U, 'library', 'catalog', 'unconfirmed')
	],
	albumGroupChanged: [
		key(U, 'albums', 'basic', 'job'),
		key(U, 'albums', 'tracks', 'job'),
		key(U, 'albums', 'editions', 'job'),
		key('shared', 'albums', 'purchase-options', 'job')
	],
	editionPlansChanged: [key(U, 'library', 'catalog', 'edition', 'job', 'plan')],
	albumHistoryChanged: [key(U, 'library', 'catalog', 'album-history', 'job')],
	favoritesChanged: [key(U, 'favorites')],
	downloadTaskListChanged: [{ key: [U, 'downloads', 'tasks', 'anon', 0], exact: true }],
	downloadActivityChanged: [{ key: [U, 'downloads', 'tasks', 'anon', 0, 'activity'], exact: true }],
	heldFilesChanged: [{ key: [U, 'downloads', 'tasks', 'anon', 0, 'held'] }],
	integrationStatusChanged: [key(U, 'home', 'integration-status')],
	albumCopiesChanged: [key(U, 'album-match', 'job')],
	albumCopiesStale: [{ key: [U, 'album-match', 'job'], mark: true }],
	artistChanged: [key('shared', 'artist', 'u')],
	playlistSourcesDropped: [
		{ key: [U, 'playlists', 'sources', 'job'], exact: true, dropSaved: true }
	],
	discoverRadioChanged: [{ key: [U, 'discover', 'radio', 'job', 'x', {}] }]
};

describe('effects', () => {
	it.each(Object.keys(EXPECTED) as (keyof typeof EFFECTS)[])('%s refreshes its areas', (name) => {
		const build = EFFECTS[name] as (...args: string[]) => Sweep[];
		expect(JSON.parse(JSON.stringify(build('u', 'job', 'x')))).toEqual(EXPECTED[name]);
	});

	it('names every fact in the table', () => {
		expect(Object.keys(EFFECTS).sort()).toEqual(Object.keys(EXPECTED).sort());
	});
});
