/**
 * Every native-engine backend route must have a matching entry in the central
 * `API` endpoint registry that queries/mutations consume; this proves coverage
 * and catches path drift between backend routes and frontend builders.
 *
 * `GET /downloads/{id}/files` has no dedicated builder by design - the file list
 * ships inside the task-detail response and the live SSE stream - so it is
 * intentionally excluded below.
 */
import { describe, expect, it } from 'vitest';

import { API } from '$lib/constants';

// [description, actual path produced by the API builder, expected backend route]
const COVERAGE: Array<[string, string, string]> = [
	['device session', API.auth.deviceSessions(), '/api/v1/auth/device-sessions'],
	// local password recovery
	[
		'password recovery reset',
		API.auth.passwordRecoveryReset(),
		'/api/v1/auth/password-recovery/reset'
	],
	[
		'admin password recovery code',
		API.auth.adminPasswordRecovery('U1'),
		'/api/v1/auth/admin/users/U1/password-recovery'
	],
	// download-client (admin)
	['download-client config', API.downloadClient.config(), '/api/v1/download-client/config'],
	['download-client status', API.downloadClient.status(), '/api/v1/download-client/status'],
	['download-client test', API.downloadClient.test(), '/api/v1/download-client/test'],
	// download tasks (user-scoped)
	['download detail', API.downloads.get('T1'), '/api/v1/downloads/T1'],
	['download stream', API.downloads.stream('T1'), '/api/v1/downloads/T1/stream'],
	['download cancel', API.downloads.cancel('T1'), '/api/v1/downloads/T1/cancel'],
	['download next source', API.downloads.nextSource('T1'), '/api/v1/downloads/T1/next-source'],
	['download retry', API.downloads.retry('T1'), '/api/v1/downloads/T1/retry'],
	['download reimport', API.downloads.reimport('T1'), '/api/v1/downloads/T1/reimport'],
	[
		'download restart with policy',
		API.downloads.restartWithPolicy('T1'),
		'/api/v1/downloads/T1/restart-with-current-policy'
	],
	// download-clients policy (Acquisition plan)
	[
		'download-client policy summary',
		API.downloadClients.policySummary(),
		'/api/v1/download-clients/policy-summary'
	],
	[
		'download-client policy impact',
		API.downloadClients.policyImpact(),
		'/api/v1/download-clients/policy/impact'
	],

	[
		'management hold retry',
		API.downloads.heldManagementRetry('T1'),
		'/api/v1/downloads/held/management/T1/retry'
	],
	[
		'management hold discard',
		API.downloads.heldManagementDiscard('T1'),
		'/api/v1/downloads/held/management/T1/discard'
	],
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
	[
		'library albums',
		API.library.albums(),
		'/api/v1/library/albums?page=1&page_size=50&sort=recent'
	],
	['library tracks', API.library.tracks(), '/api/v1/library/tracks?limit=48&offset=0&sort=recent'],
	[
		'library artists',
		API.library.artists(),
		'/api/v1/library/artists?limit=50&offset=0&sort_by=name&sort_order=asc&scope=album'
	],
	['library stats', API.library.stats(), '/api/v1/library/stats'],
	['library provider IDs', API.library.mbids(), '/api/v1/library/mbids'],
	['library membership', API.library.membership(), '/api/v1/library/membership'],
	['local track lyrics', API.local.lyrics('T1'), '/api/v1/local/tracks/T1/lyrics'],
	['recently added albums', API.library.recentlyAdded(), '/api/v1/library/recently-added?limit=20'],
	['local album detail', API.library.albumDetail('A1'), '/api/v1/library/albums/A1'],
	[
		'MusicBrainz edition search',
		API.library.reidentificationReleases('A1', 'Album', 'Artist'),
		'/api/v1/library/albums/A1/reidentification/releases?title=Album&artist=Artist&limit=12&offset=0'
	],
	[
		're-enable album management',
		API.library.reenableAlbumManagement('A1'),
		'/api/v1/library/albums/A1/management/re-enable'
	],
	[
		'edition conversion preflight',
		API.library.editionConversionPreflight('A1'),
		'/api/v1/library/albums/A1/edition-conversions/preflight'
	],
	[
		'edition conversion status',
		API.library.editionConversion('J1'),
		'/api/v1/library/edition-conversions/J1'
	],
	[
		'create edition conversion preview',
		API.library.editionConversionPreview('J1'),
		'/api/v1/library/edition-conversions/J1/preview'
	],
	[
		'start edition conversion',
		API.library.editionConversionStart('J1'),
		'/api/v1/library/edition-conversions/J1/start'
	],
	[
		'retry edition conversion',
		API.library.editionConversionRetry('J1'),
		'/api/v1/library/edition-conversions/J1/retry'
	],
	[
		'recheck edition conversion',
		API.library.editionConversionRecheck('J1'),
		'/api/v1/library/edition-conversions/J1/recheck'
	],
	[
		'cancel edition conversion',
		API.library.editionConversionCancel('J1'),
		'/api/v1/library/edition-conversions/J1/cancel'
	],
	[
		'exact MusicBrainz release artwork',
		API.library.exactReleaseArtwork('R1'),
		'/api/v1/covers/release/R1?size=250'
	],
	['local album copies', API.library.albumCopies('A1'), '/api/v1/library/albums/A1/copies'],
	['local artist detail', API.library.artistDetail('R1'), '/api/v1/library/artists/R1'],
	['local artist albums', API.library.artistAlbums('R1'), '/api/v1/library/artists/R1/albums'],
	[
		'local artist appearances',
		API.library.artistAppearances('R1'),
		'/api/v1/library/artists/R1/appearances?limit=20&offset=0'
	],
	['album status', API.library.album('M1'), '/api/v1/library/albums/M1/status'],
	['album tracks', API.library.albumTracks('M1'), '/api/v1/library/albums/M1/tracks'],
	[
		'create library contribution',
		API.library.createContribution('A1'),
		'/api/v1/library/albums/A1/contributions'
	],
	[
		'library contribution detail',
		API.library.contribution('C1'),
		'/api/v1/library/contributions/C1'
	],
	[
		'update library contribution draft',
		API.library.contributionDraft('C1'),
		'/api/v1/library/contributions/C1/draft'
	],
	[
		'rebuild library contribution',
		API.library.rebuildContribution('C1'),
		'/api/v1/library/contributions/C1/rebuild'
	],
	[
		'cancel library contribution',
		API.library.cancelContribution('C1'),
		'/api/v1/library/contributions/C1/cancel'
	],
	[
		'search Discogs releases',
		API.library.searchDiscogsReleases('C1'),
		'/api/v1/library/contributions/C1/discogs/search'
	],
	[
		'select Discogs release',
		API.library.selectDiscogsRelease('C1'),
		'/api/v1/library/contributions/C1/discogs/select'
	],
	[
		'remove Discogs release',
		API.library.removeDiscogsRelease('C1'),
		'/api/v1/library/contributions/C1/discogs/remove'
	],
	[
		'check MusicBrainz contribution duplicates',
		API.library.checkContributionDuplicates('C1'),
		'/api/v1/library/contributions/C1/musicbrainz/duplicates'
	],
	[
		'attach existing MusicBrainz release',
		API.library.attachContributionRelease('C1'),
		'/api/v1/library/contributions/C1/musicbrainz/attach'
	],
	[
		'open seeded MusicBrainz editor',
		API.library.createContributionSeed('C1'),
		'/api/v1/library/contributions/C1/musicbrainz/seed'
	],
	[
		'record returned MusicBrainz release',
		API.library.recordContributionResult('C1'),
		'/api/v1/library/contributions/C1/musicbrainz/result'
	],
	[
		'retry MusicBrainz contribution verification',
		API.library.retryContributionVerification('C1'),
		'/api/v1/library/contributions/C1/musicbrainz/verify'
	],
	[
		'cached local album artwork',
		API.library.cachedAlbumArtwork('A1', 3),
		'/api/v1/library/albums/A1/artwork/cached?v=3'
	],
	['resolve local tracks', API.library.resolveTracks(), '/api/v1/library/resolve-tracks'],
	// library admin (tags + scan control)
	['track tags', API.library.trackTags('F1'), '/api/v1/library/tracks/F1/tags'],
	['remove library track', API.library.removeTrack('F1'), '/api/v1/library/tracks/F1'],
	[
		'library management tag editor',
		API.libraryManagement.tagEditor('F1'),
		'/api/v1/library/management/tracks/F1/tag-editor'
	],
	[
		'library management tag edit previews',
		API.libraryManagement.tagEditPreviews(),
		'/api/v1/library/management/tag-edit-previews'
	],
	['remove library album', API.library.removeAlbum('M1'), '/api/v1/library/album/M1'],
	['rescan album', API.library.rescanAlbum('M1'), '/api/v1/library/albums/M1/rescan'],
	['library activity', API.library.activity(), '/api/v1/library/activity'],
	['library activity stream', API.library.activityStream(), '/api/v1/library/activity/stream'],
	[
		'library operations stream',
		API.library.operationsStream(),
		'/api/v1/library/operations/stream'
	],
	[
		'library management settings',
		API.libraryManagement.settings(),
		'/api/v1/settings/library-management'
	],
	[
		'update library management settings',
		API.libraryManagement.settings(),
		'/api/v1/settings/library-management'
	],
	[
		'library management settings impact',
		API.libraryManagement.impact(),
		'/api/v1/settings/library-management/impact'
	],
	[
		'validate library management settings',
		API.libraryManagement.validate(),
		'/api/v1/settings/library-management/validate'
	],
	[
		'create library management profile',
		API.libraryManagement.profiles(),
		'/api/v1/settings/library-management/profiles'
	],
	[
		'library management profile',
		API.libraryManagement.profile('P1'),
		'/api/v1/settings/library-management/profiles/P1'
	],
	[
		'update library management profile',
		API.libraryManagement.profile('P1'),
		'/api/v1/settings/library-management/profiles/P1'
	],
	[
		'delete library management profile',
		API.libraryManagement.profile('P1'),
		'/api/v1/settings/library-management/profiles/P1'
	],
	[
		'copy library management profile',
		API.libraryManagement.copyProfile('P1'),
		'/api/v1/settings/library-management/profiles/P1/copy'
	],
	[
		'export library management profile',
		API.libraryManagement.exportProfile('P1'),
		'/api/v1/settings/library-management/profiles/P1/export'
	],
	[
		'preview library management profile import',
		API.libraryManagement.profileImportPreview(),
		'/api/v1/settings/library-management/profile-imports/preview'
	],
	[
		'import library management profile',
		API.libraryManagement.profileImports(),
		'/api/v1/settings/library-management/profile-imports'
	],
	[
		'library management profile preset diff',
		API.libraryManagement.profilePresetDiff('P1'),
		'/api/v1/settings/library-management/profiles/P1/preset-diff'
	],
	[
		'create library management activation preview',
		API.libraryManagement.activationPreviews(),
		'/api/v1/settings/library-management/activation-previews'
	],
	[
		'library management activation preview status',
		API.libraryManagement.activationPreview('J1'),
		'/api/v1/settings/library-management/activation-previews/J1'
	],
	[
		'confirm library management activation',
		API.libraryManagement.activationConfirmations(),
		'/api/v1/settings/library-management/activation-confirmations'
	],
	[
		'create library management preview',
		API.libraryManagement.previews(),
		'/api/v1/library/management/previews'
	],
	[
		'create library management baseline restore preview',
		API.libraryManagement.baselineRestorePreviews(),
		'/api/v1/library/management/baselines/restore-previews'
	],
	[
		'create library management duplicate resolution preview',
		API.libraryManagement.duplicateResolutionPreviews(),
		'/api/v1/library/management/duplicate-resolution-previews'
	],
	[
		'preview library management baseline purge impact',
		API.libraryManagement.baselinePurgeImpact(),
		'/api/v1/library/management/baselines/purge-impact'
	],
	[
		'purge library management baselines',
		API.libraryManagement.purgeBaselines(),
		'/api/v1/library/management/baselines/purge'
	],
	[
		'library management preview detail',
		API.libraryManagement.preview('J1'),
		'/api/v1/library/management/previews/J1'
	],
	[
		'library management preview items',
		API.libraryManagement.previewItems('J1'),
		'/api/v1/library/management/previews/J1/items'
	],
	[
		'library management preview artwork',
		API.libraryManagement.previewArtwork('J1', 2, 'abc123'),
		'/api/v1/library/management/previews/J1/items/2/artwork/abc123'
	],
	[
		'apply library management preview',
		API.libraryManagement.applyPreview('J1'),
		'/api/v1/library/management/previews/J1/apply'
	],
	[
		'discard library management preview',
		API.libraryManagement.discardPreview('J1'),
		'/api/v1/library/management/previews/J1/discard'
	],
	[
		'library management operation history',
		API.libraryManagement.operations(),
		'/api/v1/library/management/operations'
	],
	[
		'library management operation detail',
		API.libraryManagement.operation('J1'),
		'/api/v1/library/management/operations/J1'
	],
	[
		'library management undo preview creation',
		API.libraryManagement.undoPreview('J1'),
		'/api/v1/library/management/operations/J1/undo-preview'
	],
	[
		'library management operation results',
		API.libraryManagement.operationResults('J1'),
		'/api/v1/library/management/operations/J1/results'
	],
	[
		'library management recovery diagnostics',
		API.libraryManagement.recoveryDiagnostics(),
		'/api/v1/library/management/recovery/diagnostics'
	],
	[
		'pause identification',
		API.library.pauseIdentification(),
		'/api/v1/library/identification/pause'
	],
	[
		'resume identification',
		API.library.resumeIdentification(),
		'/api/v1/library/identification/resume'
	],
	['scan runs', API.library.scanRuns(), '/api/v1/library/scan-runs'],
	['current scan runs', API.library.currentScanRuns(), '/api/v1/library/scan-runs/current'],
	['scan run estimate', API.library.scanRunEstimate(), '/api/v1/library/scan-runs/estimate'],
	['scan run detail', API.library.scanRun('R1'), '/api/v1/library/scan-runs/R1'],
	['scan run failures', API.library.scanRunFailures('R1'), '/api/v1/library/scan-runs/R1/failures'],
	['pause scan run', API.library.pauseScanRun('R1'), '/api/v1/library/scan-runs/R1/pause'],
	['resume scan run', API.library.resumeScanRun('R1'), '/api/v1/library/scan-runs/R1/resume'],
	['stop scan run', API.library.stopScanRun('R1'), '/api/v1/library/scan-runs/R1/stop'],
	['library reviews', API.library.reviews(), '/api/v1/library/reviews'],
	['library review detail', API.library.review('V1'), '/api/v1/library/reviews/V1'],
	[
		'keep review tagged',
		API.library.reviewKeepTagged('V1'),
		'/api/v1/library/reviews/V1/keep-tagged'
	],
	[
		'detach and keep review tagged',
		API.library.reviewDetachKeepTagged('V1'),
		'/api/v1/library/reviews/V1/detach-and-keep-tagged'
	],
	['exclude review', API.library.reviewExclude('V1'), '/api/v1/library/reviews/V1/exclude'],
	['restore review', API.library.reviewRestore('V1'), '/api/v1/library/reviews/V1/restore'],
	[
		'accept review candidate',
		API.library.reviewCandidate('V1'),
		'/api/v1/library/reviews/V1/candidate'
	],
	['retry review', API.library.reviewRetry('V1'), '/api/v1/library/reviews/V1/retry'],
	['bulk review preview', API.library.bulkReviewPreview(), '/api/v1/library/reviews/bulk-preview'],
	['bulk review apply', API.library.bulkReviewApply(), '/api/v1/library/reviews/bulk-apply'],
	['library operation', API.library.operation('J1'), '/api/v1/library/operations/J1'],
	[
		'pause library operation',
		API.library.pauseOperation('J1'),
		'/api/v1/library/operations/J1/pause'
	],
	[
		'resume library operation',
		API.library.resumeOperation('J1'),
		'/api/v1/library/operations/J1/resume'
	],
	['stop library operation', API.library.stopOperation('J1'), '/api/v1/library/operations/J1/stop'],
	[
		'select operation candidate',
		API.library.operationCandidate('J1'),
		'/api/v1/library/operations/J1/candidate'
	],
	['reidentify album', API.library.reidentifyAlbum('A1'), '/api/v1/library/albums/A1/reidentify'],
	[
		'preview album split',
		API.library.previewAlbumSplit('A1'),
		'/api/v1/library/albums/A1/split-preview'
	],
	['split album', API.library.splitAlbum('A1'), '/api/v1/library/albums/A1/split'],
	['preview album merge', API.library.previewAlbumMerge(), '/api/v1/library/albums/merge-preview'],
	['merge albums', API.library.mergeAlbums(), '/api/v1/library/albums/merge'],
	['preview track move', API.library.previewTrackMove(), '/api/v1/library/tracks/move-preview'],
	['move tracks', API.library.moveTracks(), '/api/v1/library/tracks/move'],
	[
		'preview reset album grouping',
		API.library.previewResetAlbumGrouping('A1'),
		'/api/v1/library/albums/A1/reset-grouping-preview'
	],
	[
		'reset album grouping',
		API.library.resetAlbumGrouping('A1'),
		'/api/v1/library/albums/A1/reset-grouping'
	],
	[
		'preview artist merge',
		API.library.previewArtistMerge(),
		'/api/v1/library/artists/merge-preview'
	],
	['merge artists', API.library.mergeArtists(), '/api/v1/library/artists/merge'],
	[
		'artist reconciliation progress',
		API.library.artistReconciliation(),
		'/api/v1/library/artists/reconciliation'
	],
	[
		'artist duplicate groups',
		API.library.artistDuplicateGroups(),
		'/api/v1/library/artists/duplicate-groups'
	],
	[
		'artist duplicate group',
		API.library.artistDuplicateGroup('G1'),
		'/api/v1/library/artists/duplicate-groups/G1'
	],
	[
		'dismiss artist duplicate group',
		API.library.dismissArtistDuplicateGroup('G1'),
		'/api/v1/library/artists/duplicate-groups/G1/dismiss'
	],
	['identity repairs', API.library.identityRepairs(), '/api/v1/library/identity-repairs'],
	[
		'identity repair estimate',
		API.library.identityRepairEstimate([]),
		'/api/v1/library/identity-repairs/estimate'
	],
	['identity repair', API.library.identityRepair('J1'), '/api/v1/library/identity-repairs/J1'],
	[
		'identity repair findings',
		API.library.identityRepairFindings('J1'),
		'/api/v1/library/identity-repairs/J1/findings'
	],
	[
		'apply identity repair',
		API.library.applyIdentityRepair('J1'),
		'/api/v1/library/identity-repairs/J1/apply'
	],
	[
		'pause identity repair',
		API.library.pauseIdentityRepair('J1'),
		'/api/v1/library/identity-repairs/J1/pause'
	],
	[
		'resume identity repair',
		API.library.resumeIdentityRepair('J1'),
		'/api/v1/library/identity-repairs/J1/resume'
	],
	[
		'stop identity repair',
		API.library.stopIdentityRepair('J1'),
		'/api/v1/library/identity-repairs/J1/stop'
	],
	[
		'identity preparations',
		API.library.identityPreparations(),
		'/api/v1/library/management/identity-preparations'
	],
	[
		'identity preparation estimate',
		API.library.identityPreparationEstimate([]),
		'/api/v1/library/management/identity-preparations/estimate'
	],
	[
		'identity preparation',
		API.library.identityPreparation('J1'),
		'/api/v1/library/management/identity-preparations/J1'
	],
	[
		'identity preparation findings',
		API.library.identityPreparationFindings('J1'),
		'/api/v1/library/management/identity-preparations/J1/findings'
	],
	[
		'apply identity preparation',
		API.library.applyIdentityPreparation('J1'),
		'/api/v1/library/management/identity-preparations/J1/apply'
	],
	[
		'discard identity preparation',
		API.library.discardIdentityPreparation('J1'),
		'/api/v1/library/management/identity-preparations/J1/discard'
	],
	[
		'undo automatic edition',
		API.library.undoAutomaticEdition('A1'),
		'/api/v1/library/albums/A1/undo-automatic-edition'
	],
	[
		'scan diagnostics',
		API.library.scanDiagnostics('R1'),
		'/api/v1/library/scan-runs/R1/diagnostics'
	],
	['typed library settings', API.library.typedSettings(), '/api/v1/settings/library/roots'],
	['target library settings', API.library.settings(), '/api/v1/settings/library'],
	['library policy tree', API.library.policyTree(), '/api/v1/settings/library/policy-tree'],
	['library policy impact', API.library.policyImpact(), '/api/v1/settings/library/policy-impact'],
	[
		'library policy apply preview',
		API.library.policyApplyPreview(),
		'/api/v1/settings/library/policy-apply-preview'
	],
	['library path mapping', API.library.pathMapping(), '/api/v1/settings/library/path-mapping'],
	[
		'library restorable roots',
		API.library.restorableRoots(),
		'/api/v1/settings/library/restorable-roots'
	],
	['library restore roots', API.library.restoreRoots(), '/api/v1/settings/library/restore-roots'],
	// per-user section visibility (user-scoped)
	['section prefs', API.me.sectionPrefs(), '/api/v1/me/section-prefs'],
	['home integration status', API.homeIntegrationStatus(), '/api/v1/home/integration-status'],
	[
		'genre detail',
		API.homeGenre('Latin'),
		'/api/v1/home/genre/Latin?limit=50&artist_offset=0&album_offset=0'
	],
	// external-service health for the header status indicator
	['system health', API.system.health(), '/api/v1/system/health'],
	// QW9 runtime observability (admin diagnostics card)
	['system queue stats', API.system.queueStats(), '/api/v1/system/queue-stats'],
	['system provider stats', API.system.providerStats(), '/api/v1/system/provider-stats'],
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
	// Media-server account links (per-user playback attribution, issue #138)
	['connect navidrome', API.me.navidrome(), '/api/v1/me/connections/navidrome'],
	[
		'navidrome music folder preferences',
		API.me.navidromeMusicFolderPreferences(),
		'/api/v1/me/navidrome/music-folder-preferences'
	],
	['connect jellyfin', API.me.jellyfin(), '/api/v1/me/connections/jellyfin'],
	['plex link pin', API.me.plexAuthPin(), '/api/v1/me/connections/plex/auth/pin'],
	['plex link poll', API.me.plexAuthPoll(7), '/api/v1/me/connections/plex/auth/poll?pin_id=7'],
	[
		'Jellyfin user playlist image',
		API.jellyfinLibrary.playlistImage('P1', 'I1', 300),
		'/api/v1/jellyfin/playlist-image/P1/I1?size=300'
	],
	[
		'Navidrome user playlist cover',
		API.navidromeLibrary.playlistCover('P1', 'C1', 300),
		'/api/v1/navidrome/playlist-cover/P1/C1?size=300'
	],
	[
		'Plex user playlist image',
		API.plexLibrary.playlistImage('P1', 'I1', 300),
		'/api/v1/plex/playlist-image/P1/I1?size=300'
	],
	// Request page reads and actions (album default plus exact-track variants)
	['active requests', API.requests.active(), '/api/v1/requests/active'],
	['request history', API.requests.history(), '/api/v1/requests/history?page=1&page_size=20'],
	[
		'pending request approvals',
		API.requests.pendingApprovals(),
		'/api/v1/requests/pending-approvals'
	],
	[
		'cancel request (album)',
		API.requests.cancel('M1'),
		'/api/v1/requests/active/M1?request_kind=album'
	],
	[
		'cancel request (track)',
		API.requests.cancel('M/1', 'track'),
		'/api/v1/requests/active/M%2F1?request_kind=track'
	],
	[
		'retry request (album)',
		API.requests.retry('M1'),
		'/api/v1/requests/retry/M1?request_kind=album'
	],
	[
		'retry request (track)',
		API.requests.retry('M/1', 'track'),
		'/api/v1/requests/retry/M%2F1?request_kind=track'
	],
	[
		'clear request history (album)',
		API.requests.clearHistoryItem('M1'),
		'/api/v1/requests/history/M1?request_kind=album'
	],
	[
		'clear request history (track)',
		API.requests.clearHistoryItem('M/1', 'track'),
		'/api/v1/requests/history/M%2F1?request_kind=track'
	],
	[
		'approve request (album)',
		API.requests.approve('M1'),
		'/api/v1/requests/approve/M1?request_kind=album'
	],
	[
		'approve request (track)',
		API.requests.approve('M/1', 'track'),
		'/api/v1/requests/approve/M%2F1?request_kind=track'
	],
	[
		'reject request (album)',
		API.requests.reject('M1'),
		'/api/v1/requests/reject/M1?request_kind=album'
	],
	[
		'reject request (track)',
		API.requests.reject('M/1', 'track'),
		'/api/v1/requests/reject/M%2F1?request_kind=track'
	],
	// Weekly Mix (user-scoped refresh + admin standing-grant queue)
	['personal mix refresh', API.me.personalMixRefresh(), '/api/v1/me/personal-mix/refresh'],
	[
		'personal mix approvals',
		API.requests.personalMixApprovals(),
		'/api/v1/requests/personal-mix-approvals'
	],
	[
		'pending approval count',
		API.requests.pendingApprovalCount(),
		'/api/v1/requests/pending-approvals/count'
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
	// Get it (phase 01): the lazy Where-to-buy endpoint + the admin settings card
	['album purchase options', API.album.purchaseOptions('M1'), '/api/v1/albums/M1/purchase-options'],
	[
		'artist purchase options',
		API.artist.purchaseOptions('A1', 'Band'),
		'/api/v1/artists/A1/purchase-options?name=Band'
	],
	['get-it settings', API.settingsGetIt(), '/api/v1/settings/get-it'],
	['free-music settings', API.settingsFreeMusic(), '/api/v1/settings/free-music'],
	// MusicBrainz settings: staged source selection, consent, verification, and activation
	['MusicBrainz settings', API.settingsMusicbrainz(), '/api/v1/settings/musicbrainz'],
	[
		'BrainzMash stage',
		API.settingsMusicbrainzBrainzMashStage(),
		'/api/v1/settings/musicbrainz/brainzmash/stage'
	],
	[
		'BrainzMash consent',
		API.settingsMusicbrainzBrainzMashConsent(),
		'/api/v1/settings/musicbrainz/brainzmash/consent'
	],
	[
		'MusicBrainz verification',
		API.settingsMusicbrainzVerify(),
		'/api/v1/settings/musicbrainz/verify'
	],
	[
		'BrainzMash activation',
		API.settingsMusicbrainzActivate(),
		'/api/v1/settings/musicbrainz/activate'
	],
	// Plugin API (phase 01b): admin roster + curator source surfaces
	['plugins list', API.plugins.list(), '/api/v1/plugins'],
	['plugin install', API.plugins.install(), '/api/v1/plugins/install'],
	['plugin update', API.plugins.update('P1'), '/api/v1/plugins/P1'],
	['plugin uninstall', API.plugins.uninstall('P1'), '/api/v1/plugins/P1'],
	// Drop importer (Store Sync 01c): curator-gated upload/jobs/match/discard
	['drop-import uploads', API.dropImport.uploads(), '/api/v1/import/uploads'],
	['drop-import jobs', API.dropImport.jobs(), '/api/v1/import/jobs'],
	['drop-import jobs (all)', API.dropImport.jobs(true), '/api/v1/import/jobs?all=true'],
	['drop-import job', API.dropImport.job('J1'), '/api/v1/import/jobs/J1'],
	['drop-import match', API.dropImport.match(7), '/api/v1/import/items/7/match'],
	['drop-import discard', API.dropImport.discard(7), '/api/v1/import/items/7/discard'],
	// Free Music (D24): the native lawful download client
	['free-music tasks', API.freeMusic.tasks(), '/api/v1/free-music/tasks'],
	['free-music tasks (all)', API.freeMusic.tasks(true), '/api/v1/free-music/tasks?all=true'],
	['free-music task', API.freeMusic.task('T1'), '/api/v1/free-music/tasks/T1'],
	['free-music remove', API.freeMusic.remove('T1'), '/api/v1/free-music/tasks/T1'],
	['free-music clear history', API.freeMusic.clearHistory(), '/api/v1/free-music/tasks'],
	[
		'free-music clear all history',
		API.freeMusic.clearHistory(true),
		'/api/v1/free-music/tasks?all=true'
	],
	['free-music cancel', API.freeMusic.cancel('T1'), '/api/v1/free-music/tasks/T1/cancel'],
	['free-music retry', API.freeMusic.retry('T1'), '/api/v1/free-music/tasks/T1/retry'],
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
	],
	// Connect Apps admin oversight (see/revoke every user's app-passwords)
	[
		'admin app-password roster',
		API.connectApps.adminAppPasswords(),
		'/api/v1/connect-apps/admin/app-passwords'
	],
	[
		'admin app-password revoke',
		API.connectApps.adminAppPassword('ap-1'),
		'/api/v1/connect-apps/admin/app-passwords/ap-1'
	]
];

