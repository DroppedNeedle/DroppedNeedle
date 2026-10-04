import { expect, test, type Route } from '@playwright/test';

import { adminUser, installApiMocks, json, type MockTable } from './mockApi';

/**
 * ListenBrainz setup: an admin opens Settings, fills the ListenBrainz
 * connection, saves, and sees the saved confirmation. All API answers are
 * stubbed; no live network.
 */

test.describe('listenbrainz save', () => {
	test('saving the connection persists credentials and confirms', async ({ page }) => {
		let savedBody: Record<string, unknown> | null = null;
		const stored = { enabled: false, username: '', user_token: '' };

		const extra: MockTable = {
			'GET /api/v3/settings/listenbrainz': async (route: Route) => {
				const pathname = new URL(route.request().url()).pathname;
				if (pathname.endsWith('/verify')) {
					await json(route, { valid: true, message: 'Token works.' });
					return;
				}
				await json(route, stored);
			},
			'PUT /api/v3/settings/listenbrainz': async (route: Route) => {
				savedBody = (route.request().postDataJSON() ?? {}) as Record<string, unknown>;
				Object.assign(stored, savedBody);
				await json(route, stored);
			},
			'GET /api/v3/settings/scrobble': { listenbrainz: true, lastfm: false },
			'PUT /api/v3/settings/scrobble': async (route: Route) => {
				await json(route, route.request().postDataJSON() ?? {});
			}
		};
		await installApiMocks(page, { user: adminUser, extra });

		await page.goto('/settings?tab=listenbrainz');

		await page.getByLabel('Enable ListenBrainz').check();
		await page.getByPlaceholder('Your ListenBrainz username').fill('ci-listener');
		await page.getByPlaceholder('User token').fill('ci-token-123');
		await page.getByRole('button', { name: 'Save ListenBrainz settings' }).click();

		await expect(page.getByText('ListenBrainz settings saved.')).toBeVisible();
		expect(savedBody).toMatchObject({
			enabled: true,
			username: 'ci-listener',
			user_token: 'ci-token-123'
		});
	});
});
