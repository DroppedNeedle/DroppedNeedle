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
import { userIdSegment } from '../userKeySegment';
import { SPOTIFY_ENDPOINTS } from './endpoints';
import { getSpotifyPlaylistsQuery } from './SpotifyQueries.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal?: AbortSignal }) => Promise<unknown>;
	retry?: unknown;
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
