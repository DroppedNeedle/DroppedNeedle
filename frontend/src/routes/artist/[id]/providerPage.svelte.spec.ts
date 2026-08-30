import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	fetchNextPage: vi.fn(),
	extended: {
		data: {} as { description?: string } | undefined,
		isLoading: false,
		isRefetching: false,
		error: null
	}
}));

function emptyComponent() {
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
}

vi.mock('$lib/components/ArtistHeaderSkeleton.svelte', emptyComponent);
vi.mock('$lib/components/AlbumGridSkeleton.svelte', emptyComponent);
vi.mock('$lib/components/ArtistWhereToBuy.svelte', emptyComponent);
vi.mock('$lib/components/ReleaseList.svelte', emptyComponent);
vi.mock('$lib/components/Toast.svelte', emptyComponent);
vi.mock('$lib/components/ArtistHero.svelte', emptyComponent);
vi.mock('$lib/components/SimilarArtistsCarousel.svelte', emptyComponent);
vi.mock('$lib/components/TopSongsList.svelte', emptyComponent);
vi.mock('$lib/components/TopAlbumsList.svelte', emptyComponent);
vi.mock('$lib/components/LastFmEnrichment.svelte', emptyComponent);
vi.mock('$lib/components/LibraryAlbumsCarousel.svelte', emptyComponent);
vi.mock('$lib/components/library/ArtistAppearancesSection.svelte', emptyComponent);
vi.mock('$lib/components/PageSectionToc.svelte', emptyComponent);
vi.mock('$lib/components/SimpleSourceSwitcher.svelte', emptyComponent);
vi.mock('$lib/components/ArtistPlaybackBar.svelte', emptyComponent);

vi.mock('$lib/queries/artist/ArtistQueries.svelte', () => ({
	getBasicArtistQuery: () => ({
		data: {
			name: 'Pagination Fixture',
			musicbrainz_id: 'artist-1',
			tags: [],
			aliases: [],
			external_links: [],
			in_library: false,
			appears_in_library: false
		},
		isLoading: false,
		isRefetching: false,
		error: null
	}),
	getExtendedArtistQuery: () => h.extended,
	getSimilarArtistsQuery: () => ({ data: { similar_artists: [] }, isLoading: false }),
	getArtistTopAlbumsQuery: () => ({ data: { albums: [] }, isLoading: false }),
	getArtistTopSongsQuery: () => ({ data: { songs: [] }, isLoading: false }),
	getArtistLastFmEnrichmentQuery: () => ({ data: undefined, isLoading: false }),
	getArtistReleasesInfiniteQuery: () => ({
		data: {
			pages: [
				{
					albums: [
						{
							id: 'release-1',
							title: 'First page release',
							year: 2026,
							in_library: false
						}
					],
					eps: [],
					singles: [],
					offset: 0,
					limit: 50,
					returned_count: 1,
					next_offset: 100,
					has_more: true,
					source_total_count: 240
				}
			]
		},
		isLoading: false,
		isFetchingNextPage: false,
		hasNextPage: true,
		fetchNextPage: (...args: unknown[]) => h.fetchNextPage(...args)
	}),
	updateArtistReleaseInCache: vi.fn()
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/downloads/DownloadMutations.svelte')>()),
	requestAlbum: () => ({
		mutateAsync: vi.fn().mockResolvedValue({ success: true }),
		isPending: false
	})
}));
vi.mock('$lib/stores/musicSource', () => ({
	isMusicSource: (value: unknown) => value === 'listenbrainz' || value === 'lastfm'
}));
vi.mock('$lib/stores/discographyDownload.svelte', () => ({
	discographyDownloadStore: { show: vi.fn() }
}));

import ProviderArtistPage from './ProviderArtistPage.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.extended = {
		data: {},
		isLoading: false,
		isRefetching: false,
		error: null
	};
});
afterEach(() => vi.useRealTimers());

it('does not fetch a second release page until the user asks for it', async () => {
	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	const button = page.getByRole('button', { name: 'Load more releases' });
	await expect.element(button).toBeVisible();
	vi.useFakeTimers();
	await vi.advanceTimersByTimeAsync(30_000);
	expect(h.fetchNextPage).not.toHaveBeenCalled();
	vi.useRealTimers();

	await button.click();
	expect(h.fetchNextPage).toHaveBeenCalledTimes(1);
});

it('renders available biography data even while the query still reports loading', async () => {
	h.extended = {
		data: { description: 'The finished biography.' },
		isLoading: true,
		isRefetching: false,
		error: null
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await expect.element(page.getByText('The finished biography.')).toBeVisible();
	await expect.element(page.getByTestId('artist-description-skeleton')).not.toBeInTheDocument();
});

it('keeps the biography skeleton while extended data is initially unavailable', async () => {
	h.extended = {
		data: undefined,
		isLoading: true,
		isRefetching: false,
		error: null
	};

	render(ProviderArtistPage, {
		data: { artistId: 'artist-1', primarySource: 'listenbrainz' }
	});

	await expect.element(page.getByTestId('artist-description-skeleton')).toBeInTheDocument();
});
