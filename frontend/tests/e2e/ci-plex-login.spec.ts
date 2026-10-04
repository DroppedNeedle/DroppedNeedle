import { expect, test, type Route } from '@playwright/test';

import { adminUser, installApiMocks, json, type MockTable } from './mockApi';

/**
 * Plex unified login: with Plex sign-in enabled, the login page offers a
 * Plex tab; authorizing there signs the user in without a local password.
 * The Plex authorize page itself is stubbed out (popup captured, never
 * loaded). All API answers are stubbed; no live network.
 */

test.describe('plex unified login', () => {
	test('authorizing via Plex signs the user in', async ({ page }) => {
		let session: typeof adminUser | null = null;
		let polls = 0;

		await page.addInitScript(() => {
			(window as unknown as { __ciOpened: string[] }).__ciOpened = [];
			window.open = ((url?: string | URL | null) => {
				(window as unknown as { __ciOpened: string[] }).__ciOpened.push(String(url));
				return null;
			}) as typeof window.open;
		});

		const extra: MockTable = {
			'GET /api/v3/auth/providers': { local: true, plex: true, jellyfin: false, oidc: false },
			'POST /api/v3/auth/plex/start': {
				pin_id: 42,
				authorize_url: 'https://app.plex.tv/auth#code=ci'
			},
			'POST /api/v3/auth/plex/poll/login': async (route: Route) => {
				polls += 1;
				if (polls < 2) {
					await json(route, { completed: false });
					return;
				}
				session = adminUser;
				await json(route, { completed: true, token: 'ci-plex-token', user: adminUser });
			}
		};
		await installApiMocks(page, { user: () => session, extra });

		await page.goto('/login');
		await page.getByRole('button', { name: 'Plex' }).click();
		await page.getByRole('button', { name: 'Continue with Plex' }).click();

		// The Plex authorize page opens; polling completes the sign-in.
		await page.waitForURL((url) => !url.pathname.includes('/login'), { timeout: 15_000 });
		await expect(page.getByTestId('app-shell')).toBeVisible();

		const captured = await page.evaluate(
			() => (window as unknown as { __ciOpened: string[] }).__ciOpened
		);
		expect(captured).toEqual(['https://app.plex.tv/auth#code=ci']);
		expect(polls).toBeGreaterThanOrEqual(2);
	});
});
