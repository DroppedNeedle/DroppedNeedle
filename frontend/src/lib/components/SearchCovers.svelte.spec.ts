import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import {
	toV1Album,
	toV1Artist,
	type SearchResultItemV3
} from '$lib/queries/search/SearchV3Adapters';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import AlbumCard from './AlbumCard.svelte';
import SearchArtistCard from './SearchArtistCard.svelte';

// The lazy loader sets an image's src once its card scrolls into view. With it
// off, each card keeps the URL it builds in data-src and fetches nothing.
vi.mock('$lib/utils/lazyImage', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	lazyImage: () => ({ destroy() {} })
}));

const LINKED_ALBUM_MBID = 'b1392450-e666-3926-a536-22c65f834433';
const LINKED_ARTIST_MBID = 'a74b1b7f-71a5-4011-9441-d0b5e4122711';
const UNLINKED_ALBUM_ID = '8a1d6e2b-3c4f-4d5e-9f60-7b8c9d0e1f2a';

// Search rows as the server sends them: a library row carries its local id,
// and a MusicBrainz id only when it has one.
const linkedAlbum: SearchResultItemV3 = {
	id: '4f2a1c9e-7b3d-4e8f-9a6c-1d2e3f4a5b6c',
	musicbrainz_id: LINKED_ALBUM_MBID,
	kind: 'album',
	title: 'OK Computer',
	artist: 'Radiohead',
	year: 1997,
	in_library: true,
	requested: false,
	score: 100
};
const unlinkedAlbum: SearchResultItemV3 = {
	id: UNLINKED_ALBUM_ID,
	kind: 'album',
	title: 'Home Demos',
	in_library: true,
	requested: false,
	score: 100
};
const linkedArtist: SearchResultItemV3 = {
	id: '2e7c9b4a-1d3f-4a8e-b6c5-0f9e8d7c6b5a',
	musicbrainz_id: LINKED_ARTIST_MBID,
	kind: 'artist',
	title: 'Radiohead',
	in_library: true,
	requested: false,
	score: 100
};
const unlinkedArtist: SearchResultItemV3 = {
	id: '6b0f3d9e-2a7c-4e1b-8d4f-5c6a7b8e9d0f',
	kind: 'artist',
	title: 'Home Band',
	in_library: true,
	requested: false,
	score: 100
};

describe('search covers', () => {
	it('never sends a library id as a MusicBrainz id, and a linked album asks for its real cover', async () => {
		fakeApi({});

		await renderWithQuery(SearchArtistCard, { artist: toV1Artist(unlinkedArtist) });
		await expect.element(page.getByRole('heading', { name: 'Home Band' })).toBeInTheDocument();
		await expect
			.element(page.getByRole('img', { name: 'Home Band' }))
			.toHaveAttribute(
				'data-src',
				expect.stringContaining(`/library/artists/${unlinkedArtist.id}/artwork`)
			);

		await renderWithQuery(SearchArtistCard, { artist: toV1Artist(linkedArtist) });
		await expect
			.element(page.getByRole('img', { name: 'Radiohead' }))
			.toHaveAttribute('data-src', expect.stringContaining(`/covers/artist/${LINKED_ARTIST_MBID}`));

		await renderWithQuery(AlbumCard, { album: toV1Album(linkedAlbum) });
		await expect
			.element(page.getByRole('img', { name: 'OK Computer' }))
			.toHaveAttribute(
				'data-src',
				expect.stringContaining(`/covers/release-group/${LINKED_ALBUM_MBID}`)
			);

		await renderWithQuery(AlbumCard, { album: toV1Album(unlinkedAlbum) });
		await expect
			.element(page.getByRole('img', { name: 'Home Demos' }))
			.toHaveAttribute(
				'data-src',
				expect.stringContaining(`/library/albums/${UNLINKED_ALBUM_ID}/artwork`)
			);
	});
});
