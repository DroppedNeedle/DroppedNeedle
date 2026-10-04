import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), PUT: vi.fn(), POST: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		user: { id: 'admin-1' } as { id: string } | null,
		isAdmin: true
	},
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('../QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '../QueryClient';
import { ListenBrainzQueryKeyFactory } from './ListenBrainzQueryKeyFactory';
import { LISTENBRAINZ_ENDPOINTS } from './endpoints';
import {
	getListenBrainzConnectionQuery,
	getListenBrainzConnectionQueryOptions,
	getScrobbleTargetsQuery,
	getScrobbleTargetsQueryOptions
} from './ListenBrainzQuery.svelte';
import {
	createSaveListenBrainzMutation,
	createSaveScrobbleTargetsMutation,
	createVerifyListenBrainzMutation
} from './ListenBrainzMutations.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPut = vi.mocked(api.global.v3.PUT) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockSetData = vi.mocked(setQueryDataWithPersister);

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: boolean;
};

type MutationResult = {
	mutationFn: (vars: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown) => { admin: boolean };
	onSuccess?: (data: unknown, vars: unknown, context: { admin: boolean }) => unknown;
};

type AuthStoreShape = { user: { id: string } | null; isAdmin: boolean };

function setAdmin(isAdmin: boolean) {
	const store = authStore as AuthStoreShape;
	store.isAdmin = isAdmin;
	store.user = isAdmin ? { id: 'admin-1' } : { id: 'user-1' };
}

beforeEach(() => {
	vi.clearAllMocks();
	setAdmin(true);
});

describe('ListenBrainzQueryKeyFactory', () => {
	it('keys the connection and scrobble targets under settings', () => {
		expect(ListenBrainzQueryKeyFactory.prefix).toEqual(['settings']);
		expect(ListenBrainzQueryKeyFactory.connection()).toEqual(['settings', 'listenbrainz']);
		expect(ListenBrainzQueryKeyFactory.scrobble()).toEqual(['settings', 'scrobble']);
	});
});

describe('ListenBrainz queries', () => {
	it('reads the connection endpoint and forwards the abort signal', async () => {
		mockGet.mockResolvedValue({ username: '', user_token: '', enabled: false });
		const options = getListenBrainzConnectionQueryOptions() as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(LISTENBRAINZ_ENDPOINTS.connection, {
			signal: controller.signal,
			timeoutMs: expect.any(Number)
		});
	});

	it('reads the scrobble-targets endpoint', async () => {
		mockGet.mockResolvedValue({ scrobble_to_lastfm: false, scrobble_to_listenbrainz: false });
		const options = getScrobbleTargetsQueryOptions() as QueryResult;
		await options.queryFn?.({ signal: new AbortController().signal });

		expect(mockGet).toHaveBeenCalledWith(LISTENBRAINZ_ENDPOINTS.scrobble, {
			signal: expect.any(AbortSignal),
			timeoutMs: expect.any(Number)
		});
	});

	it('enables both queries for admins only', () => {
		expect((getListenBrainzConnectionQuery() as QueryResult).enabled).toBe(true);
		expect((getScrobbleTargetsQuery() as QueryResult).enabled).toBe(true);

		setAdmin(false);
		expect((getListenBrainzConnectionQuery() as QueryResult).enabled).toBe(false);
		expect((getScrobbleTargetsQuery() as QueryResult).enabled).toBe(false);
	});
});

describe('ListenBrainz mutations', () => {
	const connection = { username: 'needle', user_token: 'token-1', enabled: true };

	it('saves the connection then refreshes the cached connection', async () => {
		mockPut.mockResolvedValue(connection);
		const mutation = createSaveListenBrainzMutation() as unknown as MutationResult;

		await mutation.mutationFn(connection);
		expect(mockPut).toHaveBeenCalledWith(LISTENBRAINZ_ENDPOINTS.connection, connection);

		const context = mutation.onMutate?.(connection) ?? { admin: true };
		await mutation.onSuccess?.(connection, connection, context);
		expect(mockSetData).toHaveBeenCalledWith(ListenBrainzQueryKeyFactory.connection(), connection);
	});

	it('saves the scrobble targets then refreshes the cached targets', async () => {
		const targets = { scrobble_to_lastfm: true, scrobble_to_listenbrainz: true };
		mockPut.mockResolvedValue(targets);
		const mutation = createSaveScrobbleTargetsMutation() as unknown as MutationResult;

		await mutation.mutationFn(targets);
		expect(mockPut).toHaveBeenCalledWith(LISTENBRAINZ_ENDPOINTS.scrobble, targets);

		const context = mutation.onMutate?.(targets) ?? { admin: true };
		await mutation.onSuccess?.(targets, targets, context);
		expect(mockSetData).toHaveBeenCalledWith(ListenBrainzQueryKeyFactory.scrobble(), targets);
	});

	it('tests the submitted values without touching the cache', async () => {
		const verdict = { valid: true, message: 'Connected as needle' };
		mockPost.mockResolvedValue(verdict);
		const mutation = createVerifyListenBrainzMutation() as unknown as MutationResult;

		const result = await mutation.mutationFn(connection);
		expect(mockPost).toHaveBeenCalledWith(LISTENBRAINZ_ENDPOINTS.verify, connection);
		expect(result).toEqual(verdict);
		expect(mockSetData).not.toHaveBeenCalled();
	});
});
