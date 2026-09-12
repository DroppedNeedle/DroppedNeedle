import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const fixtures = vi.hoisted(() => ({
	albumItems: Array.from({ length: 9 }, (_, index) => ({
		id: `album-${index}`,
		cover_available: true
	})),
	artistItems: Array.from({ length: 9 }, (_, index) => ({
		id: `artist-${index}`,
		musicbrainz_artist_id: `mbid-${index}`
	}))
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryRecentlyAddedQuery: () => ({ data: { items: fixtures.albumItems } }),
	getLibraryArtistThumbsQuery: () => ({ data: { items: fixtures.artistItems } })
}));

import LibraryHubTiles from './LibraryHubTiles.svelte';

const stats = {
	total_albums: 9,
	total_artists: 9,
	total_tracks: 90,
	total_size_bytes: 0,
	format_breakdown: {},
	review_count: 0,
	local_only_count: 0,
	last_scan_at: null
};

async function renderComponent() {
	return await render(LibraryHubTiles, {
		props: { stats }
	} as Parameters<typeof render<typeof LibraryHubTiles>>[1]);
}

describe('LibraryHubTiles artwork window', () => {
	beforeEach(async () => {
		await page.viewport(1280, 800);
	});

	afterEach(async () => {
		await page.viewport(1280, 800);
	});

	it('mounts only the five visible cards per desktop fan at 250px', async () => {
		await renderComponent();

		const cards = page.getByTestId('library-fan-card').all();
		expect(cards).toHaveLength(15);
		expect(page.getByTestId('library-fan-image').all()).toHaveLength(15);
		await expect.element(cards[0]!).toHaveAttribute('data-request-size', '250');
		const responsiveImages = page
			.getByTestId('library-fan-image')
			.all()
			.filter((image) => image.element().hasAttribute('data-srcset'));
		expect(responsiveImages).toHaveLength(10);
	});

	it('does not mount the CSS-hidden track fan on mobile', async () => {
		await page.viewport(390, 760);
		await renderComponent();

		expect(page.getByTestId('library-fan-card').all()).toHaveLength(10);
		expect(page.getByTestId('library-fan-image').all()).toHaveLength(10);
	});

	it('keeps the fan still when reduced motion is requested', async () => {
		vi.useFakeTimers();
		const matchMedia = vi.spyOn(window, 'matchMedia').mockImplementation(
			(query) =>
				({
					matches: query.includes('prefers-reduced-motion') || query.includes('min-width'),
					media: query,
					onchange: null,
					addEventListener: vi.fn(),
					removeEventListener: vi.fn(),
					addListener: vi.fn(),
					removeListener: vi.fn(),
					dispatchEvent: vi.fn()
				}) as MediaQueryList
		);
		await renderComponent();
		const cards = page.getByTestId('library-fan-card').all();
		const firstKey = (cards[0]!.element() as HTMLElement).dataset.fanKey;

		await vi.advanceTimersByTimeAsync(8400);

		expect((cards[0]!.element() as HTMLElement).dataset.fanKey).toBe(firstKey);
		matchMedia.mockRestore();
		vi.useRealTimers();
	});
});
