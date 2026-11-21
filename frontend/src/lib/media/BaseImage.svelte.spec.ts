import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import BaseImage from './BaseImage.svelte';

// Keep lazy images on their data-src attributes without firing network requests.
vi.mock('$lib/media/lazyImage', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	lazyImage: () => ({ destroy() {} })
}));

const REMOTE_ALBUM_ID = 'al-4f8b92c7e1a03d651209ab';
const REMOTE_COVER_PATH = '/covers/remote/navidrome/al-4f8b92c7e1a03d651209ab';

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
