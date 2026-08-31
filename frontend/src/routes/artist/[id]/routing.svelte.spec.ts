import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	goto: vi.fn(),
	cache: vi.fn().mockResolvedValue(undefined),
	localView: vi.fn(),
	providerView: vi.fn(),
	artist: {
		id: 'local-artist-id',
		musicbrainz_artist_id: 'provider-artist-id' as string | null
	},
	// Mocked TanStack result the dispatcher reads. `isFetched` is the monotonic
	// flag the skeleton gate depends on (see the not-in-library cases below).
	query: { data: undefined as unknown, isLoading: false, isFetched: true }
}));

vi.mock('$app/navigation', () => ({
	goto: (...args: unknown[]) => h.goto(...args)
}));

vi.mock('./LocalArtistPage.svelte', () => {
	const Component = function () {
		h.localView();
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('./ProviderArtistPage.svelte', () => {
	const Component = function () {
		h.providerView();
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryQueries.svelte')>()),
	getLibraryArtistDetailQuery: () => h.query,
	cacheCanonicalLibraryArtistDetail: (...args: unknown[]) => h.cache(...args)
}));

import type { MusicSource } from '$lib/stores/musicSource';
import ArtistPage from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.artist.musicbrainz_artist_id = 'provider-artist-id';
	h.query = { data: h.artist, isLoading: false, isFetched: true };
});

it('keeps a linked artist on its MusicBrainz route', async () => {
	render(ArtistPage, {
		props: {
			data: {
				artistId: 'provider-artist-id',
				primarySource: 'listenbrainz' as MusicSource
			}
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.goto).not.toHaveBeenCalled());
	expect(h.cache).not.toHaveBeenCalled();
	expect(h.providerView).toHaveBeenCalled();
	expect(h.localView).not.toHaveBeenCalled();
});

it('replaces a linked local route with its MusicBrainz route', async () => {
	render(ArtistPage, {
		props: {
			data: {
				artistId: 'local-artist-id',
				primarySource: 'listenbrainz' as MusicSource
			}
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => {
		expect(h.goto).toHaveBeenCalledWith('/artist/provider-artist-id', {
			replaceState: true
		});
	});
	expect(h.cache).toHaveBeenCalledWith(expect.objectContaining({ id: 'local-artist-id' }));
	expect(h.providerView).not.toHaveBeenCalled();
	expect(h.localView).not.toHaveBeenCalled();
});

it('keeps a local-only artist on its local route', async () => {
	h.artist.musicbrainz_artist_id = null;
	render(ArtistPage, {
		props: {
			data: {
				artistId: 'local-artist-id',
				primarySource: 'listenbrainz' as MusicSource
			}
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.goto).not.toHaveBeenCalled());
	expect(h.localView).toHaveBeenCalled();
	expect(h.providerView).not.toHaveBeenCalled();
});

it('shows the loading skeleton only on the first library fetch', async () => {
	// Nothing resolved yet: no data, no error, first fetch in flight.
	h.query = { data: undefined, isLoading: true, isFetched: false };
	render(ArtistPage, {
		props: {
			data: {
				artistId: 'unowned-artist-mbid',
				primarySource: 'listenbrainz' as MusicSource
			}
		}
	} as unknown as Parameters<typeof render>[1]);

	// Skeleton branch: neither the local nor the provider page is mounted.
	await vi.waitFor(() => expect(h.providerView).not.toHaveBeenCalled());
	expect(h.localView).not.toHaveBeenCalled();
	expect(h.goto).not.toHaveBeenCalled();
});

it('renders the provider page for an unowned artist while its 404 library query refetches', async () => {
	// GH-339 / GH-341: for an artist not in the library the detail endpoint 404s.
	// ProviderArtistPage re-observes that same query for its MusicBrainz-down
	// fallback, so the errored query is refetched and reports isLoading=true with
	// no data. Gating the skeleton on isLoading alone would swap back to the
	// skeleton, unmount the provider page, cancel the refetch and remount - an
	// infinite loop. isFetched stays true once the query has resolved once, so the
	// provider page must stay put.
	h.query = { data: undefined, isLoading: true, isFetched: true };
	render(ArtistPage, {
		props: {
			data: {
				artistId: 'unowned-artist-mbid',
				primarySource: 'listenbrainz' as MusicSource
			}
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.providerView).toHaveBeenCalled());
	expect(h.localView).not.toHaveBeenCalled();
	expect(h.goto).not.toHaveBeenCalled();
});
