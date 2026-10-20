import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

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
				return { setup_required: true };
			},
			'GET /api/v3/me': apiError(401, 'UNAUTHORIZED', 'Sign in first')
		});

		const url = new URL('/setup', window.location.origin);
		await expect(load({ url } as Parameters<typeof load>[0])).resolves.toMatchObject({
			user: null
		});
		await renderWithQuery(SetupPage, {});

		await expect.element(page.getByLabelText('Display Name')).toBeVisible();
		expect(attempts).toBe(2);
	});
});
