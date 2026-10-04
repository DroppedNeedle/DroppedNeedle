import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));
vi.mock('$lib/api/client', () => ({
	api: {
		global: { v3: { POST: vi.fn(), PUT: vi.fn(), PATCH: vi.fn(), DELETE: vi.fn() } }
	}
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import { FavoriteQueryKeyFactory, PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';
import {
	addPlaylistTracksV3,
	checkPlaylistTracksV3,
	createPlaylistV3,
	deletePlaylistCoverV3,
	deletePlaylistV3,
	removePlaylistTrackV3,
	removePlaylistTracksV3,
	reorderPlaylistTrackV3,
	resolvePlaylistSourcesV3,
	setFavoriteV3,
	setPlaylistVisibilityV3,
	updatePlaylistTrackV3,
	updatePlaylistV3,
	uploadPlaylistCoverV3
} from './PlaylistV3Mutations.svelte';

type Mutation<TVars, TData = unknown> = {
	mutationFn: (vars: TVars) => Promise<TData>;
	onSuccess?: (data: TData, vars: TVars) => Promise<unknown> | unknown;
};

const mutation = <TVars, TData = unknown>() =>
	captured.current as unknown as Mutation<TVars, TData>;

const listKey = { queryKey: PlaylistQueryKeyFactory.v3.list('user-1') };
const detailKey = { queryKey: PlaylistQueryKeyFactory.v3.detail('user-1', 'pl-1') };

beforeEach(() => vi.clearAllMocks());

describe('PlaylistV3Mutations', () => {
	it('creates a playlist and refreshes the list', async () => {
		createPlaylistV3();
		await mutation<string>().mutationFn('Road trip');
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/playlists', {
			name: 'Road trip'
		});
		await mutation<string>().onSuccess?.({} as never, 'Road trip');
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(listKey);
	});

	it('renames a playlist and refreshes list plus detail', async () => {
		updatePlaylistV3();
		const vars = { id: 'pl-1', name: 'Renamed' };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.PUT).toHaveBeenCalledWith('/api/v3/playlists/pl-1', {
			name: 'Renamed'
		});
		await mutation<typeof vars>().onSuccess?.({} as never, vars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(listKey);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);
	});

	it('deletes a playlist and refreshes list plus detail', async () => {
		deletePlaylistV3();
		await mutation<string>().mutationFn('pl-1');
		expect(api.global.v3.DELETE).toHaveBeenCalledWith('/api/v3/playlists/pl-1');
		await mutation<string>().onSuccess?.(undefined, 'pl-1');
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(listKey);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);
	});

	it('adds tracks at a position and refreshes list plus detail', async () => {
		addPlaylistTracksV3();
		const tracks = [{ track_name: 'Song', artist_name: 'Artist', album_name: 'Album' }];
		await mutation<{ id: string; tracks: unknown[]; position?: number }>().mutationFn({
			id: 'pl-1',
			tracks,
			position: 2
		});
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/playlists/pl-1/tracks', {
			tracks,
			position: 2
		});
		await mutation<{ id: string }>().onSuccess?.({} as never, { id: 'pl-1' });
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(listKey);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);
	});

	it('removes one track and many tracks', async () => {
		removePlaylistTrackV3();
		await mutation<{ id: string; trackId: string }>().mutationFn({
			id: 'pl-1',
			trackId: 't-1'
		});
		expect(api.global.v3.DELETE).toHaveBeenCalledWith('/api/v3/playlists/pl-1/tracks/t-1');
		await mutation<{ id: string; trackId: string }>().onSuccess?.({} as never, {
			id: 'pl-1',
			trackId: 't-1'
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);

		removePlaylistTracksV3();
		await mutation<{ id: string; trackIds: string[] }>().mutationFn({
			id: 'pl-1',
			trackIds: ['t-1', 't-2']
		});
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/playlists/pl-1/tracks/remove', {
			track_ids: ['t-1', 't-2']
		});
	});

	it('retargets a track source and reorders within the detail only', async () => {
		updatePlaylistTrackV3();
		const updateVars = { id: 'pl-1', trackId: 't-1', sourceType: 'local' };
		await mutation<typeof updateVars>().mutationFn(updateVars);
		expect(api.global.v3.PATCH).toHaveBeenCalledWith('/api/v3/playlists/pl-1/tracks/t-1', {
			source_type: 'local'
		});
		await mutation<typeof updateVars>().onSuccess?.({} as never, updateVars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);
		expect(invalidateQueriesWithPersister).not.toHaveBeenCalledWith(listKey);

		reorderPlaylistTrackV3();
		const reorderVars = { id: 'pl-1', trackId: 't-1', newPosition: 0 };
		await mutation<typeof reorderVars>().mutationFn(reorderVars);
		expect(api.global.v3.PATCH).toHaveBeenCalledWith('/api/v3/playlists/pl-1/tracks/reorder', {
			track_id: 't-1',
			new_position: 0
		});
		await mutation<typeof reorderVars>().onSuccess?.({} as never, reorderVars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);
	});

	it('flips visibility and refreshes list plus detail', async () => {
		setPlaylistVisibilityV3();
		const vars = { id: 'pl-1', isPublic: true };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.PATCH).toHaveBeenCalledWith('/api/v3/playlists/pl-1/visibility', {
			is_public: true
		});
		await mutation<typeof vars>().onSuccess?.({} as never, vars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(listKey);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);
	});

	it('uploads a cover as base64 and removes it through DELETE', async () => {
		uploadPlaylistCoverV3();
		const file = new File(['cover-bytes'], 'cover.png', { type: 'image/png' });
		await mutation<{ id: string; file: File }>().mutationFn({ id: 'pl-1', file });
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/playlists/pl-1/cover', {
			content_type: 'image/png',
			image_base64: Buffer.from('cover-bytes').toString('base64')
		});
		await mutation<{ id: string; file: File }>().onSuccess?.({} as never, {
			id: 'pl-1',
			file
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(listKey);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith(detailKey);

		deletePlaylistCoverV3();
		await mutation<string>().mutationFn('pl-1');
		expect(api.global.v3.DELETE).toHaveBeenCalledWith('/api/v3/playlists/pl-1/cover');
	});

	it('checks membership and resolves sources as pure reads without invalidation', async () => {
		checkPlaylistTracksV3();
		const tracks = [{ track_name: 'Song', artist_name: 'Artist', album_name: 'Album' }];
		const body = { tracks };
		await mutation<typeof body>().mutationFn(body);
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/playlists/check-tracks', {
			tracks
		});
		expect(mutation<typeof body>().onSuccess).toBeUndefined();

		resolvePlaylistSourcesV3();
		await mutation<string>().mutationFn('pl-1');
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/playlists/pl-1/resolve-sources');
		expect(mutation<string>().onSuccess).toBeUndefined();
		expect(invalidateQueriesWithPersister).not.toHaveBeenCalled();
	});

	it('favorites an album and sweeps favorites plus the library views carrying the flag', async () => {
		setFavoriteV3();
		const vars = { kind: 'album' as const, itemId: 'album-1', favorited: true };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.PUT).toHaveBeenCalledWith('/api/v3/favorites/album/album-1', {
			favorited: true
		});
		await mutation<typeof vars>().onSuccess?.({} as never, vars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: FavoriteQueryKeyFactory.user('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.root('user-1')
		});
	});
});
