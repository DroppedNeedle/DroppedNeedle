import { expect, test, type Route } from '@playwright/test';

import { adminUser, installApiMocks, json, type MockTable } from './mockApi';

/**
 * Batch cancel: an admin selects two active requests on the requests page
 * and cancels both with one action. All API answers are stubbed; no live
 * network.
 */

const MBID_A = '11111111-2222-3333-4444-555555555555';
const MBID_B = '66666666-7777-8888-9999-000000000000';

function activeItem(mbid: string, title: string) {
	return {
		album_title: title,
		artist_mbid: null,
		artist_name: 'The Fixtures',
		completed_at: null,
		duration_seconds: null,
		musicbrainz_id: mbid,
		request_kind: 'album',
		requested_at: 1759400000,
		requested_by_name: 'Ada Admin',
		requester_count: 0,
		reviewed_by_name: null,
		status: 'downloading',
		task_id: `task-${mbid.slice(0, 4)}`,
		track_title: null,
		user_id: 'admin-1'
	};
}

test.describe('batch cancel', () => {
	test('selecting two requests cancels both at once', async ({ page }) => {
		let cancelled: string[] = [];
		let cancelBody: unknown = null;

		const extra: MockTable = {
			'GET /api/v3/requests/active': async (route: Route) => {
				const pathname = new URL(route.request().url()).pathname;
				if (pathname.endsWith('/count')) {
					await json(route, { count: 2 - cancelled.length });
					return;
				}
				const items = [activeItem(MBID_A, 'Copper Static'), activeItem(MBID_B, 'Tin Echo')].filter(
					(i) => !cancelled.includes(i.musicbrainz_id)
				);
				await json(route, { count: items.length, items });
			},
			'POST /api/v3/requests/batches/cancel': async (route: Route) => {
				cancelBody = route.request().postDataJSON();
				const mbids = (cancelBody as { musicbrainz_ids?: string[] })?.musicbrainz_ids ?? [];
				cancelled = [...cancelled, ...mbids];
				await json(route, { success: true, cancelled: mbids.length });
			}
		};
		await installApiMocks(page, { user: adminUser, extra });

		await page.goto('/requests');

		await expect(page.getByText('Copper Static').first()).toBeVisible();
		await expect(page.getByText('Tin Echo').first()).toBeVisible();

		await page.getByRole('checkbox', { name: 'Select Copper Static' }).check();
		await page.getByRole('checkbox', { name: 'Select Tin Echo' }).check();
		await expect(page.getByText('2 selected')).toBeVisible();

		await page.getByRole('button', { name: 'Cancel selected' }).click();

		await expect.poll(() => cancelled.length, { timeout: 10_000 }).toBe(2);
		expect(cancelBody).toMatchObject({ musicbrainz_ids: [MBID_A, MBID_B], kind: 'album' });
		await expect(page.getByText('Copper Static')).toHaveCount(0);
		await expect(page.getByText('Tin Echo')).toHaveCount(0);
	});
});
