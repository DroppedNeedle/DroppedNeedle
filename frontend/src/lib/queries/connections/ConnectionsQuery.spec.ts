import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

// In-memory stand-in for ../QueryClient: the real module instantiates the
// QueryClient class from @tanstack/svelte-query, which a plain-object mock
// cannot re-export (a factory re-importing the original crashes the browser
// worker). This fake preserves what the tests observe: set/get round-trips,
// clear/ensure caching, and invalidation via queryClient.invalidateQueries.
const queryCache = vi.hoisted(() => ({ map: new Map<string, unknown>() }));

vi.mock('../QueryClient', () => {
	const keyOf = (key: unknown) => JSON.stringify(key);
	const fakeClient = {
		getQueryData: vi.fn(
			<T = unknown>(key: unknown): T | undefined =>
				queryCache.map.get(keyOf(key)) as T | undefined
		),
		setQueryData: vi.fn((key: unknown, updater: unknown) => {
			const next =
				typeof updater === 'function'
					? (updater as (old: unknown) => unknown)(queryCache.map.get(keyOf(key)))
					: updater;
			queryCache.map.set(keyOf(key), next);
			return next;
		}),
		removeQueries: vi.fn((filters?: { queryKey?: unknown }) => {
			if (filters?.queryKey === undefined) {
				queryCache.map.clear();
				return;
			}
			const prefix = keyOf(filters.queryKey).slice(0, -1);
			for (const k of [...queryCache.map.keys()]) {
				if (k.startsWith(prefix)) queryCache.map.delete(k);
			}
		}),
		invalidateQueries: vi.fn(async (_filters?: unknown, _options?: unknown) => undefined),
		cancelQueries: vi.fn(async (_filters?: unknown) => undefined),
		clear: vi.fn(() => queryCache.map.clear()),
		ensureQueryData: vi.fn(
			async (opts: {
				queryKey: unknown;
				queryFn: (ctx: { queryKey: unknown; signal: AbortSignal }) => Promise<unknown>;
			}): Promise<unknown> => {
				const k = keyOf(opts.queryKey);
				if (!queryCache.map.has(k)) {
					queryCache.map.set(
						k,
						await opts.queryFn({
							queryKey: opts.queryKey,
							signal: new AbortController().signal
						})
					);
				}
				return queryCache.map.get(k);
			}
		)
	};
	return {
		queryClient: fakeClient,
		invalidateQueriesWithPersister: vi.fn((filters?: unknown, options?: unknown) =>
			fakeClient.invalidateQueries(filters, options)
		),
		setQueryDataWithPersister: vi.fn(
			<_T = unknown>(key: unknown, updater: unknown): Promise<void> => {
				fakeClient.setQueryData(key, updater);
				return Promise.resolve();
			}
		)
	};
});

vi.mock('idb-keyval', () => ({
	get: vi.fn(),
	set: vi.fn(),
	del: vi.fn(),
	entries: vi.fn(async () => []),
	clear: vi.fn(),
	// Inert UseStore: persistence drops writes, like the get/set stubs above.
	createStore: vi.fn(() => vi.fn(async () => {}))
}));

vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			put: vi.fn(),
			v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() }
		}
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { PLEX_ENDPOINTS } from '../plex/endpoints';
import { REMOTE_ENDPOINTS } from '../remotes/endpoints';
import { queryClient } from '../QueryClient';
import { ConnectionsQueryKeyFactory } from './ConnectionsQueryKeyFactory';
import { CONNECTIONS_ENDPOINTS } from './endpoints';
import { getConnectionsQuery } from './ConnectionsQuery.svelte';
import {
	createConnectJellyfinMutation,
	createConnectListenBrainzMutation,
	createConnectNavidromeMutation,
	createDisconnectMutation,
	createLastFmExchangeSessionMutation,
	createLastFmRequestTokenMutation,
	createPlexLinkPinMutation,
	createPlexLinkPollMutation
} from './ConnectionsMutations.svelte';

const mockPut = vi.mocked(api.global.put);
// The typed client's generics resolve mock results to void; loosen to Mock
// so resolves/implementations typecheck (assertions still pin URLs + bodies).
const mockV3Get = vi.mocked(api.global.v3.GET) as unknown as Mock;
const mockV3Post = vi.mocked(api.global.v3.POST) as unknown as Mock;
const mockV3Put = vi.mocked(api.global.v3.PUT) as unknown as Mock;
const mockV3Delete = vi.mocked(api.global.v3.DELETE) as unknown as Mock;

