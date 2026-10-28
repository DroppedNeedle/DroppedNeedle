import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { GenreArtwork } from '$lib/queries/discover/types';

import GenreGrid from './GenreGrid.svelte';

// With the lazy loader off, each tile keeps the URL it would fetch in data-src.
vi.mock('$lib/utils/lazyImage', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	lazyImage: () => ({ destroy() {} })
}));

function albumId(genre: number, index: number): string {
	return `00000000-0000-4000-8000-${String(genre * 10 + index).padStart(12, '0')}`;
}

function collage(genre: number, count: number): GenreArtwork {
	return {
		kind: 'collage',
		version: '1',
		albums: Array.from({ length: count }, (_, index) => ({
			album_id: albumId(genre, index),
			album_title: `Album ${index}`,
			album_artist_name: null,
			cover_version: 3
		}))
	};
}

describe('genre grid', () => {
	it('asks for small renditions of library art, never the original files', async () => {
		fakeApi({});
		const genres = Array.from({ length: 20 }, (_, i) => ({ name: `Genre ${i}` }));
		const artwork: Record<string, GenreArtwork> = {};
		genres.forEach((genre, i) => {
			artwork[genre.name] = collage(i, i === 0 ? 1 : 4);
		});

		await renderWithQuery(GenreGrid, { title: 'Genres', genres, genreArtwork: artwork });

		const images = page.getByTestId('genre-artwork-image');
		await expect.element(images.first()).toBeInTheDocument();
		const urls = images
			.elements()
			.map((img) => img.getAttribute('data-src') ?? img.getAttribute('src') ?? '');
		expect(urls).toHaveLength(1 + 19 * 4);
		expect(urls[0]).toContain('size=500');
		for (const url of urls.slice(1)) expect(url).toContain('size=250');
	});
});
