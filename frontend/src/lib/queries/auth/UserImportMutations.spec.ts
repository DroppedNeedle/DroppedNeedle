import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: {
		v3: { GET: vi.fn(), POST: vi.fn() }
	}
}));

// The real effects module loads the cache helpers; stub them.
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { createAppMutation, userImportCandidatesChanged, type Effect } from '../effects';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockPost = vi.mocked(api.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockCreateMutation = vi.mocked(createAppMutation);
const mockCreateQuery = vi.mocked(createQuery);

beforeEach(() => {
	vi.clearAllMocks();
});

function lastMutationOpts(): Record<string, unknown> {
	const calls = mockCreateMutation.mock.calls;
	const factory = calls[calls.length - 1][0] as unknown as () => Record<string, unknown>;
	return factory();
}

function lastQueryOpts(): Record<string, unknown> {
	const calls = mockCreateQuery.mock.calls;
	const factory = calls[calls.length - 1][0] as unknown as () => Record<string, unknown>;
	return factory();
}

describe('createImportUsersMutation', () => {
	it('posts provider + uids to the import endpoint via the authenticated api', async () => {
		mockPost.mockResolvedValue({ imported: [], conflicts: [], skipped: [], total_imported: 0 });
		const { createImportUsersMutation } = await import('./UserImportMutations.svelte');
		createImportUsersMutation();

		const mutationFn = lastMutationOpts().mutationFn as (v: unknown) => Promise<unknown>;
		await mutationFn({ provider: 'jellyfin', provider_uids: ['jf-1', 'jf-2'] });

		expect(mockPost).toHaveBeenCalledWith(AUTH_ENDPOINTS.adminImport, {
			provider: 'jellyfin',
			provider_uids: ['jf-1', 'jf-2']
		});
	});

	it("declares the imported provider's candidates changed", async () => {
		const { authStore } = await import('$lib/stores/authStore.svelte');
		authStore.setUser({ id: 'admin-1', role: 'admin' } as never);
		const { createImportUsersMutation } = await import('./UserImportMutations.svelte');
		createImportUsersMutation();

		const effects = lastMutationOpts().effects as (r: unknown, v: unknown) => Effect[];
		expect(
			effects(
				{ imported: [], conflicts: [], skipped: [], total_imported: 0 },
				{ provider: 'plex', provider_uids: [] }
			)
		).toEqual([userImportCandidatesChanged('admin-1', 'plex')]);
		authStore.clear();
	});
});

describe('getImportCandidatesQuery', () => {
	it('keys + hits the right endpoint per provider via authenticated api.get', async () => {
		mockGet.mockResolvedValue({ candidates: [] });
		const { getImportCandidatesQuery } = await import('./ImportCandidatesQuery.svelte');
		getImportCandidatesQuery(
			() => 'plex',
			() => true,
			() => 'admin-1'
		);

		const opts = lastQueryOpts();
		expect(opts.queryKey).toEqual(AuthQueryKeyFactory.importCandidates('admin-1', 'plex'));
		expect(opts.enabled).toBe(true);

		const queryFn = opts.queryFn as (ctx: { signal: AbortSignal }) => Promise<unknown>;
		await queryFn({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(AUTH_ENDPOINTS.adminImportPlex);
	});

	it('is gated by the enabled getter (disabled while the picker is closed)', async () => {
		const { getImportCandidatesQuery } = await import('./ImportCandidatesQuery.svelte');
		getImportCandidatesQuery(
			() => 'jellyfin',
			() => false,
			() => 'admin-1'
		);

		const opts = lastQueryOpts();
		expect(opts.enabled).toBe(false);
		expect(opts.queryKey).toEqual(AuthQueryKeyFactory.importCandidates('admin-1', 'jellyfin'));
	});
});
