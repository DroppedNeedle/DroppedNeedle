import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { api } from '$lib/api/client';
import { FavoriteQueryKeyFactory, PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';
import {
	getFavoritesV3Query,
	getPlaylistDetailV3Query,
	getPlaylistListV3Query
} from './PlaylistV3Queries.svelte';
import { PlaylistV3Api } from './PlaylistV3Api';

beforeEach(() => {
	vi.clearAllMocks();
	(api.global.v3.GET as ReturnType<typeof vi.fn>).mockResolvedValue({ playlists: [] });
});

describe('PlaylistV3Queries', () => {
	it('lists the caller playlists with no stale window so creates show at once', async () => {
		const query = getPlaylistListV3Query(() => true) as unknown as {
			queryKey: unknown;
			queryFn: () => Promise<unknown>;
			staleTime: number;
			enabled: boolean;
		};
		expect(query.enabled).toBe(true);
		expect(query.staleTime).toBe(0);
		expect(query.queryKey).toEqual(PlaylistQueryKeyFactory.v3.list('user-1'));
		await query.queryFn();
		expect(api.global.v3.GET).toHaveBeenCalledWith('/api/v3/playlists', expect.anything());
	});

	it('honors the caller enabled gate on the list', () => {
		const query = getPlaylistListV3Query(() => false) as unknown as { enabled: boolean };
		expect(query.enabled).toBe(false);
	});

	it('fetches one playlist without window-focus refetch or retries', async () => {
		const query = getPlaylistDetailV3Query(
			() => 'pl-1',
			() => true
		) as unknown as {
			queryKey: unknown;
			queryFn: () => Promise<unknown>;
			refetchOnWindowFocus: boolean;
			retry: boolean;
		};
		expect(query.refetchOnWindowFocus).toBe(false);
		expect(query.retry).toBe(false);
		expect(query.queryKey).toEqual(PlaylistQueryKeyFactory.v3.detail('user-1', 'pl-1'));
		(api.global.v3.GET as ReturnType<typeof vi.fn>).mockResolvedValue({ id: 'pl-1' });
		await query.queryFn();
		expect(api.global.v3.GET).toHaveBeenCalledWith('/api/v3/playlists/pl-1', expect.anything());
	});

	it('lists favorites for the caller with an optional kind filter', async () => {
		const query = getFavoritesV3Query(() => 'album') as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(query.queryKey).toEqual(FavoriteQueryKeyFactory.list('user-1', 'album'));
		await query.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/favorites?kind=album',
			expect.anything()
		);
		expect(PlaylistV3Api.favorites(null)).toBe('/api/v3/favorites');
	});

	it('encodes playlist ids in detail URLs', () => {
		expect(PlaylistV3Api.detail('a/b')).toBe('/api/v3/playlists/a%2Fb');
	});
});