type Opts = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn: (vars: unknown) => Promise<unknown>;
	onSuccess?: (data: unknown) => Promise<void> | void;
};

beforeEach(() => {
	vi.clearAllMocks();
	(authStore as { user: { id: string } | null }).user = { id: 'userA' };
	mockV3Get.mockRejectedValue(new Error('404'));
	mockV3Post.mockResolvedValue({});
	mockV3Put.mockResolvedValue({});
	mockV3Delete.mockResolvedValue({});
	mockPut.mockResolvedValue({});
});

describe('ConnectionsQueryKeyFactory (AMU-5)', () => {
	it('scopes the key by userId and normalizes a missing id to null', () => {
		expect(ConnectionsQueryKeyFactory.list('userA')).toEqual(['me', 'connections', 'userA']);
		expect(ConnectionsQueryKeyFactory.list(undefined)).toEqual(['me', 'connections', null]);
		expect(ConnectionsQueryKeyFactory.list('userB')).not.toEqual(
			ConnectionsQueryKeyFactory.list('userA')
		);
	});
});

describe('getConnectionsQuery', () => {
	it('builds a userId-scoped key', () => {
		expect((getConnectionsQuery() as unknown as Opts).queryKey).toEqual([
			'me',
			'connections',
			'userA'
		]);
	});

	it('does not leak across a user switch (key re-derives from authStore)', () => {
		expect((getConnectionsQuery() as unknown as Opts).queryKey).toEqual([
			'me',
			'connections',
			'userA'
		]);
		(authStore as { user: { id: string } | null }).user = { id: 'userB' };
		expect((getConnectionsQuery() as unknown as Opts).queryKey).toEqual([
			'me',
			'connections',
			'userB'
		]);
	});

	it('aggregates the per-service v3 reads, linked accounts only', async () => {
		mockV3Get.mockImplementation((url: unknown) => {
			const u = String(url);
			if (u.includes('/remotes/navidrome/connection'))
				return Promise.resolve({
					source: 'navidrome',
					connected: true,
					account_mode: 'linked',
					account_label: 'alice'
				});
			if (u.includes('/remotes/jellyfin/connection'))
				return Promise.resolve({
					source: 'jellyfin',
					connected: true,
					account_mode: 'shared',
					account_label: ''
				});
			if (u.includes('/remotes/plex/connection')) return Promise.reject(new Error('404'));
			if (u.includes('/me/connections/listenbrainz'))
				return Promise.resolve({ service: 'listenbrainz', enabled: true, username: 'alice-lb' });
			if (u.includes('/me/connections/lastfm'))
				return Promise.resolve({ configured: true, linked: false, username: null });
			if (u.includes('/acquire/spotify/playlists')) return Promise.resolve({ playlists: [] });
			return Promise.reject(new Error(`unexpected ${u}`));
		});
		const opts = getConnectionsQuery() as unknown as Opts;
		const data = (await opts.queryFn!({ signal: new AbortController().signal })) as {
			connections: unknown[];
		};
		expect(mockV3Get).toHaveBeenCalledTimes(6);
		// linked navidrome + listenbrainz + spotify presence; the shared-mode
		// jellyfin credential, the rejected plex read, and the unlinked
		// lastfm account stay out (presence means linked).
		expect(data.connections).toEqual([
			{ service: 'navidrome', enabled: true, username: 'alice' },
			{ service: 'listenbrainz', enabled: true, username: 'alice-lb' },
			{ service: 'spotify', enabled: true, username: '' }
		]);
	});

	it('reads an empty list when nothing is linked', async () => {
		const opts = getConnectionsQuery() as unknown as Opts;
		const data = (await opts.queryFn!({ signal: new AbortController().signal })) as {
			connections: unknown[];
		};
		expect(data).toEqual({ connections: [] });
	});
});

