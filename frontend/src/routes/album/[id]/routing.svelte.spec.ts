import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	goto: vi.fn(),
	cache: vi.fn().mockResolvedValue(undefined),
	localView: vi.fn(),
	providerView: vi.fn(),
	localDetailRequest: vi.fn(),
	localDetail404: false,
	album: {
		id: 'local-album-id',
		release_group_mbid: 'provider-album-id' as string | null
	}
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
	const Component = function (_anchor: unknown, props: { data: unknown; localAlbum?: unknown }) {
		h.providerView(props);
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryV3Queries.svelte', () => ({
	getLibraryAlbumDetailV3Query: (...args: unknown[]) => {
		h.localDetailRequest(...args);
		return h.localDetail404
			? { data: undefined, isLoading: false, isError: true, error: new Error('404') }
			: { data: h.album, isLoading: false, isError: false, error: null };
	},
	cacheCanonicalLibraryAlbumDetailV3: (...args: unknown[]) => h.cache(...args)
}));

import AlbumPage from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.album.release_group_mbid = 'provider-album-id';
	h.localDetail404 = false;
});

it('keeps a linked album on its MusicBrainz release-group route', async () => {
	await render(AlbumPage, {
		props: { data: { albumId: 'provider-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.goto).not.toHaveBeenCalled());
	expect(h.cache).not.toHaveBeenCalled();
	expect(h.providerView).toHaveBeenCalledWith(expect.objectContaining({ localAlbum: h.album }));
	expect(h.localView).not.toHaveBeenCalled();
});

it('mounts the provider once when local detail returns 404', async () => {
	h.localDetail404 = true;
	await render(AlbumPage, {
		props: { data: { albumId: 'provider-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.providerView).toHaveBeenCalledTimes(1));
	expect(h.localDetailRequest).toHaveBeenCalledTimes(1);
	expect(h.providerView).toHaveBeenCalledWith(expect.objectContaining({ localAlbum: undefined }));
});

it('replaces a linked local route with its MusicBrainz release-group route', async () => {
	await render(AlbumPage, {
		props: { data: { albumId: 'local-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => {
		expect(h.goto).toHaveBeenCalledWith('/album/provider-album-id', {
			replaceState: true
		});
	});
	expect(h.cache).toHaveBeenCalledWith(
		undefined,
		expect.objectContaining({ id: 'local-album-id' })
	);
	expect(h.providerView).not.toHaveBeenCalled();
	expect(h.localView).not.toHaveBeenCalled();
});

it('keeps a local-only album on its local route', async () => {
	h.album.release_group_mbid = null;
	await render(AlbumPage, {
		props: { data: { albumId: 'local-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.goto).not.toHaveBeenCalled());
	expect(h.localView).toHaveBeenCalled();
	expect(h.providerView).not.toHaveBeenCalled();
});
