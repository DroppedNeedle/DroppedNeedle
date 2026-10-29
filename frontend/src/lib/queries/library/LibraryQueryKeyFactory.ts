import type { components } from '$lib/api/v3/openapi';
import { ADMIN, key, userScope } from '../keys';

export type LibraryV3UserId = string | null | undefined;

// A long list's params, without its cursor: a list's pages share one key.
export interface LibraryV3AlbumsParams {
	limit: number;
	sort?: 'name' | 'date_added' | 'year' | 'artist' | 'random' | 'rediscover';
	order?: 'asc' | 'desc';
	q?: string;
	artistId?: string;
	decade?: number;
	format?: string;
	/** Only the caller's favourites; without a sort, the newest first. */
	favorites?: boolean;
}

export interface LibraryV3ArtistsParams {
	limit: number;
	sort?: 'name' | 'album_count' | 'appearance_count' | 'date_added';
	order?: 'asc' | 'desc';
	q?: string;
	scope?: 'all' | 'album_artists' | 'contributors';
	/** Only the caller's favourites; without a sort, the newest first. */
	favorites?: boolean;
}

export interface LibraryV3TracksParams {
	limit: number;
	sort?: 'title' | 'date_added' | 'artist' | 'album' | 'random';
	order?: 'asc' | 'desc';
	q?: string;
	albumId?: string;
	artistId?: string;
	genre?: string;
	/** Only the caller's favourites; without a sort, the newest first. */
	favorites?: boolean;
}

/** A stored review state, `resolved` (approved or dismissed) or `all`. */
export type LibraryReviewState =
	'pending' | 'approved' | 'rejected' | 'dismissed' | 'excluded' | 'resolved' | 'all';

export type LibraryReviewSort = components['schemas']['ReviewSort'];

export interface LibraryReviewParams {
	limit: number;
	state?: LibraryReviewState;
	search?: string;
	reasonCode?: string;
	rootId?: string;
	policy?: 'automatic' | 'local_metadata' | 'excluded';
	jobState?: 'queued' | 'running' | 'deferred' | 'succeeded' | 'failed' | 'attention';
	hideActiveJobs?: boolean;
	/** Filed on or after / before these times (Unix milliseconds). */
	createdFrom?: number;
	createdTo?: number;
	withCandidates?: boolean;
	sort?: LibraryReviewSort;
}

export interface LibraryV3PageParams {
	limit?: number;
	offset?: number;
}

/** A page of a list read by cursor: the size, and the `next` it follows. */
export interface LibraryV3CursorParams {
	limit?: number;
	after?: string | null;
}

const operations = () => key(ADMIN, 'library', 'operations');
const reviews = () => key(ADMIN, 'library', 'reviews');
const policy = () => key(ADMIN, 'library', 'policy');

