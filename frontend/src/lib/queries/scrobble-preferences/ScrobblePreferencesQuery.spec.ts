import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
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
	api: { global: { put: vi.fn(), v3: { GET: vi.fn(), PUT: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { queryClient } from '../QueryClient';
import { ScrobblePreferencesQueryKeyFactory } from './ScrobblePreferencesQueryKeyFactory';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from './endpoints';
import { getScrobblePreferencesQuery } from './ScrobblePreferencesQuery.svelte';
import { getScrobblePreferencesQueryOptions } from './ScrobblePreferencesQuery.svelte';
import { createUpdateScrobblePreferencesMutation } from './ScrobblePreferencesMutations.svelte';
import type { ScrobblePreferences } from './types';
import { musicSourceStore } from '$lib/stores/musicSource';

// The typed client's generics resolve mock results to void; loosen to Mock
// so resolves typecheck (assertions still pin URLs + bodies).
const mockV3Get = vi.mocked(api.global.v3.GET) as unknown as Mock;
const mockV3Put = vi.mocked(api.global.v3.PUT) as unknown as Mock;
const mockPut = vi.mocked(api.global.put);

type Opts = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn: (vars: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown) => { userId: string | undefined };
	onSuccess?: (
		data: unknown,
		vars: unknown,
		context: { userId: string | undefined }
	) => Promise<void> | void;
};

beforeEach(() => {
	vi.clearAllMocks();
	queryClient.clear();
	musicSourceStore.reset();
	(authStore as { user: { id: string } | null }).user = { id: 'userA' };
	mockV3Get.mockResolvedValue({
		scrobble_to_lastfm: false,
		scrobble_to_listenbrainz: false,
		primary_music_source: 'listenbrainz'
	});
	mockV3Put.mockResolvedValue({
		scrobble_to_lastfm: false,
		scrobble_to_listenbrainz: false,
		navidrome_handles_external_scrobbles: true,
		primary_music_source: 'lastfm',
		now_playing_visibility: 'full',
		auto_request_personal_mix: false,
		auto_request_state: 'none'
	});
	mockPut.mockResolvedValue({
		scrobble_to_lastfm: false,
		scrobble_to_listenbrainz: false,
		navidrome_handles_external_scrobbles: true,
		primary_music_source: 'lastfm',
		now_playing_visibility: 'full',
		auto_request_personal_mix: false,
		auto_request_state: 'none'
	});
});

describe('ScrobblePreferencesQueryKeyFactory (AMU-5)', () => {
	it('scopes the key by userId', () => {
		expect(ScrobblePreferencesQueryKeyFactory.get('userA')).toEqual([
			'me',
			'scrobble-preferences',
			'userA'
		]);
		expect(ScrobblePreferencesQueryKeyFactory.get('userB')).not.toEqual(
			ScrobblePreferencesQueryKeyFactory.get('userA')
		);
	});
});

describe('getScrobblePreferencesQuery', () => {
	it('builds a userId-scoped key and fetches /me/scrobble-preferences', async () => {
		const opts = getScrobblePreferencesQuery() as unknown as Opts;
		const signal = new AbortController().signal;
		expect(opts.queryKey).toEqual(['me', 'scrobble-preferences', 'userA']);
		await opts.queryFn!({ signal });
		expect(mockV3Get).toHaveBeenCalledWith(SCROBBLE_PREFERENCES_ENDPOINTS.get(), {
			signal,
			timeoutMs: 10_000
		});
	});

	it('reuses one session bootstrap fetch across ordinary navigation', async () => {
		const options = getScrobblePreferencesQueryOptions('userA');
		await queryClient.ensureQueryData(options);
		const cachedStart = performance.now();
		for (let transition = 0; transition < 10; transition += 1) {
			await queryClient.ensureQueryData(options);
		}
		const cachedDuration = performance.now() - cachedStart;
		expect(mockV3Get).toHaveBeenCalledOnce();
		expect(cachedDuration).toBeLessThan(400);
		expect(options.staleTime).toBe(Infinity);
		expect(options.gcTime).toBe(Infinity);
	});
});

describe('update scrobble preferences', () => {
	it('PUTs the partial update', async () => {
		const m = createUpdateScrobblePreferencesMutation() as unknown as Opts;
		await m.mutationFn({ scrobble_to_lastfm: true });
		expect(mockV3Put).toHaveBeenCalledWith(SCROBBLE_PREFERENCES_ENDPOINTS.update(), {
			scrobble_to_lastfm: true
		});
	});

	it('onSuccess immediately updates only the user-scoped persisted key', async () => {
		const m = createUpdateScrobblePreferencesMutation() as unknown as Opts;
		const context = m.onMutate!({ primary_music_source: 'lastfm' });
		const updated = await m.mutationFn({ primary_music_source: 'lastfm' });
		await m.onSuccess!(updated, { primary_music_source: 'lastfm' }, context);
		expect(queryClient.getQueryData(['me', 'scrobble-preferences', 'userA'])).toMatchObject({
			primary_music_source: 'lastfm'
		});
		expect(queryClient.getQueryData(['me', 'scrobble-preferences', 'userB'])).toBeUndefined();
	});

	it('discards a delayed mutation result after an account switch', async () => {
		const m = createUpdateScrobblePreferencesMutation() as unknown as Opts;
		const setSource = vi.spyOn(musicSourceStore, 'setSource');
		const context = m.onMutate!({ primary_music_source: 'lastfm' });
		const updated = await m.mutationFn({ primary_music_source: 'lastfm' });
		(authStore as { user: { id: string } | null }).user = { id: 'userB' };

		await m.onSuccess!(updated, { primary_music_source: 'lastfm' }, context);

		expect(setSource).not.toHaveBeenCalled();
		expect(queryClient.getQueryData(['me', 'scrobble-preferences', 'userA'])).toBeUndefined();
		expect(queryClient.getQueryData(['me', 'scrobble-preferences', 'userB'])).toBeUndefined();
		setSource.mockRestore();
	});

	it('does not repopulate source state or cache when save resolves after an account switch', async () => {
		let resolveSave: (preferences: ScrobblePreferences) => void = () => undefined;
		mockPut.mockImplementationOnce(
			() =>
				new Promise<ScrobblePreferences>((resolve) => {
					resolveSave = resolve;
				})
		);
		const save = musicSourceStore.save('lastfm');
		(authStore as { user: { id: string } | null }).user = { id: 'userB' };
		musicSourceStore.reset();
		resolveSave({
			scrobble_to_lastfm: false,
			scrobble_to_listenbrainz: false,
			navidrome_handles_external_scrobbles: true,
			primary_music_source: 'lastfm',
			now_playing_visibility: 'full',
			auto_request_personal_mix: false,
			auto_request_state: 'none'
		});

		await expect(save).resolves.toBe(true);
		expect(musicSourceStore.getSource()).toBe('listenbrainz');
		expect(queryClient.getQueryData(['me', 'scrobble-preferences', 'userA'])).toBeUndefined();
		expect(queryClient.getQueryData(['me', 'scrobble-preferences', 'userB'])).toBeUndefined();
	});
});
