import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

vi.mock('$lib/queries/library/LibraryCatalogInvalidation', () => ({
	invalidateLibraryCatalog: vi.fn()
}));

import { api } from '$lib/api/client';
import { invalidateLibraryCatalog } from '$lib/queries/library/LibraryCatalogInvalidation';
import { LibraryContributionQueryKeyFactory } from './LibraryContributionQueryKeyFactory';
import { getLibraryContributionQuery } from './LibraryContributionQueries.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock;

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
});

describe('library contribution queries', () => {
	it('keeps every key scoped to the signed-in user', () => {
		expect(LibraryContributionQueryKeyFactory.detail('user-1', 'draft-1')).toEqual([
			'library-contributions',
			'user-1',
			'detail',
			'draft-1'
		]);
		expect(LibraryContributionQueryKeyFactory.detail('user-1', 'draft-1')).not.toEqual(
			LibraryContributionQueryKeyFactory.detail('user-2', 'draft-1')
		);
	});

	it('loads contribution detail through the API registry and forwards cancellation', async () => {
		const query = getLibraryContributionQuery(() => 'draft-1') as unknown as {
			queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
			refetchInterval: (query: { state: { data?: { state: string } } }) => number | false;
		};
		const signal = new AbortController().signal;
		await query.queryFn({ signal });
		expect(mockGet).toHaveBeenCalledWith('/api/v3/library/contributions/draft-1', {
			signal
		});
		expect(query.refetchInterval({ state: { data: { state: 'seeded' } } })).toBe(2_000);
		expect(query.refetchInterval({ state: { data: { state: 'verifying' } } })).toBe(2_000);
		expect(query.refetchInterval({ state: { data: { state: 'linked' } } })).toBe(false);
	});

	it('leaves catalog sweeping to the page transition guard, even when linked', async () => {
		mockGet.mockResolvedValue({ state: 'linked' });
		const query = getLibraryContributionQuery(() => 'draft-1') as unknown as {
			queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
			refetchOnMount?: unknown;
		};

		await query.queryFn({ signal: new AbortController().signal });

		expect(invalidateLibraryCatalog).not.toHaveBeenCalled();
		// default refetch semantics - the old 'always' re-triggered the sweep loop
		expect(query.refetchOnMount).toBeUndefined();
	});
});