export const LibraryQueryKeyFactory = {
	feature: 'library',
	// Catalog keys. Album and artist views carry per-caller favorite flags, so
	// every catalog key is the caller's.
	catalog: {
		root: (userId: LibraryV3UserId) => key(userScope(userId), 'library', 'catalog'),
		albums: (userId: LibraryV3UserId, params: LibraryV3AlbumsParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'albums', params] as const,
		// The album list's pages, read on by cursor: apart from one page's key.
		albumPages: (userId: LibraryV3UserId, params: LibraryV3AlbumsParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-pages', params] as const,
		artists: (userId: LibraryV3UserId, params: LibraryV3ArtistsParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artists', params] as const,
		tracks: (userId: LibraryV3UserId, params: LibraryV3TracksParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'tracks', params] as const,
		artistThumbs: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artist-thumbs'] as const,
		albumDetail: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-detail', albumId] as const,
		albumCopies: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-copies', albumId] as const,
		albumTracks: (userId: LibraryV3UserId, albumId: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-tracks', albumId, params] as const,
		artistDetail: (userId: LibraryV3UserId, artistId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artist-detail', artistId] as const,
		artistAlbums: (userId: LibraryV3UserId, artistId: string, params: LibraryV3CursorParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artist-albums', artistId, params] as const,
		artistAppearances: (userId: LibraryV3UserId, artistId: string, params: LibraryV3CursorParams) =>
			[
				...LibraryQueryKeyFactory.catalog.root(userId),
				'artist-appearances',
				artistId,
				params
			] as const,
		recentlyAddedPrefix: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'recently-added'] as const,
		recentlyAdded: (userId: LibraryV3UserId, limit: number) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'recently-added', limit] as const,
		resolvedTracks: (userId: LibraryV3UserId, items: readonly unknown[]) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'resolved-tracks', items] as const,
		search: (userId: LibraryV3UserId, q: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'search', q] as const,
		albumSearch: (userId: LibraryV3UserId, q: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-search', q] as const,
		stats: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'stats'] as const,
		edition: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'edition', albumId] as const,
		editionPlanPrefix: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.edition(userId, albumId), 'plan'] as const,
		// A null release is the album's own edition, the plan the album page shows.
		editionPlan: (userId: LibraryV3UserId, albumId: string, release: string | null) =>
			[...LibraryQueryKeyFactory.catalog.editionPlanPrefix(userId, albumId), release] as const,
		// One tag write of the edition's tags, or its undo, by job. Outside the
		// edition prefix: a finished write refreshes that prefix, which must
		// not cancel the read that saw it finish.
		editionTagWrite: (userId: LibraryV3UserId, albumId: string, jobId: string) =>
			[
				...LibraryQueryKeyFactory.catalog.root(userId),
				'edition-tag-write',
				albumId,
				jobId
			] as const,
		albumTracksPrefix: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-tracks', albumId] as const,
		albumCopiesPrefix: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-copies'] as const,
		albumHistory: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-history', albumId] as const,
		albumRemovalsPrefix: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-removals'] as const,
		albumRemovals: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.albumRemovalsPrefix(userId), albumId] as const,
		recycleBinPrefix: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'recycle-bin'] as const,
		recycleBinPages: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.recycleBinPrefix(userId), 'pages'] as const,
		recycleBinSummary: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.recycleBinPrefix(userId), 'summary'] as const,
		recycleBinStatus: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.recycleBinPrefix(userId), 'status'] as const,
		unconfirmedPrefix: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'unconfirmed'] as const,
		unconfirmed: (userId: LibraryV3UserId, state: string) =>
			[...LibraryQueryKeyFactory.catalog.unconfirmedPrefix(userId), state] as const
	},
	activity: (userId: LibraryV3UserId) => key(userScope(userId), 'library', 'activity'),
	membership: (userId: LibraryV3UserId, albumIds: string[]) =>
		key(userScope(userId), 'library', 'membership', [...albumIds].sort()),
	currentRuns: () => [...operations(), 'current-runs'] as const,
	runHistory: (cursor: string | undefined) =>
		[...operations(), 'run-history', cursor ?? 'first'] as const,
	run: (runId: string) => [...operations(), 'run', runId] as const,
	runFailures: (runId: string) => [...operations(), 'run-failures', runId] as const,
	runEstimate: (scopeIds: string[]) => [...operations(), 'estimate', [...scopeIds].sort()] as const,
	reviewsPrefix: reviews,
	reviews: (params: object) => [...reviews(), params] as const,
	reviewCount: (params: object) => [...reviews(), 'count', params] as const,
	review: (reviewId: string) => [...reviews(), 'detail', reviewId] as const,
	reviewJobs: () => [...reviews(), 'jobs'] as const,
	policyPrefix: policy,
	targetSettings: () => [...policy(), 'settings'] as const,
	policyTree: () => [...policy(), 'tree'] as const,
	pathMapping: () => [...policy(), 'path-mapping'] as const,
	restorableRoots: () => [...policy(), 'restorable-roots'] as const,
	repairsPrefix: () => [...operations(), 'repairs'] as const,
	repairs: (cursor: string | undefined) =>
		[...LibraryQueryKeyFactory.repairsPrefix(), 'history', cursor ?? 'first'] as const,
	repairEstimate: (rootIds: string[]) =>
		[...LibraryQueryKeyFactory.repairsPrefix(), 'estimate', [...rootIds].sort()] as const,
	repair: (jobId: string) => [...LibraryQueryKeyFactory.repairsPrefix(), 'detail', jobId] as const,
	repairFindings: (jobId: string, findingCategory: string, cursor: string | undefined) =>
		[
			...LibraryQueryKeyFactory.repairsPrefix(),
			'findings',
			jobId,
			findingCategory,
			cursor ?? 'first'
		] as const,
	// The caller's identity reports sit beside the admin operations, so
	// `everyScope('library', 'operations')` refreshes both.
	identityPreparationsPrefix: (userId: LibraryV3UserId) =>
		key(userScope(userId), 'library', 'operations', 'identity-preparations'),
	identityPreparations: (userId: LibraryV3UserId, cursor: string | undefined) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'history',
			cursor ?? 'first'
		] as const,
	identityPreparationEstimate: (userId: LibraryV3UserId, rootIds: string[]) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'estimate',
			[...rootIds].sort()
		] as const,
	identityPreparationFindings: (
		userId: LibraryV3UserId,
		jobId: string,
		findingCategory: string,
		cursor: string | undefined
	) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'findings',
			jobId,
			findingCategory,
			cursor ?? 'first'
		] as const,
	// One release group's library status, with the caller's flags.
	albumPrefix: (userId: LibraryV3UserId) => key(userScope(userId), 'library', 'album'),
	album: (userId: LibraryV3UserId, mbid: string) =>
		[...LibraryQueryKeyFactory.albumPrefix(userId), mbid] as const,
	reidentificationReleases: (
		userId: LibraryV3UserId,
		albumId: string,
		title: string,
		artist: string
	) => key(userScope(userId), 'library', 'reidentification-releases', albumId, title, artist),
	scanSchedule: () => key(ADMIN, 'library', 'scan-schedule'),
	retagPairs: () => key(ADMIN, 'library', 'retag-pairs')
};