// Routes whose builder takes query params - assert the path prefix only.
const PREFIX_COVERAGE: Array<[string, string, string]> = [
	['downloads list', API.downloads.list(), '/api/v1/downloads'],
	[
		'downloads activity summary',
		API.downloads.activitySummary(),
		'/api/v1/downloads/activity-summary'
	],
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

	it('encodes the bounded management preview cursor and filters', () => {
		expect(
			API.libraryManagement.previewItems('job/1', {
				afterOrdinal: 9,
				limit: 25,
				eligibility: 'warning',
				reasonCode: 'OPTIONAL ENRICHMENT',
				rootId: 'root/1',
				artistId: 'artist/1',
				albumId: 'album/1',
				audioFormat: 'flac',
				collisionClass: 'normalized_path_collision',
				hasPreservedValue: true,
				hasRepresentationLoss: true,
				changeKind: 'tags'
			})
		).toBe(
			'/api/v1/library/management/previews/job%2F1/items?after_ordinal=9&limit=25&eligibility=warning&reason_code=OPTIONAL+ENRICHMENT&root_id=root%2F1&artist_id=artist%2F1&album_id=album%2F1&audio_format=flac&collision_class=normalized_path_collision&has_preserved_value=true&has_representation_loss=true&change_kind=tags'
		);
	});

	it('encodes management history filters and opaque cursors', () => {
		expect(
			API.libraryManagement.operations({
				limit: 20,
				cursor: 'opaque cursor',
				origin: 'manual',
				profileId: 'profile/1',
				rootId: 'root/1',
				state: 'succeeded',
				mode: 'duplicate_resolution',
				createdFrom: 10,
				createdTo: 20
			})
		).toBe(
			'/api/v1/library/management/operations?limit=20&cursor=opaque+cursor&origin=manual&profile_id=profile%2F1&root_id=root%2F1&state=succeeded&mode=duplicate_resolution&created_from=10&created_to=20'
		);
	});

	it('exposes a builder for every native endpoint group', () => {
		expect(typeof API.downloadClient.config).toBe('function');
		expect(typeof API.downloads.searchAlbum).toBe('function');
		expect(typeof API.tracks.request).toBe('function');
	});
});
