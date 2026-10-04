import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { REMOTE_ENDPOINTS } from './endpoints';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import {
	getRemoteAlbumsQuery,
	getRemoteDiscoveryQuery,
	getRemoteFoldersQuery,
	getRemotePlaylistDetailQuery,
	getRemoteRandomQuery,
	getRemoteSearchQuery
} from './RemoteQueries.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('getRemoteAlbumsQuery', () => {
	it('reads one adapter behind the source segment and forwards params', async () => {
		mockGet.mockResolvedValue({ items: [], total: 0 });
		const options = getRemoteAlbumsQuery(
			() => 'plex',
			() => ({ limit: 10, genre: 'Jazz' }),
			() => true
		) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(
			REMOTE_ENDPOINTS.albums('plex', { limit: 10, genre: 'Jazz' }),
			{ signal: controller.signal }
		);
		expect(options.queryKey).toEqual(
			RemoteQueryKeyFactory.albums('userA', 'plex', { limit: 10, genre: 'Jazz' })
		);
	});

	it('stays disabled without a signed-in user', () => {
		setUser(null);
		const options = getRemoteAlbumsQuery(
			() => 'plex',
			() => ({}),
			() => true
		) as QueryResult;
		expect(options.enabled).toBe(false);
	});
});

describe('getRemoteSearchQuery', () => {
	it('stays disabled on a blank query so empty searches never fire', () => {
		const options = getRemoteSearchQuery(
			() => 'jellyfin',
			() => '   ',
			() => 20,
			() => true
		) as QueryResult;
		expect(options.enabled).toBe(false);
		expect(mockGet).not.toHaveBeenCalled();
	});
});

describe('getRemotePlaylistDetailQuery', () => {
	it('reads the playlist behind the source segment', async () => {
		mockGet.mockResolvedValue({ id: 'pl1', tracks: [] });
		const options = getRemotePlaylistDetailQuery(
			() => 'navidrome',
			() => 'pl1',
			() => true
		) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(REMOTE_ENDPOINTS.playlist('navidrome', 'pl1'), {
			signal: controller.signal
		});
		expect(options.queryKey).toEqual(
			RemoteQueryKeyFactory.playlistDetail('userA', 'navidrome', 'pl1')
		);
	});
});

describe('getRemoteRandomQuery', () => {
	it('reads random tracks behind the source segment and forwards params', async () => {
		mockGet.mockResolvedValue({ items: [], total: 0 });
		const options = getRemoteRandomQuery(
			() => 'navidrome',
			() => ({ limit: 20, genre: 'Jazz' }),
			() => true
		) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(
			REMOTE_ENDPOINTS.random('navidrome', { limit: 20, genre: 'Jazz' }),
			{ signal: controller.signal }
		);
		expect(options.queryKey).toEqual(
			RemoteQueryKeyFactory.random('userA', 'navidrome', { limit: 20, genre: 'Jazz' })
		);
	});
});

describe('getRemoteDiscoveryQuery', () => {
	it('reads discovery shelves behind the source segment', async () => {
		mockGet.mockResolvedValue({ source: 'plex', hubs: [] });
		const options = getRemoteDiscoveryQuery(
			() => 'plex',
			() => 10,
			() => true
		) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(REMOTE_ENDPOINTS.discovery('plex', 10), {
			signal: controller.signal
		});
		expect(options.queryKey).toEqual(RemoteQueryKeyFactory.discovery('userA', 'plex', 10));
	});
});

describe('getRemoteFoldersQuery', () => {
	it('reads the navidrome folder scope', async () => {
		mockGet.mockResolvedValue({ mode: 'all', folder_ids: [] });
		const options = getRemoteFoldersQuery(() => true) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(REMOTE_ENDPOINTS.folders(), {
			signal: controller.signal
		});
		expect(options.queryKey).toEqual(RemoteQueryKeyFactory.folders('userA'));
	});
});
