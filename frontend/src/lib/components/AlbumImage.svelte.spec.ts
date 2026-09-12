import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import AlbumImage from './AlbumImage.svelte';

describe('AlbumImage cached-local mode', () => {
	it('uses only the stable local route for a UUID-shaped album ID', async () => {
		await render(AlbumImage, {
			props: {
				albumId: 'b1392450-e666-3926-a536-22c65f834433',
				coverVersion: 9,
				alt: 'Local cover',
				lazy: false
			}
		} as Parameters<typeof render<typeof AlbumImage>>[1]);

		await expect
			.element(page.getByAltText('Local cover'))
			.toHaveAttribute(
				'src',
				'/api/v1/library/albums/b1392450-e666-3926-a536-22c65f834433/artwork/cached?v=9'
			);
	});

	it('treats a local miss as terminal with no warming retry', async () => {
		vi.useFakeTimers();
		await render(AlbumImage, {
			props: { albumId: 'local-album', coverVersion: 2, alt: 'Local cover', lazy: false }
		} as Parameters<typeof render<typeof AlbumImage>>[1]);
		const image = page.getByAltText('Local cover');
		await expect.element(image).toBeInTheDocument();

		image.element().dispatchEvent(new Event('error'));
		await vi.advanceTimersByTimeAsync(60_000);

		await expect.element(page.getByAltText('Local cover')).not.toBeInTheDocument();
		vi.useRealTimers();
	});
});
