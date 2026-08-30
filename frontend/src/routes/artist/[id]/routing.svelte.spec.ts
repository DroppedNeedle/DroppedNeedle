import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	goto: vi.fn(),
	cache: vi.fn().mockResolvedValue(undefined),
	localView: vi.fn(),
	providerView: vi.fn(),
	localDetailRequest: vi.fn(),
	localDetail404: false,
	artist: {
		id: 'local-artist-id',
		musicbrainz_artist_id: 'provider-artist-id' as string | null
	}
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
	const Component = function (_anchor: unknown, props: { data: unknown; localArtist?: unknown }) {
		h.providerView(props);
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryQueries.svelte')>()),
	getLibraryArtistDetailQuery: (...args: unknown[]) => {
		h.localDetailRequest(...args);
		return h.localDetail404
			? { data: undefined, isLoading: false, isError: true, error: new Error('404') }
			: { data: h.artist, isLoading: false, isError: false, error: null };
	},
	cacheCanonicalLibraryArtistDetail: (...args: unknown[]) => h.cache(...args)
}));

import type { MusicSource } from '$lib/stores/musicSource';
import ArtistPage from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.artist.musicbrainz_artist_id = 'provider-artist-id';
	h.localDetail404 = false;
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
	expect(h.providerView).toHaveBeenCalledWith(expect.objectContaining({ localArtist: h.artist }));
	expect(h.localView).not.toHaveBeenCalled();
});

it('mounts the provider once when local detail returns 404', async () => {
	h.localDetail404 = true;
	render(ArtistPage, {
		props: {
			data: {
				artistId: 'provider-artist-id',
				primarySource: 'listenbrainz' as MusicSource
			}
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.providerView).toHaveBeenCalledTimes(1));
	expect(h.localDetailRequest).toHaveBeenCalledTimes(1);
	expect(h.providerView).toHaveBeenCalledWith(expect.objectContaining({ localArtist: undefined }));
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
