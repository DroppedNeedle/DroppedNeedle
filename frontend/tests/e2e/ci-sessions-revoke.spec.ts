import { expect, test, type Route } from '@playwright/test';

import { installApiMocks, json, standardUser, type MockTable } from './mockApi';

/**
 * Session hygiene: a signed-in user opens their profile, revokes a stale
 * companion session after the inline confirm, and the list updates. All API
 * answers are stubbed; no live network.
 */

const liveSessions = [
	{
		id: 'sess-current',
		kind: 'standard',
		label: 'Firefox on Linux',
		created_at: 1759400000,
		last_seen_at: 1759403600,
		expires_at: 1762000000,
		current: true
	},
	{
		id: 'sess-old',
		kind: 'companion',
		label: 'Bedroom speaker',
		created_at: 1759300000,
		last_seen_at: 1759303600,
		expires_at: 1762000000,
		current: false
	}
];

test.describe('sessions revoke', () => {
	test('revoking a companion session removes it from the profile list', async ({ page }) => {
		let revoked: string | null = null;

		const extra: MockTable = {
			'GET /api/v3/auth/sessions': async (route: Route) => {
				await json(route, {
					sessions: liveSessions.filter((s) => s.id !== revoked)
				});
			},
			'DELETE /api/v3/auth/sessions/': async (route: Route) => {
				revoked = new URL(route.request().url()).pathname.split('/').pop() ?? null;
				await json(route, { success: true });
			}
		};
		await installApiMocks(page, { user: standardUser, extra });

		await page.goto('/profile');

		// Both sessions list; the current one carries no revoke control.
		await expect(page.getByText('Bedroom speaker')).toBeVisible();
		await expect(page.getByRole('button', { name: /revoke bedroom speaker/i })).toBeVisible();
		await expect(page.getByRole('button', { name: /revoke firefox on linux/i })).toHaveCount(0);

		// Revoke runs only after the inline confirm.
		await page.getByRole('button', { name: /revoke bedroom speaker/i }).click();
		await expect(page.getByRole('button', { name: 'Confirm revoke' })).toBeVisible();
		await page.getByRole('button', { name: 'Confirm revoke' }).click();

		expect(revoked).toBe('sess-old');
		await expect(page.getByText('Bedroom speaker')).toHaveCount(0);
		await expect(page.getByText('Firefox on Linux')).toBeVisible();
	});
});
