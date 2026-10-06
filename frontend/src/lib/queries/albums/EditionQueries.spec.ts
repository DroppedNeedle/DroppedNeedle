import { beforeEach, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	v3: {
		GET: vi.fn().mockResolvedValue({}),
		PUT: vi.fn().mockResolvedValue({}),
		POST: vi.fn().mockResolvedValue({ status: 'started', message: 'ok' }),
		DELETE: vi.fn().mockResolvedValue({})
	},
	invalidate: vi.fn().mockResolvedValue(undefined),
	removeBasic: vi.fn(),
	removeTracks: vi.fn()
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
	invalidateQueriesWithPersister: h.invalidate
}));

vi.mock('$lib/utils/albumDetailCache', () => ({
	albumBasicCache: { remove: h.removeBasic },
	albumTracksCache: { remove: h.removeTracks }
}));

vi.mock('$lib/queries/downloads/DownloadQueryKeyFactory', () => ({
	DownloadQueryKeyFactory: { tasks: (userId: string | undefined) => ['downloads', 'tasks', userId] }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-a' } }
}));

import {
	acquireEdition,
	clearEditionPin,
	clearLocalAlbumEditionPin,
	editionsKey,
	getAlbumEditionsQuery,
	getLocalAlbumEditionPinQuery,
	setEditionPin,
	setLocalAlbumEditionPin
} from './EditionQueries.svelte';

type EditionQueryOptions = {
	queryKey: readonly unknown[];
	enabled: boolean;
	queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
};

type EditionMutationOptions = {
	mutationFn: (variables: Record<string, unknown>) => Promise<unknown>;
	onSuccess: (data: unknown, variables: Record<string, unknown>) => Promise<unknown>;
};

beforeEach(() => {
	vi.clearAllMocks();
	h.invalidate.mockResolvedValue(undefined);
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

it('pins by group on v3 and refreshes the group and its one copy', async () => {
	const pin = setEditionPin() as unknown as EditionMutationOptions;
	const clear = clearEditionPin() as unknown as EditionMutationOptions;
	const pinVariables = {
		userId: 'user-a',
		mbid: 'release-group',
		releaseMbid: 'release',
		localId: 'local-1'
	};
	const clearVariables = { userId: 'user-b', mbid: 'release-group' };

	await pin.mutationFn(pinVariables);
	await pin.onSuccess(undefined, pinVariables);
	await clear.mutationFn(clearVariables);
	await clear.onSuccess(undefined, clearVariables);

	expect(h.v3.PUT).toHaveBeenCalledWith('/api/v3/albums/release-group/edition', {
		release_mbid: 'release'
	});
	expect(h.v3.DELETE).toHaveBeenCalledWith('/api/v3/albums/release-group/edition');
	const invalidated = h.invalidate.mock.calls.map(([arg]) => arg.queryKey);
	expect(invalidated).toContainEqual(editionsKey('user-a', 'release-group'));
	expect(invalidated).toContainEqual(editionsKey('user-b', 'release-group'));
	expect(invalidated).toContainEqual(['library', 'catalog', 'user-a', 'edition-pin', 'local-1']);
	expect(invalidated).toContainEqual(['library', 'catalog', 'user-a', 'album-detail', 'local-1']);
	expect(h.removeTracks).toHaveBeenCalledWith('release-group');
	expect(h.removeBasic).toHaveBeenCalledWith('release-group');
});

it('keeps acquire invalidation scoped to the authenticated download queue', async () => {
	const acquire = acquireEdition() as unknown as EditionMutationOptions;
	await acquire.mutationFn({ mbid: 'release-group' });

	expect(h.v3.POST).toHaveBeenCalledWith('/api/v3/albums/release-group/edition/acquire');
});

it('addresses per-copy pins by local id and scopes the key by user', () => {
	const queryFor = (userId: string | undefined, localId: string) =>
		getLocalAlbumEditionPinQuery(
			() => userId,
			() => localId,
			() => true
		) as unknown as EditionQueryOptions;

	expect(queryFor('user-a', 'local-1').queryKey).toEqual([
		'library',
		'catalog',
		'user-a',
		'edition-pin',
		'local-1'
	]);
	expect(queryFor(undefined, 'local-1').queryKey).toEqual([
		'library',
		'catalog',
		null,
		'edition-pin',
		'local-1'
	]);
	expect(queryFor('user-a', 'local-1').queryKey).not.toEqual(
		queryFor('user-b', 'local-1').queryKey
	);
});

it('refuses per-album pins carrying an RG MBID or a missing local id', async () => {
	const pin = setLocalAlbumEditionPin() as unknown as EditionMutationOptions;
	const clear = clearLocalAlbumEditionPin() as unknown as EditionMutationOptions;
	expect(() =>
		pin.mutationFn({
			userId: 'user-a',
			localId: 'release-group',
			rgMbid: 'release-group',
			releaseMbid: 'release'
		})
	).toThrow('RG MBIDs cannot pin through the per-album edition route.');
	expect(() =>
		clear.mutationFn({ userId: 'user-a', localId: '', rgMbid: 'release-group' })
	).toThrow('Missing local album id for the edition pin.');
	expect(h.v3.PUT).not.toHaveBeenCalled();
	expect(h.v3.DELETE).not.toHaveBeenCalled();
});
