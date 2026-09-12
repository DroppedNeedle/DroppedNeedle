import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import ViewMoreAlbumCard from './ViewMoreAlbumCard.svelte';
import ViewMoreArtistCard from './ViewMoreArtistCard.svelte';

describe('Search view-more artwork', () => {
	it('offers right-sized WebP variants for the artist card', async () => {
		await render(ViewMoreArtistCard);

		const image = page.getByTestId('view-more-artist-background');
		await expect.element(image).toHaveAttribute('src', '/img/artist_bg-250.webp');
		await expect
			.element(image)
			.toHaveAttribute('srcset', '/img/artist_bg-250.webp 250w, /img/artist_bg-500.webp 500w');
		await expect.element(image).toHaveAttribute('sizes', '200px');
	});

	it('offers right-sized WebP variants for the album card', async () => {
		await render(ViewMoreAlbumCard);

		const image = page.getByTestId('view-more-album-background');
		await expect.element(image).toHaveAttribute('src', '/img/album_bg-250.webp');
		await expect
			.element(image)
			.toHaveAttribute('srcset', '/img/album_bg-250.webp 250w, /img/album_bg-500.webp 500w');
		await expect.element(image).toHaveAttribute('sizes', '200px');
	});
});
