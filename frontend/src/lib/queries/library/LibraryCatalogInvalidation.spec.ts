import { beforeEach, describe, expect, it, vi } from 'vitest';

const invalidate = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
const clearSearch = vi.hoisted(() => vi.fn());

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: invalidate
}));

vi.mock('$lib/stores/search', () => ({
	searchStore: { clear: clearSearch }
}));

import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';

beforeEach(() => {
	vi.clearAllMocks();
});

describe('invalidateLibraryCatalog', () => {
	it('sweeps catalog, artist, discovery, reconciliation, lyrics, and local caches', async () => {
		await invalidateLibraryCatalog();
		const keys = invalidate.mock.calls.map(([filters]) => filters.queryKey);

		expect(clearSearch).toHaveBeenCalledOnce();
		expect(keys).toContainEqual(['library']);
		expect(keys).toContainEqual(['artist']);
		expect(keys).toContainEqual(['home']);
		expect(keys).toContainEqual(['discover']);
		expect(keys).toContainEqual(['library', 'artist-reconciliation']);
		expect(keys).toContainEqual(['lyrics']);
		expect(keys).toContainEqual(['local']);
	});
});
