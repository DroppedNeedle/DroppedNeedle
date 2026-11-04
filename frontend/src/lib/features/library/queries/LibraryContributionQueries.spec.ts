import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

vi.mock('$lib/queries/effects', () => ({
	applyEffects: vi.fn(),
	catalogChanged: vi.fn()
}));

import { api } from '$lib/api/client';
import { applyEffects } from '$lib/queries/effects';
import { getLibraryContributionQuery } from './LibraryContributionQueries.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock;

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
});

describe('library contribution queries', () => {
	it('leaves catalog sweeping to the page transition guard, even when linked', async () => {
		mockGet.mockResolvedValue({ state: 'linked' });
		const query = getLibraryContributionQuery(() => 'draft-1') as unknown as {
			queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
			refetchOnMount?: unknown;
		};

		await query.queryFn({ signal: new AbortController().signal });

		expect(applyEffects).not.toHaveBeenCalled();
		// default refetch semantics - the old 'always' re-triggered the sweep loop
		expect(query.refetchOnMount).toBeUndefined();
	});
});
