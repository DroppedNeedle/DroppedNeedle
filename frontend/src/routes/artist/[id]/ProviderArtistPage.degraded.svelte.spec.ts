import { page } from '@vitest/browser/context';
import { beforeEach, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { render } from 'vitest-browser-svelte';

// Degraded MusicBrainz-outage fallback on the artist page: when the provider
// /artists/{id} fetch fails but the MB-free library detail query succeeded,
// the page must render the local artist surface (with an explanatory banner)
// instead of a whole-page error.

const h = vi.hoisted(() => ({
	localView: vi.fn(),
	libraryDetailObserver: vi.fn(),
	basicRefetch: vi.fn(),
	basicError: null as unknown,
	libraryArtist: {
		id: 'local-artist-id',
		name: 'Local Artist',
		musicbrainz_artist_id: 'mb-artist-id'
	} as Record<string, unknown> | null
}));

vi.mock('$lib/queries/artist/ArtistQueries.svelte', () => ({
	getBasicArtistQuery: () => ({
		data: null,
		isLoading: false,
		get error() {
			return h.basicError;
		},
		refetch: (...args: unknown[]) => h.basicRefetch(...args)
	}),
	getExtendedArtistQuery: () => ({ data: null, isLoading: false, error: null }),
	getSimilarArtistsQuery: () => ({ data: null, isLoading: false }),
	getArtistTopSongsQuery: () => ({ data: null, isLoading: false }),
	getArtistTopAlbumsQuery: () => ({ data: null, isLoading: false }),
	getArtistLastFmEnrichmentQuery: () => ({ data: null, isLoading: false }),
	updateArtistReleaseInCache: vi.fn(),
	getArtistReleasesInfiniteQuery: () => ({
		data: { pages: [] },
		isLoading: false,
		fetchNextPage: vi.fn()
	})
}));

vi.mock('./LocalArtistPage.svelte', () => {
	const Component = function (_anchor: unknown, props: { artistId: string }) {
		h.localView(props.artistId);
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryQueries.svelte')>()),
	getLibraryArtistDetailQuery: (...args: unknown[]) => {
		h.libraryDetailObserver(...args);
		return {
			data: h.libraryArtist,
			isLoading: false,
			error: null
		};
	}
}));

vi.mock('$lib/stores/library', () => ({
	libraryStore: {
		isInLibrary: () => false,
		isRequested: () => false,
		subscribe: () => () => {}
	}
}));

vi.mock('$lib/stores/discographyDownload.svelte', () => ({
	discographyDownloadStore: { subscribe: () => () => {} }
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestAlbum: () => ({ isPending: false, mutate: vi.fn() })
}));

vi.mock('runed', () => ({
	PersistedState: class {
		current: unknown;
		constructor(_key: string, initial: unknown) {
			this.current = initial;
		}
	}
}));

import ProviderArtistPage from './ProviderArtistPage.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.basicError = new ApiError(503, 'Service unavailable');
	h.libraryArtist = {
		id: 'local-artist-id',
		name: 'Local Artist',
		musicbrainz_artist_id: 'mb-artist-id'
	};
});

it('renders the local artist surface with a banner when the provider is unavailable', async () => {
	render(ProviderArtistPage, {
		props: {
			data: { artistId: 'mb-artist-id', primarySource: 'local' },
			localArtist: h.libraryArtist
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.localView).toHaveBeenCalledWith('local-artist-id'));
	expect(h.libraryDetailObserver).not.toHaveBeenCalled();
	await expect.element(page.getByText('MusicBrainz is unreachable')).toBeVisible();
});

it('renders a terminal not-found state for a provider 404 even with a local artist', async () => {
	h.basicError = new ApiError(404, 'Artist not found');
	render(ProviderArtistPage, {
		props: {
			data: { artistId: 'missing-artist-id', primarySource: 'local' },
			localArtist: h.libraryArtist
		}
	} as unknown as Parameters<typeof render>[1]);

	await expect.element(page.getByText('Artist not found.')).toBeVisible();
	expect(h.localView).not.toHaveBeenCalled();
	expect(h.libraryDetailObserver).not.toHaveBeenCalled();
});

it('offers an explicit retry when the provider is unavailable without a local artist', async () => {
	render(ProviderArtistPage, {
		props: { data: { artistId: 'unavailable-artist-id', primarySource: 'local' } }
	} as unknown as Parameters<typeof render>[1]);

	await expect.element(page.getByText('MusicBrainz is temporarily unavailable.')).toBeVisible();
	await page.getByRole('button', { name: 'Retry' }).click();
	expect(h.basicRefetch).toHaveBeenCalledTimes(1);
});

it('keeps other provider errors generic instead of using the local fallback', async () => {
	h.basicError = new ApiError(400, 'Bad request');
	render(ProviderArtistPage, {
		props: {
			data: { artistId: 'invalid-artist-id', primarySource: 'local' },
			localArtist: h.libraryArtist
		}
	} as unknown as Parameters<typeof render>[1]);

	await expect.element(page.getByText('Failed to load artist information.')).toBeVisible();
	expect(h.localView).not.toHaveBeenCalled();
	await expect.element(page.getByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
});
