import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { PurchaseOptionsResponse } from '$lib/types';

const h = vi.hoisted(() => ({
	data: undefined as PurchaseOptionsResponse | undefined,
	isLoading: false
}));

vi.mock('$lib/queries/albums/GetItQueries.svelte', () => ({
	getPurchaseOptionsQuery: () => ({
		get data() {
			return h.data;
		},
		get isLoading() {
			return h.isLoading;
		}
	})
}));

import WhereToBuy from './WhereToBuy.svelte';

function options(overrides: Partial<PurchaseOptionsResponse> = {}): PurchaseOptionsResponse {
	return {
		digital: [
			{
				store: 'bandcamp',
				label: 'Bandcamp',
				url: 'https://x.bandcamp.com/album/y',
				kind: 'digital'
			},
			{ store: 'qobuz', label: 'Qobuz', url: 'https://www.qobuz.com/album/y', kind: 'digital' },
			{
				store: 'itunes',
				label: 'iTunes / Apple Music',
				url: 'https://music.apple.com/gb/album/y',
				kind: 'digital'
			}
		],
		physical: [
			{ store: 'amazon', label: 'Amazon', url: 'https://www.amazon.co.uk/dp/X', kind: 'physical' }
		],
		free: [],
		bandcamp_search_url: 'https://bandcamp.com/search?q=x&item_type=a',
		disclosure: false,
		...overrides
	};
}

describe('WhereToBuy', () => {
	beforeEach(() => {
		h.data = undefined;
		h.isLoading = false;
	});

	it('shows a slim skeleton while loading', async () => {
		h.isLoading = true;
		const { container } = render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		await expect.element(page.getByText('Where to buy')).toBeVisible();
		expect(container.querySelector('.skeleton')).not.toBeNull();
	});

	it('collapsed row shows stores in fairness order, across every kind', async () => {
		h.data = options();
		render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		// Bandcamp first (D19); physical links are eligible for the row too
		await expect.element(page.getByRole('link', { name: 'Bandcamp', exact: true })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Qobuz', exact: true })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Amazon', exact: true })).toBeVisible();
	});

	it('shows physical links in the row when an album has no digital ones at all', async () => {
		// the vinyl-only reissue case: the row used to render empty next to "N more"
		h.data = options({
			digital: [],
			physical: [
				{ store: 'amazon', label: 'Amazon', url: 'https://amazon.co.uk/dp/A', kind: 'physical' },
				{
					store: 'other',
					label: 'cstrecords.com',
					url: 'https://cstrecords.com/p',
					kind: 'physical'
				}
			]
		});
		render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		await expect.element(page.getByRole('link', { name: 'Amazon', exact: false })).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'cstrecords.com', exact: false }))
			.toBeVisible();
	});

	it('shows one pill per store and hides the duplicates behind the counter', async () => {
		h.data = options({
			digital: [
				{ store: 'bandcamp', label: 'Bandcamp', url: 'https://a.bandcamp.com/1', kind: 'digital' }
			],
			physical: [
				{ store: 'amazon', label: 'Amazon', url: 'https://amazon.co.uk/dp/A', kind: 'physical' },
				{ store: 'amazon', label: 'Amazon', url: 'https://amazon.co.uk/dp/B', kind: 'physical' },
				{ store: 'amazon', label: 'Amazon', url: 'https://amazon.co.uk/dp/C', kind: 'physical' }
			]
		});
		render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		await expect.element(page.getByRole('button', { name: '2 more' })).toBeVisible();
	});

	it('expanding reveals grouped stores and the ownership line', async () => {
		h.data = options();
		render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		await page.getByRole('button', { name: 'Details' }).click();
		await expect.element(page.getByText('Vinyl & CD')).toBeVisible();
		await expect
			.element(page.getByText('Buy it once, own it forever', { exact: false }))
			.toBeVisible();
	});

	it('offers only the Bandcamp search when nothing direct exists', async () => {
		h.data = options({ digital: [], physical: [] });
		render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		await expect.element(page.getByRole('link', { name: 'Search Bandcamp' })).toBeVisible();
	});

	it('shows the commission disclosure only when the backend says so', async () => {
		h.data = options({ disclosure: true });
		render(WhereToBuy, { releaseGroupMbid: 'rg-1' });
		await page.getByRole('button', { name: 'Details' }).click();
		await expect
			.element(page.getByText('Some links earn DroppedNeedle a commission at no cost to you.'))
			.toBeVisible();
	});
});
