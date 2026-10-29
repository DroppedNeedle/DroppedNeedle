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
	api: { v3: h.v3 }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: h.invalidate,
	queryClient: { getQueryData: () => h.cached }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-a' } }
}));

import {
	acquireEdition,
	chooseAlbumEdition,
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
	onError: (error: unknown, variables: Record<string, unknown>) => unknown;
};

beforeEach(() => {
	vi.clearAllMocks();
	h.invalidate.mockResolvedValue(undefined);
	h.cached = undefined;
});

it('keys the editions by user and drops them on a source switch', () => {
	const userA = AlbumQueryKeyFactory.editions('user-a', 'release-group');
	const userB = AlbumQueryKeyFactory.editions('user-b', 'release-group');

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
	expect((queryA as unknown as { meta: unknown }).meta).toEqual({ provider: 'musicbrainz' });
	expect(queryA.enabled).toBe(true);
});

it('refreshes the copy, its plans and pages and both groups after a choice', async () => {
	const choose = chooseAlbumEdition() as unknown as EditionMutationOptions;
	const variables = {
		userId: 'user-a',
		localId: 'local-1',
		rgMbid: 'old-group',
		releaseMbid: 'release',
		identityRevision: 3
	};

	await choose.mutationFn(variables);
	await choose.onSuccess({ release_group_mbid: 'new-group' }, variables);

	expect(h.v3.PUT).toHaveBeenCalledWith('/api/v3/library/albums/local-1/edition', {
		release_mbid: 'release',
		expected_identity_revision: 3
	});
	const invalidated = h.invalidate.mock.calls.map(([arg]) => arg.queryKey);
	expect(invalidated).toEqual(
		expect.arrayContaining([
			['user:user-a', 'library', 'catalog', 'edition', 'local-1'],
			['user:user-a', 'library', 'catalog', 'album-detail', 'local-1'],
			['user:user-a', 'library', 'catalog', 'album-tracks', 'local-1'],
			['user:user-a', 'library', 'catalog', 'album-copies'],
			['user:user-a', 'library', 'album'],
			AlbumQueryKeyFactory.editions('user-a', 'old-group'),
			AlbumQueryKeyFactory.editions('user-a', 'new-group'),
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
	const plans = ['user:user-a', 'library', 'catalog', 'edition', 'local-1', 'plan'];
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
	expect(keys).toContain(JSON.stringify(['user:user-a', 'downloads', 'tasks']));
	expect(keys).toContain(JSON.stringify(['user:user-a', 'library', 'album', 'release-group']));
});

it('reads one copy edition by local id and scopes the key by user', () => {
	const queryFor = (userId: string | undefined, localId: string) =>
		getAlbumEditionStatusQuery(
			() => userId,
			() => localId,
			() => true
		) as unknown as EditionQueryOptions;

	expect(queryFor('user-a', 'local-1').queryKey).toEqual([
		'user:user-a',
		'library',
		'catalog',
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
	expect(h.v3.DELETE).toHaveBeenCalledWith(
		'/api/v3/library/albums/local-1/edition/files/file-9?expected_identity_revision=4'
	);

	for (const [done, error] of [
		[match, null],
		[unmatch, null],
		[match, new Error('refused')],
		[unmatch, new Error('refused')]
	] as const) {
		h.invalidate.mockClear();
		if (error) await done.onError(error, variables);
		else await done.onSuccess({}, variables);
		const invalidated = h.invalidate.mock.calls.map(([arg]) => arg.queryKey);
		expect(invalidated).toEqual(
			expect.arrayContaining([
				['user:user-a', 'library', 'catalog', 'edition', 'local-1'],
				['user:user-a', 'library', 'catalog', 'album-detail', 'local-1'],
				['user:user-a', 'library', 'catalog', 'album-tracks', 'local-1'],
				['user:user-a', 'library', 'catalog', 'album-copies'],
				['user:user-a', 'library', 'catalog', 'album-history', 'local-1'],
				['user:user-a', 'library', 'catalog'],
				['user:user-a', 'playlists']
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
			releaseMbid: 'release',
			identityRevision: 1
		})
	).toThrow('Choose the edition on the library copy, not the release group.');
	expect(() =>
		handBack.mutationFn({
			userId: 'user-a',
			localId: '',
			rgMbid: 'release-group',
			identityRevision: 1
		})
	).toThrow('Missing local album id for the edition.');
	expect(h.v3.PUT).not.toHaveBeenCalled();
	expect(h.v3.DELETE).not.toHaveBeenCalled();
});
