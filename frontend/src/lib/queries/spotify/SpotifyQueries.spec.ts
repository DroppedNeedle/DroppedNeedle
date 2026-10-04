import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('../QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { userIdSegment } from '../userKeySegment';
import { PlaylistQueryKeyFactory } from '../playlists/PlaylistQueryKeyFactory';
import { SPOTIFY_ENDPOINTS } from './endpoints';
import {
	createImportSpotifyPlaylistMutation,
	getSpotifyPlaylistsQuery
} from './SpotifyQueries.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal?: AbortSignal }) => Promise<unknown>;
	retry?: unknown;
};

type MutationResult = {
	mutationFn: (input: { id: string; name: string }) => Promise<unknown>;
	onSuccess?: () => void;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('getSpotifyPlaylistsQuery', () => {
	it('reads the caller-owned Spotify playlists behind a user-scoped key', async () => {
		mockGet.mockResolvedValue({ items: [] });
		const query = getSpotifyPlaylistsQuery() as QueryResult;

		await query.queryFn?.({});

		expect(mockGet).toHaveBeenCalledWith(SPOTIFY_ENDPOINTS.playlists());
		expect(query.queryKey).toEqual(['spotify-playlists', userIdSegment('userA')]);
	});

	it('never retries: an unlinked account answers 400, not a flake', () => {
		const query = getSpotifyPlaylistsQuery() as QueryResult;

		expect(query.retry).toBe(false);
	});

	it('scopes the key per user so shared browsers never leak playlists', () => {
		const first = getSpotifyPlaylistsQuery() as QueryResult;
		setUser({ id: 'userB' });
		const second = getSpotifyPlaylistsQuery() as QueryResult;

		expect(first.queryKey).not.toEqual(second.queryKey);
	});
});

describe('createImportSpotifyPlaylistMutation', () => {
	it('imports by stored playlist id, then refreshes playlists and the Spotify list', async () => {
		mockPost.mockResolvedValue({ imported_playlist_id: 'pl-1' });
		const mutation = createImportSpotifyPlaylistMutation() as unknown as MutationResult;

		await mutation.mutationFn({ id: 'spotify-9', name: 'Road Trip' });

		expect(mockPost).toHaveBeenCalledWith(SPOTIFY_ENDPOINTS.importPlaylist('spotify-9'));
		mutation.onSuccess?.();
		const keys = mockInvalidate.mock.calls.map((call) => (call[0] as { queryKey: unknown }).queryKey);
		expect(keys).toContainEqual(PlaylistQueryKeyFactory.list('userA'));
		expect(keys).toContainEqual(['spotify-playlists', userIdSegment('userA')]);
	});
});
