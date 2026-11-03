import { v3 } from '$lib/api/v3/endpoint';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), PUT: vi.fn(), POST: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		user: { id: 'admin-1' } as { id: string } | null,
		isAdmin: true
	},
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { ListenBrainzQueryKeyFactory } from '$lib/features/settings/queries/listenbrainz.svelte';
import {
	getListenBrainzConnectionQuery,
	getListenBrainzConnectionQueryOptions
} from '$lib/features/settings/queries/listenbrainz.svelte';
import {
	createSaveListenBrainzMutation,
	createVerifyListenBrainzMutation
} from '$lib/features/settings/queries/listenbrainz.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockPut = vi.mocked(api.v3.PUT) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockPost = vi.mocked(api.v3.POST) as unknown as Mock<
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

describe('ListenBrainz queries', () => {
	it('reads the connection endpoint and forwards the abort signal', async () => {
		mockGet.mockResolvedValue({ username: '', user_token: '', enabled: false });
		const options = getListenBrainzConnectionQueryOptions() as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(v3('/api/v3/settings/listenbrainz'), {
			signal: controller.signal,
			timeoutMs: expect.any(Number)
		});
	});

	it('enables the query for admins only', () => {
		expect((getListenBrainzConnectionQuery() as QueryResult).enabled).toBe(true);

		setAdmin(false);
		expect((getListenBrainzConnectionQuery() as QueryResult).enabled).toBe(false);
	});
});

describe('ListenBrainz mutations', () => {
	const connection = { username: 'needle', user_token: 'token-1', enabled: true };

	it('saves the connection then refreshes the cached connection', async () => {
		mockPut.mockResolvedValue(connection);
		const mutation = createSaveListenBrainzMutation() as unknown as MutationResult;

		await mutation.mutationFn(connection);
		expect(mockPut).toHaveBeenCalledWith(v3('/api/v3/settings/listenbrainz'), connection);

		const context = mutation.onMutate?.(connection) ?? { admin: true };
		await mutation.onSuccess?.(connection, connection, context);
		expect(mockSetData).toHaveBeenCalledWith(ListenBrainzQueryKeyFactory.connection(), connection);
	});

	it('tests the submitted values without touching the cache', async () => {
		const verdict = { valid: true, message: 'Connected as needle' };
		mockPost.mockResolvedValue(verdict);
		const mutation = createVerifyListenBrainzMutation() as unknown as MutationResult;

		const result = await mutation.mutationFn(connection);
		expect(mockPost).toHaveBeenCalledWith(v3('/api/v3/settings/listenbrainz/verify'), connection);
		expect(result).toEqual(verdict);
		expect(mockSetData).not.toHaveBeenCalled();
	});
});
