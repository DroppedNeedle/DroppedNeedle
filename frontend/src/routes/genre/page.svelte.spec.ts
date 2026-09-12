import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	genre: {
		pages: [
			{
				genre: 'Electronic',
				genre_artwork: { kind: 'gradient', albums: [], version: 'v2:0:test' },
				library: {
					artists: [
						{ name: 'Same Name', mbid: null, local_id: 'artist-local-1', in_library: true },
						{ name: 'Same Name', mbid: null, local_id: 'artist-local-2', in_library: true }
					],
					albums: [
						{
							name: 'Same Album',
							artist_name: 'First Artist',
							mbid: null,
							local_id: 'album-local-1',
							in_library: true
						},
						{
							name: 'Same Album',
							artist_name: 'Second Artist',
							mbid: null,
							local_id: 'album-local-2',
							in_library: true
						}
					],
					artist_count: 2,
					album_count: 2
				},
				popular: { artists: [], albums: [], has_more_artists: false, has_more_albums: false },
				artists: []
			}
		]
	}
}));

vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/genre?name=Electronic') } }));
vi.mock('$lib/queries/genre/GenreQueries.svelte', () => ({
	getGenreDetailQuery: () => ({
		data: h.genre,
		isPending: false,
		isError: false,
		isFetchingNextPage: false,
		hasNextPage: false,
		fetchNextPage: vi.fn()
	}),
	getGenreAlbumPagesQuery: () => ({
		data: undefined,
		isPending: false,
		isError: false,
		isFetching: false,
		hasNextPage: false,
		fetchNextPage: vi.fn(),
		refetch: vi.fn()
	})
}));

import GenrePage from './+page.svelte';

describe('genre local catalog routing', () => {
	it('renders same-name local-only artists and albums as distinct stable links', async () => {
		await render(GenrePage);

		const artists = page.getByRole('link', { name: /Same Name/ }).all();
		const albums = page.getByRole('link', { name: /Same Album/ }).all();
		expect(artists).toHaveLength(2);
		expect(albums).toHaveLength(2);
		await expect.element(artists[0]).toHaveAttribute('href', '/artist/artist-local-1');
		await expect.element(artists[1]).toHaveAttribute('href', '/artist/artist-local-2');
		await expect.element(albums[0]).toHaveAttribute('href', '/album/album-local-1');
		await expect.element(albums[1]).toHaveAttribute('href', '/album/album-local-2');
	});
});
