import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// Stub the query factories so the route shell renders without a QueryClientProvider.
vi.mock('$app/environment', () => ({ browser: true }));

vi.mock('$lib/queries/HomeQuery.svelte', () => ({
	getHomeQuery: () => ({
		data: undefined,
		isLoading: false,
		isRefetching: false,
		dataUpdatedAt: 0,
		error: null,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/local/LocalQueries.svelte', () => ({
	getLocalStatsQuery: () => ({ data: undefined, isError: false }),
	// re-exported key factory (DropImportMutations imports it via this module)
	LOCAL_KEYS: { root: ['local'] }
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryStatsQuery: () => ({ data: undefined, isError: false }),
	getAlbumSearchQuery: () => ({ data: [], isFetching: false })
}));

// SimpleSourceSwitcher (rendered by the page) calls getConnectionsQuery, which
// needs a QueryClient context; stub it like the others so the shell renders.
vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({ data: undefined, isPending: false })
}));

import Page from './+page.svelte';

describe('/+page.svelte', () => {
	it('should render the greeting h1', async () => {
		expect.assertions(2);
		render(Page);

		const heading = page.getByRole('heading', { level: 1 });
		await expect.element(heading).toBeInTheDocument();
		// getGreeting() returns one of these depending on the time of day.
		await expect.element(heading).toHaveTextContent(/Good (morning|afternoon|evening)/);
	});

	it('renders the page subtitle', async () => {
		expect.assertions(1);
		render(Page);

		await expect
			.element(page.getByText('Discover music, explore your library, and find new favorites.'))
			.toBeVisible();
	});
});
