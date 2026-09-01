import { beforeEach, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	get: vi.fn().mockResolvedValue({ items: [] }),
	put: vi.fn().mockResolvedValue(undefined),
	delete: vi.fn().mockResolvedValue(undefined),
	post: vi.fn().mockResolvedValue({ requested: 0, upgrades: 0 }),
	invalidate: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: (factory: () => unknown) => factory(),
	createMutation: (factory: () => unknown) => factory()
}));

vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: h.get,
			put: h.put,
			delete: h.delete,
			post: h.post
		}
	}
}));

vi.mock('$lib/constants', () => ({
	CACHE_TTL: { ALBUM_DETAIL_EDITIONS: 60_000 }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: h.invalidate
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
	editionsKey,
	getAlbumEditionsQuery,
	setEditionPin
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
	h.get.mockResolvedValue({ items: [] });
	h.put.mockResolvedValue(undefined);
	h.delete.mockResolvedValue(undefined);
	h.post.mockResolvedValue({ requested: 0, upgrades: 0 });
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

it('forwards the query abort signal without changing the request contract', async () => {
	const query = getAlbumEditionsQuery(
		() => 'user-a',
		() => 'release-group',
		() => true
	) as unknown as EditionQueryOptions;
	const signal = new AbortController().signal;

	await query.queryFn({ signal });

	expect(h.get).toHaveBeenCalledWith('/api/v1/albums/release-group/editions', { signal });
});

it('invalidates the initiating user edition key after pin and clear mutations', async () => {
	const pin = setEditionPin() as unknown as EditionMutationOptions;
	const clear = clearEditionPin() as unknown as EditionMutationOptions;
	const pinVariables = { userId: 'user-a', mbid: 'release-group', releaseMbid: 'release' };
	const clearVariables = { userId: 'user-b', mbid: 'release-group' };

	await pin.mutationFn(pinVariables);
	await pin.onSuccess(undefined, pinVariables);
	await clear.mutationFn(clearVariables);
	await clear.onSuccess(undefined, clearVariables);

	expect(h.put).toHaveBeenCalledWith('/api/v1/albums/release-group/edition', {
		release_mbid: 'release'
	});
	expect(h.delete).toHaveBeenCalledWith('/api/v1/albums/release-group/edition');
	expect(h.invalidate).toHaveBeenNthCalledWith(1, {
		queryKey: editionsKey('user-a', 'release-group')
	});
	expect(h.invalidate).toHaveBeenNthCalledWith(2, {
		queryKey: editionsKey('user-b', 'release-group')
	});
});

it('keeps acquire invalidation scoped to the authenticated download queue', async () => {
	const acquire = acquireEdition() as unknown as EditionMutationOptions;
	await acquire.mutationFn({ mbid: 'release-group' });

	expect(h.post).toHaveBeenCalledWith('/api/v1/albums/release-group/edition/acquire', {});
});
