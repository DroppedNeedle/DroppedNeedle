import { beforeEach, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	v3: {
		GET: vi.fn().mockResolvedValue({}),
		PUT: vi.fn().mockResolvedValue({}),
		POST: vi.fn().mockResolvedValue({ status: 'started', message: 'ok' }),
		DELETE: vi.fn().mockResolvedValue({})
	},
	invalidate: vi.fn().mockResolvedValue(undefined),
	cached: undefined as unknown
}));

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: (factory: () => unknown) => factory(),
	createMutation: (factory: () => unknown) => factory()
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: h.v3 } }
}));

vi.mock('$lib/constants', () => ({
	CACHE_TTL: { ALBUM_DETAIL_EDITIONS: 60_000 }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: h.invalidate,
	queryClient: { getQueryData: () => h.cached }
}));

vi.mock('$lib/queries/library-management/LibraryManagementInvalidation', () => ({
	invalidateLibraryManagementSurfaces: h.invalidate
}));

vi.mock('$lib/queries/downloads/DownloadQueryKeyFactory', () => ({
	DownloadQueryKeyFactory: {
		tasks: (userId: string | undefined) => ['downloads', 'tasks', userId, 'user', 0],
		tasksPrefix: (userId: string | undefined) => ['downloads', 'tasks', userId]
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-a' } }
}));

import {
	acquireEdition,
	chooseAlbumEdition,
	editionsKey,
	getAlbumEditionStatusQuery,
	getAlbumEditionsQuery,
	handBackAlbumEdition,
	matchEditionFile,
	unmatchEditionFile
} from './EditionQueries.svelte';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';

type EditionQueryOptions = {
	queryKey: readonly unknown[];
	enabled: boolean;
	queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
	refetchInterval: (query: { state: { data?: unknown } }) => number | false;
};

type EditionMutationOptions = {
	mutationFn: (variables: Record<string, unknown>) => Promise<unknown>;
	onSuccess: (data: unknown, variables: Record<string, unknown>) => Promise<unknown>;
	onSettled: (data: unknown, error: unknown, variables: Record<string, unknown>) => unknown;
};

beforeEach(() => {
	vi.clearAllMocks();
	h.invalidate.mockResolvedValue(undefined);
	h.cached = undefined;
});

it('includes the authenticated user in every editions query key', () => {
	const userA = editionsKey('user-a', 'release-group');
	const userB = editionsKey('user-b', 'release-group');

	expect(userA).toEqual([
		'albums',
		'editions',
		'user-a',
		{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 },
		'release-group'
	]);
	expect(userA).not.toEqual(userB);

	const queryA = getAlbumEditionsQuery(
		() => 'user-a',
		() => 'release-group',
		() => true
	) as unknown as EditionQueryOptions;
	const queryB = getAlbumEditionsQuery(
		() => 'user-b',
		() => 'release-group',
		() => true
	) as unknown as EditionQueryOptions;

	expect(queryA.queryKey).toEqual(userA);
	expect(queryB.queryKey).toEqual(userB);
	expect(queryA.enabled).toBe(true);
});

it('refreshes the copy, its plans and pages and both groups after a choice', async () => {
	const choose = chooseAlbumEdition() as unknown as EditionMutationOptions;
	const variables = {
		userId: 'user-a',
		localId: 'local-1',
		rgMbid: 'old-group',
		releaseMbid: 'release'
	};

	await choose.mutationFn(variables);
	await choose.onSuccess({ release_group_mbid: 'new-group' }, variables);

	expect(h.v3.PUT).toHaveBeenCalledWith('/api/v3/library/albums/local-1/edition', {
		release_mbid: 'release'
	});
	const invalidated = h.invalidate.mock.calls.map(([arg]) => arg.queryKey);
	expect(invalidated).toEqual(
		expect.arrayContaining([
			['library', 'catalog', 'user-a', 'edition', 'local-1'],
			['library', 'catalog', 'user-a', 'album-detail', 'local-1'],
			['library', 'catalog', 'user-a', 'album-tracks', 'local-1'],
			['library', 'catalog', 'user-a', 'album-copies'],
			['library', 'album'],
			editionsKey('user-a', 'old-group'),
			editionsKey('user-a', 'new-group'),
			AlbumQueryKeyFactory.basic('user-a', 'old-group'),
			AlbumQueryKeyFactory.basic('user-a', 'new-group'),
			AlbumQueryKeyFactory.tracks('user-a', 'new-group')
		])
	);
});

it('polls the status while the files are placed or written, and only then', () => {
	const query = getAlbumEditionStatusQuery(
		() => 'user-a',
		() => 'local-1'
	) as unknown as EditionQueryOptions;
	const poll = (data: Record<string, unknown>) => query.refetchInterval({ state: { data } });

	expect(poll({ placing: { attempts: 0 }, writing: false, identifying: false })).toBe(2000);
	expect(poll({ placing: null, writing: true, identifying: false })).toBe(2000);
	expect(poll({ placing: null, writing: false, identifying: true })).toBe(2000);
	expect(poll({ placing: null, writing: false, identifying: false })).toBe(false);
});

it('marks the plans stale once the files are placed', async () => {
	const query = getAlbumEditionStatusQuery(
		() => 'user-a',
		() => 'local-1'
	) as unknown as EditionQueryOptions;
	const plans = ['library', 'catalog', 'user-a', 'edition', 'local-1', 'plan'];
	const plansInvalidated = () =>
		h.invalidate.mock.calls.some(([arg]) => JSON.stringify(arg.queryKey) === JSON.stringify(plans));

	h.cached = { release_mbid: 'release', placing: { attempts: 0 } };
	h.v3.GET.mockResolvedValueOnce({ release_mbid: 'release', placing: { attempts: 1 } });
	await query.queryFn({ signal: new AbortController().signal });
	expect(plansInvalidated()).toBe(false);

	h.cached = { release_mbid: 'release', placing: { attempts: 1 } };
	h.v3.GET.mockResolvedValueOnce({ release_mbid: 'release', placing: null });
	await query.queryFn({ signal: new AbortController().signal });
	expect(plansInvalidated()).toBe(true);
});

it('refreshes the download queue and the album after an acquire, for this user only', async () => {
	const acquire = acquireEdition() as unknown as EditionMutationOptions;
	await acquire.mutationFn({ mbid: 'release-group' });

	expect(h.v3.POST).toHaveBeenCalledWith('/api/v3/albums/release-group/edition/acquire');

	await acquire.mutationFn({ mbid: 'release-group', copyId: 'copy-1', replaceVersions: true });
	expect(h.v3.POST).toHaveBeenLastCalledWith(
		'/api/v3/albums/release-group/edition/acquire?album_id=copy-1&replace_versions=true'
	);

	await acquire.onSuccess({}, { mbid: 'release-group' });
	const keys = h.invalidate.mock.calls.map(([arg]) => JSON.stringify(arg.queryKey));
	expect(keys).toContain(JSON.stringify(['downloads', 'tasks', 'user-a']));
	expect(keys).toContain(JSON.stringify(['library', 'album', 'release-group']));
});

it('reads one copy edition by local id and scopes the key by user', () => {
	const queryFor = (userId: string | undefined, localId: string) =>
		getAlbumEditionStatusQuery(
			() => userId,
			() => localId,
			() => true
		) as unknown as EditionQueryOptions;

	expect(queryFor('user-a', 'local-1').queryKey).toEqual([
		'library',
		'catalog',
		'user-a',
		'edition',
		'local-1'
	]);
	expect(queryFor('user-a', 'local-1').queryKey).not.toEqual(
		queryFor('user-b', 'local-1').queryKey
	);
});

it('matches a file by hand and undoes it, refreshing what the carry can move, refused or not', async () => {
	const match = matchEditionFile() as unknown as EditionMutationOptions;
	const unmatch = unmatchEditionFile() as unknown as EditionMutationOptions;
	const variables = {
		localId: 'local-1',
		trackId: 'file-9',
		releaseTrackMbid: 'track-3',
		identityRevision: 4
	};

	await match.mutationFn(variables);
	expect(h.v3.PUT).toHaveBeenCalledWith('/api/v3/library/albums/local-1/edition/files/file-9', {
		release_track_mbid: 'track-3',
		expected_identity_revision: 4
	});
	await unmatch.mutationFn(variables);
	expect(h.v3.DELETE).toHaveBeenCalledWith('/api/v3/library/albums/local-1/edition/files/file-9');

	for (const [done, error] of [
		[match, null],
		[unmatch, null],
		[match, new Error('refused')],
		[unmatch, new Error('refused')]
	] as const) {
		h.invalidate.mockClear();
		await done.onSettled(error ? undefined : {}, error, variables);
		const invalidated = h.invalidate.mock.calls.map(([arg]) => arg.queryKey);
		expect(invalidated).toEqual(
			expect.arrayContaining([
				['library', 'catalog', 'user-a', 'edition', 'local-1'],
				['library', 'catalog', 'user-a', 'album-detail', 'local-1'],
				['library', 'catalog', 'user-a', 'album-tracks', 'local-1'],
				['library', 'catalog', 'user-a', 'album-copies'],
				['library', 'catalog', 'user-a', 'album-history', 'local-1'],
				['favorites'],
				['playlists']
			])
		);
	}
});

it('refuses edition choices carrying an RG MBID or a missing local id', async () => {
	const choose = chooseAlbumEdition() as unknown as EditionMutationOptions;
	const handBack = handBackAlbumEdition() as unknown as EditionMutationOptions;
	expect(() =>
		choose.mutationFn({
			userId: 'user-a',
			localId: 'release-group',
			rgMbid: 'release-group',
			releaseMbid: 'release'
		})
	).toThrow('Choose the edition on the library copy, not the release group.');
	expect(() =>
		handBack.mutationFn({ userId: 'user-a', localId: '', rgMbid: 'release-group' })
	).toThrow('Missing local album id for the edition.');
	expect(h.v3.PUT).not.toHaveBeenCalled();
	expect(h.v3.DELETE).not.toHaveBeenCalled();
});
