/**
 * Integration coverage: every native-engine backend route must have a corresponding
 * frontend surface in the central `API` endpoint registry (which the TanStack Query
 * queries/mutations consume). This both proves coverage and catches path drift between
 * the backend routes and the frontend builders.
 *
 * The download-task `GET /downloads/{id}/files` route has no dedicated builder by
 * design - the file list is delivered inside the task-detail response and the live
 * SSE stream, not fetched separately - so it is intentionally excluded below.
 */
import { describe, expect, it } from 'vitest';

import { API } from '$lib/constants';

// [description, actual path produced by the API builder, expected backend route]
const COVERAGE: Array<[string, string, string]> = [
	// download-client (admin)
	['download-client config', API.downloadClient.config(), '/api/v1/download-client/config'],
	['download-client status', API.downloadClient.status(), '/api/v1/download-client/status'],
	['download-client test', API.downloadClient.test(), '/api/v1/download-client/test'],
	// download tasks (user-scoped)
	['download detail', API.downloads.get('T1'), '/api/v1/downloads/T1'],
	['download stream', API.downloads.stream('T1'), '/api/v1/downloads/T1/stream'],
	['download cancel', API.downloads.cancel('T1'), '/api/v1/downloads/T1/cancel'],
	['download retry', API.downloads.retry('T1'), '/api/v1/downloads/T1/retry'],
	['download reimport', API.downloads.reimport('T1'), '/api/v1/downloads/T1/reimport'],
	// search (user-scoped)
	['search album', API.downloads.searchAlbum(), '/api/v1/downloads/search/album'],
	['search job', API.downloads.searchJob('J1'), '/api/v1/downloads/search/J1'],
	['search pick', API.downloads.pick('J1'), '/api/v1/downloads/search/J1/pick'],
	[
		'search dismiss review',
		API.downloads.dismissReview('J1'),
		'/api/v1/downloads/search/J1/dismiss'
	],
	['search cancel', API.downloads.cancelSearch('J1'), '/api/v1/downloads/search/J1/cancel'],
	// quarantine (admin)
	['quarantine list', API.downloads.quarantine(), '/api/v1/downloads/quarantine'],
	['quarantine delete', API.downloads.quarantineDelete(7), '/api/v1/downloads/quarantine/7'],
	// track request (user-scoped)
	['track request', API.tracks.request('R1'), '/api/v1/tracks/R1/request'],
	// library reads (user)
	['library stats', API.library.stats(), '/api/v1/library/stats'],
	['album status', API.library.album('M1'), '/api/v1/library/albums/M1/status'],
	['album tracks', API.library.albumTracks('M1'), '/api/v1/library/albums/M1/tracks'],
	// library admin (tags + scan control)
	['track tags', API.library.trackTags('F1'), '/api/v1/library/tracks/F1/tags'],
	['update track tags', API.library.updateTrackTags('F1'), '/api/v1/library/tracks/F1'],
	['remove library track', API.library.removeTrack('F1'), '/api/v1/library/tracks/F1'],
	['rescan album', API.library.rescanAlbum('M1'), '/api/v1/library/albums/M1/rescan'],
	['scan start', API.library.scanStart(), '/api/v1/library/scan/start'],
	['scan cancel', API.library.scanCancel(), '/api/v1/library/scan/cancel'],
	['scan status', API.library.scanStatus(), '/api/v1/library/scan/status'],
	['scan unmatched', API.library.unmatched(), '/api/v1/library/scan/unmatched'],
	[
		'resolve unmatched',
		API.library.resolveUnmatched(1),
		'/api/v1/library/scan/unmatched/1/resolve'
	],
	[
		'resolve unmatched batch',
		API.library.resolveUnmatchedBatch(),
		'/api/v1/library/scan/unmatched/resolve-batch'
	],
	// per-user section visibility (user-scoped)
	['section prefs', API.me.sectionPrefs(), '/api/v1/me/section-prefs'],
	// external-service health for the header status indicator
	['system health', API.system.health(), '/api/v1/system/health'],
	// keyless 30s previews (user-scoped)
	[
		'track preview',
		API.discoverTrackPreview('A', 'T'),
		'/api/v1/discover/track-preview?artist=A&track=T'
	],
	[
		'album preview',
		API.discoverAlbumPreview('A', 'B'),
		'/api/v1/discover/album-preview?artist=A&album=B'
	],
	// smart radio (user-scoped)
	['radio plan', API.discoverRadioPlan(), '/api/v1/discover/radio/plan'],
	// discovery batches (user-scoped)
	['batches list/create', API.discoverBatches(), '/api/v1/discover/batches'],
	['batch detail', API.discoverBatch('B1'), '/api/v1/discover/batches/B1'],
	[
		'batch remove',
		API.discoverBatchRemove('B1', true),
		'/api/v1/discover/batches/B1?remove_albums=true'
	],
	// following hub (user-scoped)
	['followed artists', API.following.artists(), '/api/v1/following/artists'],
	[
		'new releases',
		API.following.newReleases(50, 0),
		'/api/v1/following/new-releases?limit=50&offset=0'
	],
	[
		'recent releases log',
		API.following.recentReleases(30, 8, false),
		'/api/v1/following/new-releases/recent?days=30&limit=8&include_owned=false'
	],
	[
		'new releases unseen count',
		API.following.newReleasesUnseenCount(),
		'/api/v1/following/new-releases/unseen-count'
	],
	[
		'mark new releases seen',
		API.following.markNewReleasesSeen(),
		'/api/v1/following/new-releases/seen'
	],
	['following events', API.following.events(), '/api/v1/following/events'],
	// upcoming events / concerts (user-scoped)
	['concerts', API.following.concerts(), '/api/v1/following/concerts'],
	['concert cities', API.following.concertCities(), '/api/v1/following/concerts/cities'],
	[
		'concert city search',
		API.following.concertCitySearch('liverpool'),
		'/api/v1/following/concerts/city-search?q=liverpool'
	],
	[
		'concerts unseen count',
		API.following.concertsUnseenCount(),
		'/api/v1/following/concerts/unseen-count'
	],
	['mark concerts seen', API.following.markConcertsSeen(), '/api/v1/following/concerts/seen'],
	// Weekly Mix (user-scoped refresh + admin standing-grant queue)
	['personal mix refresh', API.me.personalMixRefresh(), '/api/v1/me/personal-mix/refresh'],
	[
		'personal mix approvals',
		API.requests.personalMixApprovals(),
		'/api/v1/requests/personal-mix-approvals'
	],
	[
		'personal mix approve',
		API.requests.approvePersonalMix('U1'),
		'/api/v1/requests/personal-mix-approvals/U1/approve'
	],
	[
		'personal mix reject',
		API.requests.rejectPersonalMix('U1'),
		'/api/v1/requests/personal-mix-approvals/U1/reject'
	],
	[
		'personal mix revoke',
		API.requests.revokePersonalMix('U1'),
		'/api/v1/requests/personal-mix-approvals/U1/revoke'
	],
	// Wanted watches (availability re-search, user-scoped with admin visibility)
	['wanted list', API.requests.wanted(), '/api/v1/requests/wanted'],
	['wanted stop', API.requests.wantedStop('M1'), '/api/v1/requests/wanted/M1/stop'],
	['wanted resume', API.requests.wantedResume('M1'), '/api/v1/requests/wanted/M1/resume'],
	['wanted mark seen', API.requests.wantedSeen('M1'), '/api/v1/requests/wanted/M1/seen'],
	['wanted watcher settings', API.downloadClients.wanted(), '/api/v1/download-clients/wanted'],
	// Lidarr import (LidarrImport): admin config/test, user status/artists/import
	['lidarr-import config', API.lidarrImport.config(), '/api/v1/lidarr-import/config'],
	['lidarr-import test', API.lidarrImport.test(), '/api/v1/lidarr-import/test'],
	['lidarr-import status', API.lidarrImport.status(), '/api/v1/lidarr-import/status'],
	['lidarr-import artists', API.lidarrImport.artists(), '/api/v1/lidarr-import/artists'],
	['lidarr-import import', API.lidarrImport.import(), '/api/v1/lidarr-import/import'],
	// Bulk auto-download approval batches (admin, requests router)
	[
		'auto-download approval batches',
		API.requests.autoDownloadApprovalBatches(),
		'/api/v1/requests/auto-download-approval-batches'
	],
	[
		'approve approval batch',
		API.requests.approveAutoDownloadBatch('B1'),
		'/api/v1/requests/auto-download-approval-batches/B1/approve'
	],
	[
		'reject approval batch',
		API.requests.rejectAutoDownloadBatch('B1'),
		'/api/v1/requests/auto-download-approval-batches/B1/reject'
	]
];

// Routes whose builder takes query params - assert the path prefix only.
const PREFIX_COVERAGE: Array<[string, string, string]> = [
	['downloads list', API.downloads.list(), '/api/v1/downloads'],
	['search stream', API.downloads.searchStream('J1'), '/api/v1/downloads/search/stream'],
	['library albums', API.library.albums(), '/api/v1/library/albums'],
	['library artists', API.library.artists(), '/api/v1/library/artists'],
	['library tracks', API.library.tracks(), '/api/v1/library/tracks']
];

describe('native engine: backend routes have a frontend API surface', () => {
	it.each(COVERAGE)('%s -> %s', (_label, actual, expected) => {
		expect(actual).toBe(expected);
	});

	it.each(PREFIX_COVERAGE)('%s -> starts with %s', (_label, actual, expectedPrefix) => {
		expect(actual.startsWith(expectedPrefix)).toBe(true);
	});

	it('exposes a builder for every native endpoint group', () => {
		expect(typeof API.downloadClient.config).toBe('function');
		expect(typeof API.downloads.searchAlbum).toBe('function');
		expect(typeof API.library.scanStart).toBe('function');
		expect(typeof API.tracks.request).toBe('function');
	});
});
