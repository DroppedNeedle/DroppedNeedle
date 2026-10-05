import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';

// Stub createQuery/createMutation so we can pull the options object straight out;
// ../QueryClient is an in-memory fake below (the real one needs the QueryClient
// class, which a plain-object tanstack mock cannot re-export).
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
			<T = unknown>(key: unknown): T | undefined => queryCache.map.get(keyOf(key)) as T | undefined
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
		),
		resetQueryCacheForUserSwitch: vi.fn(async (): Promise<void> => {
			queryClient.clear();
			const idb = await import('idb-keyval');
			await idb.clear();
		})
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
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), PATCH: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { setUser: vi.fn(), clear: vi.fn(), user: null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { clear as idbClear } from 'idb-keyval';
import {
	queryClient,
	resetQueryCacheForUserSwitch,
	setQueryDataWithPersister
} from '../QueryClient';
import { ProfileQueryKeyFactory } from './ProfileQueryKeyFactory';
import { PROFILE_ENDPOINTS } from './endpoints';
import { getProfileQuery } from './ProfileQuery.svelte';
import {
	createChangePasswordMutation,
	createSetPasswordMutation,
	createUpdateEmailMutation,
	createUpdateUsernameMutation
} from './ProfileMutations.svelte';

// The typed client's generics resolve mock results to void; loosen to Mock
// so resolves typecheck (assertions still pin URLs + bodies).
const mockV3Get = vi.mocked(api.global.v3.GET) as unknown as Mock;
const mockV3Post = vi.mocked(api.global.v3.POST) as unknown as Mock;
const mockV3Put = vi.mocked(api.global.v3.PUT) as unknown as Mock;
const mockV3Patch = vi.mocked(api.global.v3.PATCH) as unknown as Mock;

const SESSION_USER = {
	id: 'userA',
	display_name: 'Alice',
	role: 'user',
	email: null,
	avatar_url: null,
	username: 'alice',
	username_display: 'alice',
	providers: ['local']
};

beforeEach(() => {
	vi.clearAllMocks();
	mockV3Get.mockResolvedValue({});
	mockV3Post.mockResolvedValue(SESSION_USER);
	mockV3Put.mockResolvedValue(SESSION_USER);
	mockV3Patch.mockResolvedValue(SESSION_USER);
});

type Opts = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn: (vars: unknown) => Promise<unknown>;
	onSuccess: (user: unknown) => Promise<void> | void;
};

describe('ProfileQueryKeyFactory', () => {
	it('scopes the profile key by userId (AMU-5)', () => {
		expect(ProfileQueryKeyFactory.profile('userA')).toEqual(['profile', 'userA']);
		expect(ProfileQueryKeyFactory.profile('userB')).not.toEqual(
			ProfileQueryKeyFactory.profile('userA')
		);
	});
});

describe('getProfileQuery', () => {
	it('builds a userId-scoped key and fetches GET /api/v3/me', async () => {
		const opts = getProfileQuery('userA') as unknown as Opts;
		expect(opts.queryKey).toEqual(['profile', 'userA']);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockV3Get.mock.calls[0][0]).toBe(PROFILE_ENDPOINTS.get());
	});
});

describe('profile mutations hit the correct endpoints', () => {
	it('username -> PUT /api/v3/me/username', async () => {
		const m = createUpdateUsernameMutation('userA') as unknown as Opts;
		await m.mutationFn({ username: 'bob' });
		expect(mockV3Put).toHaveBeenCalledWith(PROFILE_ENDPOINTS.updateUsername(), {
			username: 'bob'
		});
	});

	it('email -> PUT /api/v3/me/email', async () => {
		const m = createUpdateEmailMutation('userA') as unknown as Opts;
		await m.mutationFn({ email: null });
		expect(mockV3Put).toHaveBeenCalledWith(PROFILE_ENDPOINTS.updateEmail(), { email: null });
	});

	it('change password -> POST /api/v3/me/password', async () => {
		const m = createChangePasswordMutation('userA') as unknown as Opts;
		await m.mutationFn({ current_password: 'a', new_password: 'b' });
		expect(mockV3Post).toHaveBeenCalledWith(PROFILE_ENDPOINTS.changePassword(), {
			current_password: 'a',
			new_password: 'b'
		});
	});

	it('set password -> POST /api/v3/me/local-password', async () => {
		const m = createSetPasswordMutation('userA') as unknown as Opts;
		await m.mutationFn({ new_password: 'b' });
		expect(mockV3Post).toHaveBeenCalledWith(PROFILE_ENDPOINTS.setPassword(), {
			new_password: 'b'
		});
	});
});

describe('mutation onSuccess', () => {
	it('syncs authStore and invalidates the user-scoped profile key', async () => {
		const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries');
		const m = createUpdateUsernameMutation('userA') as unknown as Opts;

		await m.onSuccess(SESSION_USER);

		expect(authStore.setUser).toHaveBeenCalledTimes(1);
		expect(invalidateSpy).toHaveBeenCalledTimes(1);
		expect(invalidateSpy.mock.calls[0][0]).toEqual(
			expect.objectContaining({ queryKey: ['profile', 'userA'] })
		);
		invalidateSpy.mockRestore();
	});
});

describe('resetQueryCacheForUserSwitch (AMU-5)', () => {
	it('empties the in-memory client AND the persisted IndexedDB store', async () => {
		await setQueryDataWithPersister(['profile', 'userA'], { display_name: 'leaked' });
		expect(queryClient.getQueryData(['profile', 'userA'])).toBeDefined();

		await resetQueryCacheForUserSwitch();

		expect(queryClient.getQueryData(['profile', 'userA'])).toBeUndefined();
		expect(idbClear).toHaveBeenCalled();
	});
});
