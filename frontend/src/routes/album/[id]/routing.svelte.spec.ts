import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	goto: vi.fn(),
	cache: vi.fn().mockResolvedValue(undefined),
	localView: vi.fn(),
	providerView: vi.fn(),
	album: {
		id: 'local-album-id',
		musicbrainz_release_group_id: 'provider-album-id' as string | null
	},
	// Mocked TanStack result the dispatcher reads. `isFetched` is the monotonic
	// flag the skeleton gate depends on (see the not-in-library cases below).
	query: { data: undefined as unknown, isLoading: false, isFetched: true }
}));

vi.mock('$app/navigation', () => ({
	goto: (...args: unknown[]) => h.goto(...args)
}));

vi.mock('./LocalAlbumPage.svelte', () => {
	const Component = function () {
		h.localView();
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('./ProviderAlbumPage.svelte', () => {
	const Component = function () {
		h.providerView();
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryQueries.svelte')>()),
	getLibraryAlbumDetailQuery: () => h.query,
	cacheCanonicalLibraryAlbumDetail: (...args: unknown[]) => h.cache(...args)
}));

import AlbumPage from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.album.musicbrainz_release_group_id = 'provider-album-id';
	h.query = { data: h.album, isLoading: false, isFetched: true };
});

it('keeps a linked album on its MusicBrainz release-group route', async () => {
	render(AlbumPage, {
		props: { data: { albumId: 'provider-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.goto).not.toHaveBeenCalled());
	expect(h.cache).not.toHaveBeenCalled();
	expect(h.providerView).toHaveBeenCalled();
	expect(h.localView).not.toHaveBeenCalled();
});

it('replaces a linked local route with its MusicBrainz release-group route', async () => {
	render(AlbumPage, {
		props: { data: { albumId: 'local-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => {
		expect(h.goto).toHaveBeenCalledWith('/album/provider-album-id', {
			replaceState: true
		});
	});
	expect(h.cache).toHaveBeenCalledWith(expect.objectContaining({ id: 'local-album-id' }));
	expect(h.providerView).not.toHaveBeenCalled();
	expect(h.localView).not.toHaveBeenCalled();
});

it('keeps a local-only album on its local route', async () => {
	h.album.musicbrainz_release_group_id = null;
	render(AlbumPage, {
		props: { data: { albumId: 'local-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.goto).not.toHaveBeenCalled());
	expect(h.localView).toHaveBeenCalled();
	expect(h.providerView).not.toHaveBeenCalled();
});

it('shows the loading skeleton only on the first library fetch', async () => {
	// Nothing resolved yet: no data, no error, first fetch in flight.
	h.query = { data: undefined, isLoading: true, isFetched: false };
	render(AlbumPage, {
		props: { data: { albumId: 'unowned-album-mbid' } }
	} as unknown as Parameters<typeof render>[1]);

	// Skeleton branch: neither the local nor the provider page is mounted.
	await vi.waitFor(() => expect(h.providerView).not.toHaveBeenCalled());
	expect(h.localView).not.toHaveBeenCalled();
	expect(h.goto).not.toHaveBeenCalled();
});

it('renders the provider page for an unowned album while its 404 library query refetches', async () => {
	// GH-339 / GH-341: for an album not in the library the detail endpoint 404s.
	// ProviderAlbumPage re-observes that same query for its MusicBrainz-down
	// fallback, so the errored query is refetched and reports isLoading=true with
	// no data. Gating the skeleton on isLoading alone would swap back to the
	// skeleton, unmount the provider page, cancel the refetch and remount - an
	// infinite loop. isFetched stays true once the query has resolved once, so the
	// provider page must stay put.
	h.query = { data: undefined, isLoading: true, isFetched: true };
	render(AlbumPage, {
		props: { data: { albumId: 'unowned-album-mbid' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.providerView).toHaveBeenCalled());
	expect(h.localView).not.toHaveBeenCalled();
	expect(h.goto).not.toHaveBeenCalled();
});
