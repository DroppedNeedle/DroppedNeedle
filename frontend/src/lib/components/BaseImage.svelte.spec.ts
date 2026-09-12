import { cdp, page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';

let mockDirectRemoteEnabled = true;
const warmMock = vi.hoisted(() => {
	type Update = { status: 'warming' } | { status: 'ready'; url: string } | { status: 'failed' };
	const listeners: Array<(update: Update) => void> = [];
	const watch = vi.fn((_url: string, listener: (update: Update) => void) => {
		listeners.push(listener);
		listener({ status: 'warming' });
		return vi.fn();
	});
	return { listeners, watch };
});

vi.mock('$lib/stores/imageSettings', () => ({
	imageSettingsStore: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb({ directRemoteImagesEnabled: mockDirectRemoteEnabled });
			return () => {};
		}),
		load: vi.fn()
	}
}));

vi.mock('$lib/utils/coverWarmCoordinator', () => ({
	COVER_VISUAL_SETTLE_MS: 6500,
	watchWarmingCover: warmMock.watch
}));

import BaseImage from './BaseImage.svelte';

const validMbid = 'b1392450-e666-3926-a536-22c65f834433';
const cdnUrl = 'https://r2.theaudiodb.com/images/media/artist/thumb/abc123.jpg';

interface FetchCdpSession {
	send(
		method: 'Fetch.enable',
		params: { patterns: Array<{ urlPattern: string }> }
	): Promise<unknown>;
	send(method: 'Fetch.disable'): Promise<unknown>;
}

// hold CDN requests so the remote img deterministically stalls: the fake key 404s whenever
// the real network beats the fake-timer clock, and then the stall path is never exercised
async function holdCdnRequests(): Promise<() => Promise<unknown>> {
	const session = cdp() as unknown as FetchCdpSession;
	await session.send('Fetch.enable', { patterns: [{ urlPattern: 'https://r2.theaudiodb.com/*' }] });
	return () => session.send('Fetch.disable');
}

async function renderComponent(
	overrides: Partial<{
		mbid: string;
		remoteUrl: string | null;
		customUrl: string | null;
		imageType: 'album' | 'artist';
		size: 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'hero' | 'full';
		requestSize: 250 | 500 | 1200;
		responsiveSizes: string;
		lazy: boolean;
		alt: string;
	}> = {}
) {
	return await render(BaseImage, {
		props: {
			mbid: overrides.mbid ?? validMbid,
			remoteUrl: overrides.remoteUrl ?? null,
			customUrl: overrides.customUrl ?? null,
			imageType: overrides.imageType ?? 'album',
			size: overrides.size ?? 'md',
			requestSize: overrides.requestSize,
			responsiveSizes: overrides.responsiveSizes,
			lazy: overrides.lazy ?? false,
			alt: overrides.alt ?? 'Test Image'
		}
	} as Parameters<typeof render<typeof BaseImage>>[1]);
}

