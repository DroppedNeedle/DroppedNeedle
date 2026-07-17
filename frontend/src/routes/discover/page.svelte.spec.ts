import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { DiscoverResponse } from '$lib/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const { discoverState, deckState, launchRadioMock } = vi.hoisted(() => ({
	discoverState: {
		data: undefined as Partial<DiscoverResponse> | undefined,
		isLoading: false,
		isFetching: false,
		isRefetching: false,
		error: null as Error | null,
		dataUpdatedAt: 0,
		refetch: () => {}
	},
	deckState: { shouldThrow: false },
	launchRadioMock: vi.fn().mockResolvedValue(true)
}));

vi.mock('$lib/queries/discover/DiscoverQuery.svelte', () => ({
	getDiscoverQuery: () => discoverState,
	getDiscoverQueryOptions: () => ({ queryKey: ['discover'], queryFn: () => ({}) }),
	getRadioQuery: () => ({
		data: {
			title: 'Radio',
			type: 'albums',
			items: [],
			source: 'lastfm',
			fallback_message: null,
			connect_service: null
		},
		isLoading: false,
		isFetching: false
	}),
	getPlaylistSuggestionsQuery: () => ({ data: undefined, isLoading: false })
}));
vi.mock('$lib/queries/section-prefs/SectionPrefsQuery.svelte', () => ({
	getSectionPrefsQuery: () => ({ data: undefined, isLoading: false })
}));
vi.mock('$lib/queries/discover/DiscoverMutations.svelte', () => ({
	getIgnoreDiscoveryMutation: () => ({ mutateAsync: vi.fn().mockResolvedValue(undefined) })
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

function emptyResponse(overrides: Partial<DiscoverResponse> = {}): Partial<DiscoverResponse> {
	return {
		because_you_listen_to: [],
		discover_queue_enabled: false,
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
		render(DiscoverPage);

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
		render(DiscoverPage);

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
		render(DiscoverPage);

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
		render(DiscoverPage);

		await expect.element(page.getByRole('heading', { name: 'Still Loading' })).toBeVisible();
	});

	it('a crashing section degrades to an inline error card instead of killing the page', async () => {
		deckState.shouldThrow = true;
		discoverState.data = emptyResponse({ discover_queue_enabled: true });
		render(DiscoverPage);

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
		render(DiscoverPage);

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
		render(DiscoverPage);

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
		render(DiscoverPage);

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
