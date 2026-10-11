import { v3 } from '$lib/api/v3/endpoint';
import type {
	LibraryReviewParams,
	LibraryV3AlbumsParams,
	LibraryV3ArtistsParams,
	LibraryV3PageParams,
	LibraryV3TracksParams
} from './LibraryQueryKeyFactory';

/** A review list filter as the server's query fields (paging aside). */
export function reviewFilterQuery(params: Omit<LibraryReviewParams, 'limit' | 'offset'>) {
	return {
		...(params.search ? { search: params.search } : {}),
		...(params.reasonCode ? { reason_code: params.reasonCode } : {}),
		...(params.rootId ? { root_id: params.rootId } : {}),
		...(params.policy ? { policy: params.policy } : {}),
		...(params.jobState ? { job_state: params.jobState } : {}),
		...(params.hideActiveJobs ? { exclude_active_jobs: true } : {}),
		...(params.createdFrom !== undefined ? { created_from: params.createdFrom } : {}),
		...(params.createdTo !== undefined ? { created_to: params.createdTo } : {}),
		...(params.sort ? { sort: params.sort } : {})
	};
}

// /api/v3 library URLs, built through the typed registry: every template is
// a literal the contract-coverage gate verifies against the generated spec.
// Nothing outside this feature imports them.
export const LibraryV3Api = {
	removeAlbum: (albumId: string, stopWanted: boolean) =>
		v3('/api/v3/library/albums/{id}', {
			path: { id: albumId },
			query: { delete_files: true, stop_wanted: stopWanted }
		}),
	removeTrack: (trackId: string) => v3('/api/v3/library/tracks/{id}', { path: { id: trackId } }),
	rescanAlbum: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/rescan', { path: { album_id: albumId } }),
	reenableManagement: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/management/re-enable', {
			path: { album_id: albumId }
		}),
	albums: (params: LibraryV3AlbumsParams) =>
		v3('/api/v3/library/albums', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.artistId ? { artist_id: params.artistId } : {}),
				...(params.decade !== undefined ? { decade: params.decade } : {}),
				...(params.format ? { format: params.format } : {})
			}
		}),
	artists: (params: LibraryV3ArtistsParams) =>
		v3('/api/v3/library/artists', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.scope ? { scope: params.scope } : {})
			}
		}),
	albumDetail: (albumId: string) => v3('/api/v3/library/albums/{id}', { path: { id: albumId } }),
	albumStatus: (albumId: string) =>
		v3('/api/v3/library/albums/{id}/status', { path: { id: albumId } }),
	membership: () => v3('/api/v3/library/membership'),
	resolveTracks: () => v3('/api/v3/library/resolve-tracks'),
	// The album's edition: read, choose, hand back, confirm, undo.
	edition: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition', { path: { album_id: albumId } }),
	editionConfirm: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition/confirm', { path: { album_id: albumId } }),
	editionUndo: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition/undo', { path: { album_id: albumId } }),
	editionExtrasRemove: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition/extras/remove', {
			path: { album_id: albumId }
		}),
	editionExtrasRestore: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition/extras/restore', {
			path: { album_id: albumId }
		}),
	// What taking an edition would do to the files; the current one without a release.
	editionPlan: (albumId: string, releaseMbid?: string) =>
		v3('/api/v3/library/albums/{album_id}/edition/plan', {
			path: { album_id: albumId },
			query: releaseMbid ? { release_mbid: releaseMbid } : {}
		}),
	unconfirmed: (state: 'unconfirmed' | 'unmatched', limit: number, offset: number) =>
		v3('/api/v3/library/unconfirmed', { query: { state, limit, offset } }),
	managePreview: () => v3('/api/v3/library/manage/preview'),
	manageApply: () => v3('/api/v3/library/manage/apply'),
	albumCopies: (albumId: string) =>
		v3('/api/v3/library/albums/{id}/copies', { path: { id: albumId } }),
	albumTracks: (albumId: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/albums/{id}/tracks', {
			path: { id: albumId },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	artistDetail: (artistId: string) =>
		v3('/api/v3/library/artists/{id}', { path: { id: artistId } }),
	artistAlbums: (artistId: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/artists/{id}/albums', {
			path: { id: artistId },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	artistAppearances: (artistId: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/artists/{id}/appearances', {
			path: { id: artistId },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	tracks: (params: LibraryV3TracksParams) =>
		v3('/api/v3/library/tracks', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.albumId ? { album_id: params.albumId } : {}),
				...(params.artistId ? { artist_id: params.artistId } : {}),
				...(params.genre ? { genre: params.genre } : {})
			}
		}),
	lyrics: (trackId: string) => v3('/api/v3/library/tracks/{id}/lyrics', { path: { id: trackId } }),
	recentlyAdded: (limit: number) => v3('/api/v3/library/recently-added', { query: { limit } }),
	stats: () => v3('/api/v3/library/stats'),
	// Identification reviews: list, read, and every way to settle one.
	reviews: (params: LibraryReviewParams) =>
		v3('/api/v3/library/reviews', {
			query: {
				state: params.state ?? 'pending',
				limit: params.limit,
				offset: params.offset,
				...reviewFilterQuery(params)
			}
		}),
	review: (reviewId: string) => v3('/api/v3/library/reviews/{id}', { path: { id: reviewId } }),
	dismissReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/dismiss', { path: { id: reviewId } }),
	detachReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/detach', { path: { id: reviewId } }),
	excludeReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/exclude', { path: { id: reviewId } }),
	restoreReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/restore', { path: { id: reviewId } }),
	retryReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/retry', { path: { id: reviewId } }),
	retryPendingReviews: () => v3('/api/v3/library/reviews/retry-pending'),
	bulkReviewPreview: () => v3('/api/v3/library/reviews/bulk-preview'),
	bulkReviewApply: () => v3('/api/v3/library/reviews/bulk-apply'),
	approveReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/approve', { path: { id: reviewId } }),
	rejectReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/reject', { path: { id: reviewId } }),
	operation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}', { path: { job_id: jobId } }),
	pauseOperation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/pause', { path: { job_id: jobId } }),
	resumeOperation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/resume', { path: { job_id: jobId } }),
	stopOperation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/stop', { path: { job_id: jobId } }),
	operationCandidate: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/candidate', { path: { job_id: jobId } }),
	reidentifyAlbum: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/reidentify', { path: { album_id: albumId } }),
	reidentificationReleases: (
		albumId: string,
		params: {
			title: string;
			artist: string;
			release_group_mbid?: string;
			limit: number;
			offset: number;
		}
	) =>
		v3('/api/v3/library/albums/{album_id}/reidentification/releases', {
			path: { album_id: albumId },
			query: params
		}),
	undoAutomaticEdition: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/undo-automatic-edition', {
			path: { album_id: albumId }
		}),
	// Catalog corrections: each change is previewed, then applied with the
	// preview's token.
	splitPreview: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/split-preview', { path: { album_id: albumId } }),
	split: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/split', { path: { album_id: albumId } }),
	mergePreview: () => v3('/api/v3/library/albums/merge-preview'),
	merge: () => v3('/api/v3/library/albums/merge'),
	movePreview: () => v3('/api/v3/library/tracks/move-preview'),
	move: () => v3('/api/v3/library/tracks/move'),
	resetGroupingPreview: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/reset-grouping-preview', {
			path: { album_id: albumId }
		}),
	resetGrouping: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/reset-grouping', { path: { album_id: albumId } }),
	artistMergePreview: () => v3('/api/v3/library/artists/merge-preview'),
	artistMerge: () => v3('/api/v3/library/artists/merge'),
	scanRuns: () => v3('/api/v3/library/scan/runs'),
	scanRun: (runId: string) => v3('/api/v3/library/scan/runs/{id}', { path: { id: runId } }),
	currentScanRuns: () => v3('/api/v3/library/scan/runs/current'),
	scanRunHistory: (limit: number, cursor?: string) =>
		v3('/api/v3/library/scan/runs/history', {
			query: { limit, ...(cursor ? { cursor } : {}) }
		}),
	scanRunEstimate: (scopeIds: string[]) =>
		v3('/api/v3/library/scan/runs/estimate', {
			query: scopeIds.length ? { scope_ids: scopeIds } : {}
		}),
	scanRunFailures: (runId: string, limit: number, cursor?: number) =>
		v3('/api/v3/library/scan/runs/{id}/failures', {
			path: { id: runId },
			query: { limit, ...(cursor !== undefined ? { cursor } : {}) }
		}),
	scanDiagnostics: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/diagnostics', { path: { id: runId } }),
	// Identity repairs: recheck existing matches, then apply safe detaches.
	identityRepairs: (limit?: number, cursor?: string) =>
		v3('/api/v3/library/identity-repairs', {
			query: { ...(limit !== undefined ? { limit } : {}), ...(cursor ? { cursor } : {}) }
		}),
	identityRepairEstimate: (rootIds: string[]) =>
		v3('/api/v3/library/identity-repairs/estimate', {
			query: rootIds.length ? { root_id: rootIds } : {}
		}),
	identityRepair: (jobId: string) =>
		v3('/api/v3/library/identity-repairs/{job_id}', { path: { job_id: jobId } }),
	identityRepairFindings: (jobId: string, limit: number, cursor?: string, category?: string) =>
		v3('/api/v3/library/identity-repairs/{job_id}/findings', {
			path: { job_id: jobId },
			query: {
				limit,
				...(cursor ? { cursor } : {}),
				...(category ? { finding_category: category } : {})
			}
		}),
	applyIdentityRepair: (jobId: string) =>
		v3('/api/v3/library/identity-repairs/{job_id}/apply', { path: { job_id: jobId } }),
	// Identity preparations: get albums ready for Library Management.
	identityPreparations: (limit?: number, cursor?: string) =>
		v3('/api/v3/library/management/identity-preparations', {
			query: { ...(limit !== undefined ? { limit } : {}), ...(cursor ? { cursor } : {}) }
		}),
	identityPreparationEstimate: (rootIds: string[]) =>
		v3('/api/v3/library/management/identity-preparations/estimate', {
			query: rootIds.length ? { root_id: rootIds } : {}
		}),
	identityPreparationFindings: (jobId: string, limit: number, cursor?: string, category?: string) =>
		v3('/api/v3/library/management/identity-preparations/{job_id}/findings', {
			path: { job_id: jobId },
			query: {
				limit,
				...(cursor ? { cursor } : {}),
				...(category ? { finding_category: category } : {})
			}
		}),
	applyIdentityPreparation: (jobId: string) =>
		v3('/api/v3/library/management/identity-preparations/{job_id}/apply', {
			path: { job_id: jobId }
		}),
	discardIdentityPreparation: (jobId: string) =>
		v3('/api/v3/library/management/identity-preparations/{job_id}/discard', {
			path: { job_id: jobId }
		}),
	pauseScanRun: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/pause', { path: { id: runId } }),
	resumeScanRun: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/resume', { path: { id: runId } }),
	stopScanRun: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/stop', { path: { id: runId } }),
	activity: () => v3('/api/v3/library/activity'),
	pauseIdentification: () => v3('/api/v3/library/identification/pause'),
	resumeIdentification: () => v3('/api/v3/library/identification/resume'),
	schedule: () => v3('/api/v3/settings/library/schedule'),
	settings: () => v3('/api/v3/settings/library'),
	policyTree: () => v3('/api/v3/settings/library/policy-tree'),
	policyImpact: () => v3('/api/v3/settings/library/policy-impact'),
	policyApplyPreview: () => v3('/api/v3/settings/library/policy-apply-preview'),
	pathMapping: () => v3('/api/v3/settings/library/path-mapping'),
	restorableRoots: () => v3('/api/v3/settings/library/restorable-roots'),
	restoreRoots: () => v3('/api/v3/settings/library/restore-roots')
};