describe('BaseImage.svelte - remoteUrl', () => {
	beforeEach(() => {
		mockDirectRemoteEnabled = true;
		warmMock.listeners.length = 0;
		warmMock.watch.mockClear();
	});

	it('renders CDN URL with referrerpolicy when remoteUrl is set', async () => {
		await renderComponent({ remoteUrl: cdnUrl });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).toHaveAttribute('referrerpolicy', 'no-referrer');
		await expect.element(img).toHaveAttribute('src', `${cdnUrl}/small`);
	});

	it('appends /medium suffix for lg size', async () => {
		await renderComponent({ remoteUrl: cdnUrl, size: 'lg' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toHaveAttribute('src', `${cdnUrl}/medium`);
	});

	it('uses original URL for full size', async () => {
		await renderComponent({ remoteUrl: cdnUrl, size: 'full' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toHaveAttribute('src', cdnUrl);
	});

	it('lets card layout request a 250px proxy and small direct image', async () => {
		await renderComponent({ remoteUrl: cdnUrl, size: 'full', requestSize: 250 });

		await expect.element(page.getByAltText('Test Image')).toHaveAttribute('src', `${cdnUrl}/small`);

		mockDirectRemoteEnabled = false;
		await renderComponent({
			imageType: 'artist',
			size: 'full',
			requestSize: 250,
			alt: 'Proxy image'
		});
		await expect
			.element(page.getByAltText('Proxy image'))
			.toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
	});

	it('offers 250px and 500px variants when the rendered size can cross the boundary', async () => {
		mockDirectRemoteEnabled = false;
		await renderComponent({
			imageType: 'artist',
			size: 'full',
			requestSize: 250,
			responsiveSizes: '(max-width: 400px) 70vw, 280px',
			alt: 'Responsive proxy'
		});

		await expect
			.element(page.getByAltText('Responsive proxy'))
			.toHaveAttribute(
				'srcset',
				`/api/v1/covers/artist/${validMbid}?size=250 250w, /api/v1/covers/artist/${validMbid}?size=500 500w`
			);
		await expect
			.element(page.getByAltText('Responsive proxy'))
			.toHaveAttribute('sizes', '(max-width: 400px) 70vw, 280px');
	});

	it('renders proxy img without referrerpolicy when remoteUrl is null', async () => {
		await renderComponent({ remoteUrl: null, imageType: 'album' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).not.toHaveAttribute('referrerpolicy');
	});

	it('renders proxy URL for artist when remoteUrl is null', async () => {
		await renderComponent({ remoteUrl: null, imageType: 'artist' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
	});

	it('renders proxy URL when remoteUrl is set but setting is disabled', async () => {
		mockDirectRemoteEnabled = false;
		await renderComponent({ remoteUrl: cdnUrl, imageType: 'artist' });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();
		await expect.element(img).toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
		await expect.element(img).not.toHaveAttribute('referrerpolicy');
	});

	it('falls back to proxy URL when remote image errors', async () => {
		await renderComponent({ remoteUrl: cdnUrl, imageType: 'artist' });

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
		warmMock.listeners.length = 0;
		warmMock.watch.mockClear();
	});

	it('shows a shimmer skeleton while the cover is loading', async () => {
		await renderComponent({ imageType: 'album' });

		await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
	});

	it('holds the skeleton on the lazy path (placeholder gif load must not hide it)', async () => {
		// The lazy <img> mounts with a 1x1 data-URI gif whose load fires immediately; it must not
		// count as the cover loading, or the skeleton would vanish on the default grid path.
		await renderComponent({ imageType: 'album', lazy: true });

		await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
	});

	it('holds the skeleton after a warming error instead of dropping to the placeholder', async () => {
		await renderComponent({ imageType: 'album', lazy: false });

		const img = page.getByAltText('Test Image');
		await expect.element(img).toBeInTheDocument();

		// A cold cover comes back as 202 (warming) -> the <img> fires error. We must keep the
		// skeleton and poll it in, not settle on the static placeholder.
		img.element().dispatchEvent(new Event('error'));

		await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
		expect(warmMock.watch).toHaveBeenCalledTimes(1);
	});

	it('settles the visible shimmer within 6.5 seconds while warming remains subscribed', async () => {
		vi.useFakeTimers();
		await renderComponent({ imageType: 'album', lazy: false });

		page.getByAltText('Test Image').element().dispatchEvent(new Event('error'));
		await vi.advanceTimersByTimeAsync(6500);

		await expect.element(page.getByTestId('cover-fallback')).toBeInTheDocument();
		await expect.element(page.getByTestId('cover-skeleton')).not.toBeInTheDocument();
		expect(warmMock.watch).toHaveBeenCalledTimes(1);
		vi.useRealTimers();
	});

	it('falls back to the covers proxy when a direct image emits neither load nor error', async () => {
		const releaseCdn = await holdCdnRequests();
		vi.useFakeTimers();
		try {
			await renderComponent({ remoteUrl: cdnUrl, imageType: 'artist', lazy: false });

			await vi.advanceTimersByTimeAsync(6500);

			// the CDN branch must be gone: shimmer is back and the img points at the covers proxy
			await expect.element(page.getByTestId('cover-fallback')).not.toBeInTheDocument();
			await expect.element(page.getByTestId('cover-skeleton')).toBeInTheDocument();
			await expect
				.element(page.getByAltText('Test Image'))
				.toHaveAttribute('src', `/api/v1/covers/artist/${validMbid}?size=250`);
		} finally {
			vi.useRealTimers();
			await releaseCdn();
		}
	});

	it('keeps the direct image when it loads before the settle deadline', async () => {
		const releaseCdn = await holdCdnRequests();
		vi.useFakeTimers();
		try {
			await renderComponent({ remoteUrl: cdnUrl, imageType: 'artist', lazy: false });

			await vi.advanceTimersByTimeAsync(4000);
			page.getByAltText('Test Image').element().dispatchEvent(new Event('load'));
			await vi.advanceTimersByTimeAsync(3000);

			await expect
				.element(page.getByAltText('Test Image'))
				.toHaveAttribute('src', `${cdnUrl}/small`);
			await expect.element(page.getByTestId('cover-fallback')).not.toBeInTheDocument();
		} finally {
			vi.useRealTimers();
			await releaseCdn();
		}
	});

	it('replaces a settled fallback when shared warming succeeds later', async () => {
		vi.useFakeTimers();
		await renderComponent({ imageType: 'album', lazy: false });

		page.getByAltText('Test Image').element().dispatchEvent(new Event('error'));
		await vi.advanceTimersByTimeAsync(6500);
		await expect.element(page.getByTestId('cover-fallback')).toBeInTheDocument();

		warmMock.listeners[0]?.({ status: 'ready', url: 'blob:late-cover' });
		await expect.element(page.getByAltText('Test Image')).toHaveAttribute('src', 'blob:late-cover');
		vi.useRealTimers();
	});
});
