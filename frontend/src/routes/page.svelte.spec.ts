import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { HomeResponse } from '$lib/types';

// Stub the query factories so the route shell renders without a QueryClientProvider.
vi.mock('$app/environment', () => ({ browser: true }));

const { homeState, sectionState } = vi.hoisted(() => ({
	homeState: {
		data: undefined as Partial<HomeResponse> | undefined,
		isLoading: false,
		isRefetching: false,
		dataUpdatedAt: 0,
		error: null,
		refetch: () => {}
	},
	sectionState: { shouldThrow: false }
}));

vi.mock('$lib/queries/HomeQuery.svelte', () => ({
	getHomeQuery: () => homeState
}));

// section carousel stub, optionally throwing to exercise the error boundary
vi.mock('$lib/components/HomeSection.svelte', () => ({
	default: function () {
		if (sectionState.shouldThrow) throw new Error('section exploded');
	}
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

vi.mock('$lib/queries/discover/DiscoverDemand.svelte', () => ({ useDiscoverActivity: vi.fn() }));
import Page from './+page.svelte';

function contentResponse(): Partial<HomeResponse> {
	return {
		popular_albums: {
			title: 'Popular Now',
			type: 'albums',
			items: [
				{
					mbid: null,
					name: 'Album',
					artist_name: 'Artist',
					artist_mbid: null,
					image_url: null,
					release_date: null,
					listen_count: null,
					in_library: false
				}
			],
			source: null,
			fallback_message: null,
			connect_service: null
		},
		service_prompts: [],
		refreshing: false
	};
}

describe('/+page.svelte', () => {
	beforeEach(() => {
		homeState.data = undefined;
		sectionState.shouldThrow = false;
	});

	it('should render the greeting h1', async () => {
		expect.assertions(2);
		await render(Page);

		const heading = page.getByRole('heading', { level: 1 });
		await expect.element(heading).toBeInTheDocument();
		// getGreeting() returns one of these depending on the time of day.
		await expect.element(heading).toHaveTextContent(/Good (morning|afternoon|evening)/);
	});

	it('renders the page subtitle', async () => {
		expect.assertions(1);
		await render(Page);

		await expect
			.element(page.getByText('Discover music, explore your library, and find new favorites.'))
			.toBeVisible();
	});

	it('a crashing section degrades to an inline error card instead of killing the page', async () => {
		homeState.data = contentResponse();
		sectionState.shouldThrow = true;
		await render(Page);

		await expect.element(page.getByText('Something Went Wrong')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Try Again' })).toBeVisible();
	});

	it('renders content sections without the error card when nothing crashes', async () => {
		homeState.data = contentResponse();
		await render(Page);

		await expect.element(page.getByText("What's Hot")).toBeVisible();
		await expect.element(page.getByText('Something Went Wrong')).not.toBeInTheDocument();
	});
});
