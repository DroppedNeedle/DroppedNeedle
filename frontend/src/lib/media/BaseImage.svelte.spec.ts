import { page } from 'vitest/browser';
import { describe, expect, it, onTestFinished, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import BaseImage from './BaseImage.svelte';

// Keep lazy images on their data-src attributes without firing network
// requests; an eager image loads through the real action.
vi.mock('$lib/media/lazyImage', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/media/lazyImage')>();
	return {
		...actual,
		lazyImage: (img: HTMLImageElement, options?: { eager?: boolean }) =>
			options?.eager ? actual.lazyImage(img, options) : { destroy() {} }
	};
});

const REMOTE_ALBUM_ID = 'al-4f8b92c7e1a03d651209ab';
const REMOTE_COVER_PATH = '/covers/remote/navidrome/al-4f8b92c7e1a03d651209ab';
const RELEASE_GROUP = 'b1392450-e666-3926-a536-22c65f834433';

describe('BaseImage', () => {
	it('renders a placeholder instead of a cover request when a non-UUID album or artist has no customUrl', async () => {
		fakeApi({});

		await renderWithQuery(BaseImage, {
			mbid: REMOTE_ALBUM_ID,
			alt: 'Remote Album',
			imageType: 'album'
		});
		await expect.element(page.getByRole('img', { name: 'Remote Album' })).not.toBeInTheDocument();

		await renderWithQuery(BaseImage, {
			mbid: REMOTE_ALBUM_ID,
			alt: 'Empty Custom',
			imageType: 'album',
			customUrl: ''
		});
		await expect.element(page.getByRole('img', { name: 'Empty Custom' })).not.toBeInTheDocument();

		await renderWithQuery(BaseImage, {
			mbid: REMOTE_ALBUM_ID,
			alt: 'Remote Artist',
			imageType: 'artist'
		});
		await expect.element(page.getByRole('img', { name: 'Remote Artist' })).not.toBeInTheDocument();
	});

	it('lets a 176 px box asked at 500 pick from candidates up to 500, sized to the box', async () => {
		fakeApi({});
		document.body.style.width = '176px';
		onTestFinished(() => {
			document.body.style.removeProperty('width');
		});
		// The cover has no server here, so its image may fail and leave the
		// page: note the srcset and sizes the image is given as they are set.
		const given: { srcset: string; sizes: string | null }[] = [];
		const watch = new MutationObserver((records) => {
			for (const { target } of records) {
				if (target instanceof HTMLImageElement && target.getAttribute('srcset')) {
					given.push({
						srcset: target.getAttribute('srcset') ?? '',
						sizes: target.getAttribute('sizes')
					});
				}
			}
		});
		watch.observe(document.body, { attributes: true, attributeFilter: ['srcset'], subtree: true });
		onTestFinished(() => watch.disconnect());

		await renderWithQuery(BaseImage, {
			mbid: RELEASE_GROUP,
			alt: 'Home Shelf Cover',
			imageType: 'album',
			size: 'full',
			requestSize: 500,
			lazy: false
		});

		const cover = (size: number) => `/api/v3/covers/release-group/${RELEASE_GROUP}?size=${size}`;
		await expect
			.poll(() => given)
			.toEqual([
				{ srcset: `${cover(250)} 250w, ${cover(375)} 375w, ${cover(500)} 500w`, sizes: '176px' }
			]);
	});

	it('uses customUrl without emitting a release-group srcset for a non-UUID mbid', async () => {
		fakeApi({});

		await renderWithQuery(BaseImage, {
			mbid: REMOTE_ALBUM_ID,
			alt: 'Navidrome Cover',
			imageType: 'album',
			customUrl: REMOTE_COVER_PATH,
			requestSize: 250,
			responsiveSizes: '250px'
		});

		const img = page.getByRole('img', { name: 'Navidrome Cover' });
		await expect
			.element(img)
			.toHaveAttribute('data-src', expect.stringContaining(REMOTE_COVER_PATH));
		await expect.element(img).not.toHaveAttribute('data-srcset');
	});

	it('falls back to the placeholder when a remoteUrl fails on a non-UUID mbid with no customUrl', async () => {
		fakeApi({});

		await renderWithQuery(BaseImage, {
			mbid: REMOTE_ALBUM_ID,
			alt: 'Broken Remote Cover',
			imageType: 'album',
			remoteUrl: 'https://example.invalid/cover.jpg'
		});

		const img = page.getByRole('img', { name: 'Broken Remote Cover' });
		await expect
			.element(img)
			.toHaveAttribute('data-src', expect.stringContaining('https://example.invalid/cover.jpg'));
		img.element().dispatchEvent(new Event('error'));

		await expect.element(img).not.toBeInTheDocument();
	});
});
