import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { ArtistPurchaseOptionsResponse } from '$lib/types';

const h = vi.hoisted(() => ({
	data: undefined as ArtistPurchaseOptionsResponse | undefined,
	isLoading: false
}));

vi.mock('$lib/queries/albums/GetItQueries.svelte', () => ({
	getArtistPurchaseOptionsQuery: () => ({
		get data() {
			return h.data;
		},
		get isLoading() {
			return h.isLoading;
		}
	})
}));

import ArtistWhereToBuy from './ArtistWhereToBuy.svelte';

const props = { artistMbid: 'artist-1', artistName: 'Test Artist' };

describe('ArtistWhereToBuy', () => {
	beforeEach(() => {
		h.data = undefined;
		h.isLoading = false;
	});

	it('renders the artist storefronts it is given', async () => {
		h.data = {
			links: [
				{
					store: 'bandcamp',
					label: 'Bandcamp',
					url: 'https://a.bandcamp.com',
					kind: 'digital'
				}
			],
			bandcamp_search_url: 'https://bandcamp.com/search?q=Test+Artist&item_type=b',
			disclosure: false
		};
		render(ArtistWhereToBuy, props);
		await expect.element(page.getByText('Support the artist')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Bandcamp', exact: true })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'More on Bandcamp' })).toBeVisible();
	});

	it('falls back to a Bandcamp artist search when MusicBrainz knows no store', async () => {
		h.data = {
			links: [],
			bandcamp_search_url: 'https://bandcamp.com/search?q=Test+Artist&item_type=b',
			disclosure: false
		};
		render(ArtistWhereToBuy, props);
		await expect.element(page.getByRole('link', { name: 'Search Bandcamp' })).toBeVisible();
	});

	it('shows a skeleton while loading', async () => {
		h.isLoading = true;
		const { container } = render(ArtistWhereToBuy, props);
		expect(container.querySelector('.skeleton')).not.toBeNull();
	});
});
