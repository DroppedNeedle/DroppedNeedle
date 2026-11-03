import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { load } from '../+layout';
import SetupPage from './+page.svelte';

describe('setup on a fresh server', () => {
	it('shows the form when the first status check never reached the server', async () => {
		let attempts = 0;
		fakeApi({
			'GET /api/v3/auth/setup/status': () => {
				attempts += 1;
				if (attempts === 1) throw new TypeError('Failed to fetch');
				return { setup_required: true, code_required: false };
			},
			'GET /api/v3/me': apiError(401, 'UNAUTHORIZED', 'Sign in first')
		});

		const url = new URL('/setup', window.location.origin);
		await expect(load({ url } as Parameters<typeof load>[0])).resolves.toMatchObject({
			user: null
		});
		await renderWithQuery(SetupPage, {});

		await expect.element(page.getByLabelText('Display Name')).toBeVisible();
		await expect.element(page.getByLabelText('Code from the server log')).not.toBeInTheDocument();
		expect(attempts).toBe(2);
	});

	it('asks for the code from the server log after a fresh start from v2', async () => {
		authStore.setSetupRequired(true, true);
		const api = fakeApi({
			'POST /api/v3/auth/setup': apiError(401, 'UNAUTHORIZED', 'That code did not work')
		});
		await renderWithQuery(SetupPage, {});

		await expect.element(page.getByText('docker compose logs droppedneedle')).toBeVisible();
		await page.getByLabelText('Code from the server log').fill('ABCD-EFGH');
		await page.getByLabelText('Display Name').fill('Owner');
		await page.getByLabelText('Username').fill('owner');
		await page.getByLabelText('Password', { exact: true }).fill('a long fresh password');
		await page.getByLabelText('Confirm Password').fill('a long fresh password');
		await page.getByRole('button', { name: 'Create Admin Account' }).click();

		await expect.element(page.getByText('That code did not work')).toBeVisible();
		expect(api.calls('POST /api/v3/auth/setup')[0]?.body).toMatchObject({
			code: 'ABCD-EFGH',
			username: 'owner'
		});
	});
});