describe('connection mutations hit the correct endpoints', () => {
	it('lastfm request token -> POST', async () => {
		const m = createLastFmRequestTokenMutation() as unknown as Opts;
		await m.mutationFn(undefined);
		expect(mockV3Post.mock.calls[0][0]).toBe(CONNECTIONS_ENDPOINTS.lastfmToken());
	});

	it('lastfm exchange session -> POST with token', async () => {
		const m = createLastFmExchangeSessionMutation() as unknown as Opts;
		await m.mutationFn('tok-1');
		expect(mockV3Post).toHaveBeenCalledWith(CONNECTIONS_ENDPOINTS.lastfmSession(), {
			token: 'tok-1'
		});
	});

	it('connect listenbrainz -> PUT with token + username', async () => {
		const m = createConnectListenBrainzMutation() as unknown as Opts;
		await m.mutationFn({ user_token: 'lb', username: 'alice' });
		expect(mockV3Put).toHaveBeenCalledWith(CONNECTIONS_ENDPOINTS.listenbrainz(), {
			user_token: 'lb',
			username: 'alice'
		});
	});

	it('disconnect lastfm -> DELETE the lastfm link', async () => {
		const m = createDisconnectMutation() as unknown as Opts;
		await m.mutationFn('lastfm');
		expect(mockV3Delete.mock.calls[0][0]).toBe(CONNECTIONS_ENDPOINTS.lastfm());
	});

	it('disconnect navidrome -> DELETE the remotes connection', async () => {
		const m = createDisconnectMutation() as unknown as Opts;
		await m.mutationFn('navidrome');
		expect(mockV3Delete.mock.calls[0][0]).toBe(REMOTE_ENDPOINTS.connection('navidrome'));
	});

	it('disconnect spotify -> loud rejection (v3 ships no unlink)', async () => {
		const m = createDisconnectMutation() as unknown as Opts;
		await expect(m.mutationFn('spotify')).rejects.toThrow('not supported');
		expect(mockV3Delete).not.toHaveBeenCalled();
	});

	// media-server account links (issue #138)
	it('connect navidrome -> PUT with username + password', async () => {
		const m = createConnectNavidromeMutation() as unknown as Opts;
		await m.mutationFn({ username: 'alice', password: 'pw' });
		expect(mockPut).toHaveBeenCalledWith(REMOTE_ENDPOINTS.connection('navidrome'), {
			username: 'alice',
			password: 'pw'
		});
	});

	it('connect jellyfin -> PUT with username + password', async () => {
		const m = createConnectJellyfinMutation() as unknown as Opts;
		await m.mutationFn({ username: 'alice', password: 'pw' });
		expect(mockPut).toHaveBeenCalledWith(REMOTE_ENDPOINTS.connection('jellyfin'), {
			username: 'alice',
			password: 'pw'
		});
	});

	it('plex link pin -> POST the single flow start for link', async () => {
		mockV3Post.mockResolvedValueOnce({ pin_id: 7, authorize_url: 'https://plex.tv/link' });
		const m = createPlexLinkPinMutation() as unknown as Opts;
		const pin = (await m.mutationFn(undefined)) as { pin_id: number; auth_url: string };
		expect(mockV3Post.mock.calls[0][0]).toBe(PLEX_ENDPOINTS.start('link'));
		// the mutation maps authorize_url back to the card's auth_url field
		expect(pin).toEqual({ pin_id: 7, auth_url: 'https://plex.tv/link' });
	});

	it('plex link poll -> POST the link poll route with the pin id', async () => {
		mockV3Post.mockResolvedValueOnce({ completed: false });
		const m = createPlexLinkPollMutation() as unknown as Opts;
		await m.mutationFn(7);
		expect(mockV3Post.mock.calls.at(-1)).toEqual([PLEX_ENDPOINTS.poll('link'), { pin_id: 7 }]);
	});
});

describe('mutation onSuccess invalidates the user-scoped key', () => {
	it('disconnect invalidates ["me","connections","userA"]', async () => {
		const spy = vi.spyOn(queryClient, 'invalidateQueries');
		const m = createDisconnectMutation() as unknown as Opts;
		await m.onSuccess!({ service: 'lastfm', deleted: true });
		expect(spy.mock.calls[0][0]).toEqual(
			expect.objectContaining({ queryKey: ['me', 'connections', 'userA'] })
		);
		spy.mockRestore();
	});

	it('plex poll invalidates only once the link completes', async () => {
		const spy = vi.spyOn(queryClient, 'invalidateQueries');
		const m = createPlexLinkPollMutation() as unknown as Opts;
		await m.onSuccess!({ completed: false, username: '' });
		expect(spy).not.toHaveBeenCalled();
		await m.onSuccess!({ completed: true, username: 'alice' });
		expect(spy.mock.calls[0][0]).toEqual(
			expect.objectContaining({ queryKey: ['me', 'connections', 'userA'] })
		);
		spy.mockRestore();
	});
});
