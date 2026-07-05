import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';

let mockDirectRemoteEnabled = true;

vi.mock('$lib/stores/imageSettings', () => ({
	imageSettingsStore: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb({ directRemoteImagesEnabled: mockDirectRemoteEnabled });
			return () => {};
		}),
		load: vi.fn()
	}
}));

import BaseImage from './BaseImage.svelte';

const validMbid = 'b1392450-e666-3926-a536-22c65f834433';
const cdnUrl = 'https://r2.theaudiodb.com/images/media/artist/thumb/abc123.jpg';

function renderComponent(
	overrides: Partial<{
		mbid: string;
		remoteUrl: string | null;
		customUrl: string | null;
		imageType: 'album' | 'artist';
		size: 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'hero' | 'full';
		lazy: boolean;
		alt: string;
	}> = {}
) {
	return render(BaseImage, {
		props: {
			mbid: overrides.mbid ?? validMbid,
			remoteUrl: overrides.remoteUrl ?? null,
			customUrl: overrides.customUrl ?? null,
			imageType: overrides.imageType ?? 'album',
			size: overrides.size ?? 'md',
			lazy: overrides.lazy ?? false,
			alt: overrides.alt ?? 'Test Image'
		}
	} as Parameters<typeof render<typeof BaseImage>>[1]);
}

describe('BaseImage.svelte - remoteUrl', () => {
	beforeEach(() => {
		mockDirectRemoteEnabled = true;
	});

	it('renders CDN URL with referrerpolicy when remoteUrl is set', async () => {
		renderComponent({ remoteUrl: cdnUrl });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).toHaveAttribute('referrerpolicy', 'no-referrer');
		await expect.element(img).toHaveAttribute('src', `${cdnUrl}/small`);
	});

	it('appends /medium suffix for lg size', async () => {
		renderComponent({ remoteUrl: cdnUrl, size: 'lg' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toHaveAttribute('src', `${cdnUrl}/medium`);
	});

	it('uses original URL for full size', async () => {
		renderComponent({ remoteUrl: cdnUrl, size: 'full' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toHaveAttribute('src', cdnUrl);
	});

	it('renders proxy img without referrerpolicy when remoteUrl is null', async () => {
		renderComponent({ remoteUrl: null, imageType: 'album' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).not.toHaveAttribute('referrerpolicy');
	});

	it('renders proxy URL for artist when remoteUrl is null', async () => {
		renderComponent({ remoteUrl: null, imageType: 'artist' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
	});

	it('renders proxy URL when remoteUrl is set but setting is disabled', async () => {
		mockDirectRemoteEnabled = false;
		renderComponent({ remoteUrl: cdnUrl, imageType: 'artist' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
		await expect.element(img).not.toHaveAttribute('referrerpolicy');
	});

	it('falls back to proxy URL when remote image errors', async () => {
		renderComponent({ remoteUrl: cdnUrl, imageType: 'artist' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toHaveAttribute('src', `${cdnUrl}/small`);

		img.element().dispatchEvent(new Event('error'));

		await expect
			.element(page.getByAltText('Test Image'))
			.toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
	});
});

describe('BaseImage.svelte - warming skeleton', () => {
	beforeEach(() => {
		mockDirectRemoteEnabled = true;
	});

	it('shows a shimmer skeleton while the cover is loading', async () => {
		renderComponent({ imageType: 'album' });

		await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
	});

	it('holds the skeleton on the lazy path (placeholder gif load must not hide it)', async () => {
		// The lazy <img> mounts with a 1x1 data-URI gif whose load fires immediately; it must not
		// count as the cover loading, or the skeleton would vanish on the default grid path.
		renderComponent({ imageType: 'album', lazy: true });

		await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
	});

	it('holds the skeleton after a warming error instead of dropping to the placeholder', async () => {
		renderComponent({ imageType: 'album', lazy: false });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();

		// A cold cover comes back as 202 (warming) -> the <img> fires error. We must keep the
		// skeleton and poll it in, not settle on the static placeholder.
		img.element().dispatchEvent(new Event('error'));

		await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
	});
});
