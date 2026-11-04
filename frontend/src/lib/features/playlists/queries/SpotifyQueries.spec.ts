import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), POST: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import { authStore } from '$lib/stores/authStore.svelte';
import { getSpotifyPlaylistsQuery } from '$lib/features/playlists/queries/spotify.svelte';

type QueryResult = { queryKey?: unknown };

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('getSpotifyPlaylistsQuery', () => {
	it('scopes the key per user so shared browsers never leak playlists', () => {
		const first = getSpotifyPlaylistsQuery() as QueryResult;
		setUser({ id: 'userB' });
		const second = getSpotifyPlaylistsQuery() as QueryResult;

		expect(first.queryKey).not.toEqual(second.queryKey);
	});
});
