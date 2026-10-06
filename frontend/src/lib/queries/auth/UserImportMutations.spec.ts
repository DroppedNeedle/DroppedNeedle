import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: {
		v3: { GET: vi.fn(), POST: vi.fn() }
	}
}));

vi.mock('../QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { createMutation, createQuery } from '@tanstack/svelte-query';
import { invalidateQueriesWithPersister } from '../QueryClient';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockPost = vi.mocked(api.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockCreateMutation = vi.mocked(createMutation);
const mockCreateQuery = vi.mocked(createQuery);
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

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

describe('AuthQueryKeyFactory.importCandidates', () => {
	it('keys by provider and userId under the auth prefix', () => {
		expect(AuthQueryKeyFactory.importCandidates('plex', 'admin-1')).toEqual([
			'auth',
			'import',
			'plex',
			'admin-1'
		]);
	});
});

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

	it('onSuccess invalidates the importCandidates key for the imported provider', async () => {
		const { authStore } = await import('$lib/stores/authStore.svelte');
		authStore.setUser({ id: 'admin-1', role: 'admin' } as never);
		const { createImportUsersMutation } = await import('./UserImportMutations.svelte');
		createImportUsersMutation();

		const onSuccess = lastMutationOpts().onSuccess as (r: unknown, v: unknown) => unknown;
		await onSuccess(
			{ imported: [], conflicts: [], skipped: [], total_imported: 0 },
			{ provider: 'plex', provider_uids: [] }
		);

		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: AuthQueryKeyFactory.importCandidates('plex', 'admin-1')
		});
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
		expect(opts.queryKey).toEqual(AuthQueryKeyFactory.importCandidates('plex', 'admin-1'));
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
		expect(opts.queryKey).toEqual(AuthQueryKeyFactory.importCandidates('jellyfin', 'admin-1'));
	});
});
