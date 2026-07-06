import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import WantedRetryingCard from './WantedRetryingCard.svelte';
import type { WantedRetryingItem } from '$lib/queries/wanted/types';

function makeItem(overrides: Partial<WantedRetryingItem> = {}): WantedRetryingItem {
	return {
		release_group_mbid: '22222222-2222-2222-2222-222222222222',
		artist_name: 'Yan Qing',
		album_title: 'the arrival',
		retry_count: 1,
		max_attempts: 6,
		next_retry_at: Date.now() / 1000 + 25 * 60,
		artist_mbid: null,
		year: 2026,
		cover_url: null,
		user_id: 'user-a',
		user_name: null,
		...overrides
	};
}

function renderCard(
	overrides: Partial<WantedRetryingItem> = {},
	props: Record<string, unknown> = {}
) {
	return render(WantedRetryingCard, {
		props: { item: makeItem(overrides), ...props }
	} as unknown as Parameters<typeof render<typeof WantedRetryingCard>>[1]);
}

describe('WantedRetryingCard.svelte', () => {
	it('shows the album with a still-hunting retry line', async () => {
		renderCard();
		await expect.element(page.getByText('the arrival')).toBeVisible();
		await expect.element(page.getByText('Still hunting')).toBeVisible();
		await expect.element(page.getByText('retry 2 of 6')).toBeVisible();
		await expect.element(page.getByText('next try in 25 min')).toBeVisible();
	});

	it('links to the downloads queue instead of offering watch actions', async () => {
		renderCard();
		await expect.element(page.getByText('Manage in Downloads')).toBeVisible();
		expect(page.getByText('Stop').elements()).toHaveLength(0);
		expect(page.getByText('Check now').elements()).toHaveLength(0);
	});

	it('shows the requester chip when an owner name is given (admin view)', async () => {
		renderCard({}, { ownerName: 'Someone Else' });
		await expect.element(page.getByText('requested by Someone Else')).toBeVisible();
	});

	it('never claims an attempt beyond the ladder', async () => {
		renderCard({ retry_count: 6 });
		await expect.element(page.getByText('retry 6 of 6')).toBeVisible();
	});
});
