import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { DiscoverResponseV3 } from '$lib/queries/discover/DiscoverV3Queries.svelte';

const { discoverState, deckState, launchRadioMock } = vi.hoisted(() => ({
	discoverState: {
		data: undefined as DiscoverResponseV3 | undefined,
		isLoading: false,
		isFetching: false,
		isRefetching: false,
		error: null as Error | null,
		refetch: () => {}
	},
	deckState: { shouldThrow: false },
	launchRadioMock: vi.fn().mockResolvedValue(true)
}));

// The v3 home read is the page's only data input; the feed below is a real
// v3 payload and flows through the real toDiscoverResponseV1 adapter.
vi.mock('$lib/queries/discover/DiscoverV3Queries.svelte', () => ({
	getDiscoverHomeV3Query: () => discoverState,
	// RadioCard mounts its own detail read when a radio shelf renders; the
	// station-identity test needs it quiet, not fetching.
	getDiscoverRadioV3Query: () => ({ data: undefined, isFetching: false })
}));
vi.mock('$lib/queries/discover/DiscoverV3Mutations.svelte', () => ({
	getIgnoreDiscoveryV3Mutation: () => ({ mutateAsync: vi.fn().mockResolvedValue(undefined) }),
	getRefreshDiscoverV3Mutation: () => ({ mutateAsync: vi.fn().mockResolvedValue(undefined) }),
	useDiscoverActivityV3: vi.fn()
}));
vi.mock('$lib/queries/section-prefs/SectionPrefsQuery.svelte', () => ({
	getSectionPrefsQuery: () => ({ data: undefined, isLoading: false })
}));
// AlbumCardOverlay (on every in-library album card) reads download access;
// allow it so the shelf renders without a QueryClientProvider.
vi.mock('$lib/queries/local/LocalQueries.svelte', () => ({
	getDownloadAccessQuery: () => ({ data: { allowed: true } })
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined),
	setQueryDataWithPersister: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/api/client', () => {
	class ApiError extends Error {}
	class SessionExpiredError extends ApiError {}
	return {
		ApiError,
		SessionExpiredError,
		api: { global: { get: vi.fn(), post: vi.fn().mockResolvedValue({}) } }
	};
});
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'u1' }, isAdmin: false }
}));
vi.mock('$lib/player/launchRadio', () => ({
	launchRadio: launchRadioMock
}));
// stub the deck fetch; optional failure exercises the section boundary
vi.mock('$lib/components/discover/DiscoverQueueDeck.svelte', () => ({
	default: function () {
		if (deckState.shouldThrow) throw new Error('deck exploded');
	}
}));
vi.mock('$lib/components/PlaylistDiscoveryModal.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});

import DiscoverPage from './+page.svelte';

function emptyResponse(overrides: Partial<DiscoverResponseV3> = {}): DiscoverResponseV3 {
	return {
		because_you_listen_to: [],
		discover_queue_enabled: false,
		genre_artwork_schema_version: 'v2',
		service_prompts: [],
		daily_mixes: [],
		radio_sections: [],
		refreshing: false,
		service_status: null,
		...overrides
	};
}

describe('/discover degraded and error states (#147)', () => {
	beforeEach(() => {
		discoverState.data = undefined;
		discoverState.isLoading = false;
		discoverState.isFetching = false;
		discoverState.isRefetching = false;
		deckState.shouldThrow = false;
		launchRadioMock.mockClear();
	});

	it('shows a terminal degraded state instead of endless skeletons', async () => {
		discoverState.data = emptyResponse({
			service_status: { listenbrainz: 'degraded' }
		});
		await render(DiscoverPage);

		await expect
			.element(page.getByRole('heading', { name: 'Recommendations Unavailable' }))
			.toBeVisible();
		await expect.element(page.getByText(/Listenbrainz\s+is temporarily unavailable/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Retry Now/ })).toBeVisible();
	});

	it('explains a slow build while sources are degraded', async () => {
		discoverState.data = emptyResponse({
			refreshing: true,
			service_status: { listenbrainz: 'degraded' }
		});
		await render(DiscoverPage);

		await expect
			.element(page.getByText(/recommendations will appear here as each section becomes ready/i))
			.toBeVisible();
		await expect.element(page.getByText(/so this may take longer than usual/)).toBeVisible();
	});

	it('keeps stale recommendations interactive and clearly marks the background update', async () => {
		discoverState.data = emptyResponse({
			refreshing: true,
			section_status: { trending: 'updating' },
			globally_trending: {
				title: 'Globally Trending',
				type: 'artists',
				items: [
					{
						name: 'Cocteau Twins',
						mbid: null,
						local_id: null,
						image_url: null,
						listen_count: null,
						in_library: false
					}
				],
				source: null,
				fallback_message: null,
				connect_service: null
			}
		});
		await render(DiscoverPage);

		await expect.element(page.getByText('Cocteau Twins')).toBeVisible();
		await expect
			.element(page.getByRole('status', { name: 'Refreshing in the background' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('status', { name: 'Updating in the background' }))
			.toBeVisible();
	});

	it('falls back to the generic empty state when nothing is degraded', async () => {
		discoverState.data = emptyResponse({ discover_queue_enabled: true });
		await render(DiscoverPage);

		await expect.element(page.getByRole('heading', { name: 'Still Loading' })).toBeVisible();
	});

	it('a crashing section degrades to an inline error card instead of killing the page', async () => {
		deckState.shouldThrow = true;
		discoverState.data = emptyResponse({ discover_queue_enabled: true });
		await render(DiscoverPage);

		await expect.element(page.getByText('Something Went Wrong')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Try Again' })).toBeVisible();
	});

	it('renders content normally when nothing crashes', async () => {
		discoverState.data = emptyResponse({
			discover_queue_enabled: true,
			globally_trending: {
				title: 'Globally Trending',
				type: 'artists',
				items: [],
				source: null,
				fallback_message: null,
				connect_service: null
			}
		});
		await render(DiscoverPage);

		await expect.element(page.getByText('Because You Listened')).toBeVisible();
		await expect.element(page.getByText('Something Went Wrong')).not.toBeInTheDocument();
	});

	it('keeps a useful station identity when a cached detail response is empty', async () => {
		discoverState.data = emptyResponse({
			radio_sections: [
				{
					title: 'Radio: Cocteau Twins',
					type: 'albums',
					items: [],
					source: 'lastfm',
					fallback_message: null,
					connect_service: null,
					radio_seed_type: 'artist',
					radio_seed_id: '5882a127-6b1f-493a-a70f-7cfbbef01b2d'
				}
			]
		});
		await render(DiscoverPage);

		await expect.element(page.getByRole('heading', { name: 'Radio: Cocteau Twins' })).toBeVisible();
		await expect.element(page.getByText('Ready to play')).toBeVisible();
		await page.getByRole('button', { name: /Radio: Cocteau Twins radio/ }).click();
		await expect
			.element(page.getByText('The complete track list is built when you press play.'))
			.toBeVisible();
	});

	it('plays every displayed daily-mix album when one artist has several albums', async () => {
		discoverState.data = emptyResponse({
			daily_mixes: [
				{
					title: 'Daily Dream Mix',
					type: 'albums',
					items: [
						{
							mbid: 'album-one',
							name: 'Album One',
							artist_name: 'One Artist',
							artist_mbid: 'artist-one',
							image_url: null,
							release_date: null,
							listen_count: null,
							in_library: true
						},
						{
							mbid: 'album-two',
							name: 'Album Two',
							artist_name: 'One Artist',
							artist_mbid: 'artist-one',
							image_url: null,
							release_date: null,
							listen_count: null,
							in_library: true
						}
					],
					source: 'listenbrainz',
					fallback_message: null,
					connect_service: null
				}
			]
		});
		await render(DiscoverPage);

		await page.getByRole('button', { name: /Daily Dream Mix - 2 albums/ }).click();
		await page.getByRole('button', { name: 'Play all' }).click();

		expect(launchRadioMock).toHaveBeenCalledWith(
			{
				seed_type: 'items',
				items: [
					{
						artist_mbid: 'artist-one',
						artist_name: 'One Artist',
						album_mbid: 'album-one',
						album_name: 'Album One'
					},
					{
						artist_mbid: 'artist-one',
						artist_name: 'One Artist',
						album_mbid: 'album-two',
						album_name: 'Album Two'
					}
				]
			},
			false,
			{ shuffle: false, mode: undefined }
		);
	});
});
