import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { GenreArtwork as GenreArtworkModel } from '$lib/types';
import GenreArtwork from './GenreArtwork.svelte';

function artwork(count: number): GenreArtworkModel {
	return {
		kind: count ? 'collage' : 'gradient',
		version: `v2:${count}:test`,
		albums: Array.from({ length: count }, (_, index) => ({
			album_id: `00000000-0000-4000-8000-00000000000${index}`,
			album_title: `Album ${index}`,
			album_artist_name: `Artist ${index}`,
			cover_version: index + 1
		}))
	};
}

async function renderArtwork(count: number) {
	return await render(GenreArtwork, {
		props: { artwork: artwork(count), gradientClass: 'from-blue-500/90 to-cyan-700' }
	} as Parameters<typeof render<typeof GenreArtwork>>[1]);
}

describe('GenreArtwork.svelte', () => {
	it.each([0, 1, 2, 3, 4])('renders the deterministic %i-cover layout', async (count) => {
		await renderArtwork(count);

		await expect.element(page.getByTestId('genre-gradient')).toBeInTheDocument();
		if (count === 0) {
			await expect.element(page.getByTestId('genre-artwork-1')).not.toBeInTheDocument();
		} else {
			await expect.element(page.getByTestId(`genre-artwork-${count}`)).toBeInTheDocument();
			await expect
				.element(page.getByTestId('genre-artwork-cell').nth(count - 1))
				.toBeInTheDocument();
		}
	});

	it('keeps the gradient visible when a cached local cover disappears', async () => {
		await renderArtwork(1);
		const image = page.getByTestId('genre-artwork-image');
		await expect
			.element(image)
			.toHaveAttribute(
				'data-src',
				'/api/v1/library/albums/00000000-0000-4000-8000-000000000000/artwork/cached?v=1'
			);

		image.element().dispatchEvent(new Event('error'));

		await expect.element(image).not.toBeInTheDocument();
		await expect.element(page.getByTestId('genre-gradient')).toBeInTheDocument();
	});
});
