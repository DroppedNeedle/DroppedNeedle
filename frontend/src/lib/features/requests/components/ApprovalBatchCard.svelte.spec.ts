import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import ApprovalBatchCard from './ApprovalBatchCard.svelte';

type Card = RouteData<'GET /api/v3/requests/approvals/batches'>['cards'][number];

const album = (n: number, title: string): Card['items'][number] => ({
	musicbrainz_id: `a0000000-0000-4000-8000-00000000000${n}`,
	title,
	artist_name: 'Willow Fen',
	cover_url: `/api/v3/covers/release-group/a0000000-0000-4000-8000-00000000000${n}?size=250`,
	song_count: n
});

const CARD: Card = {
	id: '5c2a1b7e-0000-4000-8000-000000000001',
	source: 'playlist',
	name: 'Road Trip',
	requester_name: 'Sam',
	created_at: '2026-11-23T10:00:00Z',
	waiting: 3,
	items: [album(1, 'Harbour Lights'), album(2, 'Copper Sky'), album(3, 'Deep Water')]
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'Admin'
		})
	);
});

describe('an approval card', () => {
	it('approves only the ticked albums, saying the rest are rejected', async () => {
		const api = fakeApi({
			'POST /api/v3/requests/approvals/batches/{batch_id}/decide': {
				approved: 2,
				rejected: 1,
				skipped: 0
			}
		});
		await renderWithQuery(ApprovalBatchCard, { card: CARD });

		await expect.element(page.getByRole('button', { name: 'Approve all 3' })).toBeVisible();
		await page.getByRole('button', { name: 'Show the 3 albums' }).click();
		await expect.element(page.getByText('3 of 3 ticked')).toBeVisible();

		await page.getByRole('checkbox', { name: 'Approve Copper Sky' }).click();
		await expect.element(page.getByText('2 of 3 ticked')).toBeVisible();
		await expect.element(page.getByText('The 1 unticked is rejected.')).toBeVisible();
		await page.getByRole('button', { name: 'Approve 2' }).click();

		await expect
			.poll(() => api.calls('POST /api/v3/requests/approvals/batches/{batch_id}/decide').length)
			.toBe(1);
		const [call] = api.calls('POST /api/v3/requests/approvals/batches/{batch_id}/decide');
		expect(call?.params).toEqual({ batch_id: CARD.id });
		expect(call?.body).toEqual({
			approve: [CARD.items[0].musicbrainz_id, CARD.items[2].musicbrainz_id]
		});
	});

	it('rejects every album with Reject all', async () => {
		const api = fakeApi({
			'POST /api/v3/requests/approvals/batches/{batch_id}/decide': {
				approved: 0,
				rejected: 3,
				skipped: 0
			}
		});
		await renderWithQuery(ApprovalBatchCard, { card: CARD });

		await page.getByRole('button', { name: 'Reject all' }).click();

		await expect
			.poll(() => api.calls('POST /api/v3/requests/approvals/batches/{batch_id}/decide').length)
			.toBe(1);
		const [call] = api.calls('POST /api/v3/requests/approvals/batches/{batch_id}/decide');
		expect(call?.body).toEqual({ approve: [] });
	});
});
