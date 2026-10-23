import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
const apiMock = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('$lib/api/client', () => ({ api: { global: { get: apiMock.get } } }));
import AlbumImage from './AlbumImage.svelte';

beforeEach(() => {
	apiMock.get.mockReset();
	apiMock.get.mockImplementation(async () => new Response('', { status: 404 }));
});

describe('AlbumImage cached-local mode', () => {
	it('reads unknown local artwork by stable ID without provider requests or responsive fallback', async () => {
		await render(AlbumImage, {
			props: {
				mbid: 'b1392450-e666-3926-a536-22c65f834433',
				source: 'local',
				available: false,
				alt: 'Unlinked cover',
				lazy: false,
				requestSize: 250,
				responsiveSizes: '250px'
			}
		} as Parameters<typeof render<typeof AlbumImage>>[1]);
		const image = page.getByAltText('Unlinked cover');
		await expect
			.element(image)
			.toHaveAttribute(
				'src',
				'/api/v1/library/albums/b1392450-e666-3926-a536-22c65f834433/artwork/local'
			);
		await expect.element(image).not.toHaveAttribute('srcset');
		image.element().dispatchEvent(new Event('error'));
		await expect.element(page.getByAltText('Unlinked cover')).not.toBeInTheDocument();
	});
	it('uses bounded warming checks for a stable local route redirected to a provider', async () => {
		vi.useFakeTimers();
		try {
			apiMock.get.mockImplementation(async () => new Response('', { status: 202 }));
			await render(AlbumImage, {
				props: {
					mbid: 'local-warming',
					source: 'local',
					available: true,
					alt: 'Warming local art',
					lazy: false
				}
			} as Parameters<typeof render<typeof AlbumImage>>[1]);
			page.getByAltText('Warming local art').element().dispatchEvent(new Event('error'));
			await vi.advanceTimersByTimeAsync(1500);
			expect(apiMock.get).toHaveBeenCalledWith(
				'/api/v1/library/albums/local-warming/artwork/local?_r=1',
				expect.objectContaining({ raw: true })
			);
		} finally {
			vi.useRealTimers();
		}
	});

	it('uses stable local IDs even when an artwork association is known', async () => {
		await render(AlbumImage, {
			props: {
				mbid: 'b1392450-e666-3926-a536-22c65f834433',
				source: 'local',
				available: true,
				alt: 'Known cover',
				lazy: false
			}
		} as Parameters<typeof render<typeof AlbumImage>>[1]);
		await expect
			.element(page.getByAltText('Known cover'))
			.toHaveAttribute(
				'src',
				'/api/v1/library/albums/b1392450-e666-3926-a536-22c65f834433/artwork/local'
			);
	});
	it('keeps provider-only images on the existing provider path', async () => {
		await render(AlbumImage, {
			props: {
				mbid: 'b1392450-e666-3926-a536-22c65f834433',
				source: 'provider',
				alt: 'Provider cover',
				lazy: false
			}
		} as Parameters<typeof render<typeof AlbumImage>>[1]);
		await expect
			.element(page.getByAltText('Provider cover'))
			.toHaveAttribute(
				'src',
				'/api/v1/covers/release-group/b1392450-e666-3926-a536-22c65f834433?size=250'
			);
	});
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
