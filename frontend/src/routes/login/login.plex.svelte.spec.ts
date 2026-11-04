import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { cleanup } from 'vitest-browser-svelte';
import { renderWithQuery } from '$lib/test/render';

import Login from './+page.svelte';

const providers = { local: false, plex: true, jellyfin: false, oidc: false };

describe('signing in with Plex while requests are limited', () => {
	it('rides out a 429: keeps polling through one, and says to wait when the start is limited', async () => {
		{
			// Keeps polling through a 429 on a poll.
			vi.stubGlobal('open', vi.fn());
			let polls = 0;
			fakeApi({
				'GET /api/v3/auth/providers': providers,
				'POST /api/v3/auth/plex/start': {
					pin_id: 1,
					pin_secret: 'secret',
					authorize_url: 'https://plex.test/auth'
				},
				'POST /api/v3/auth/plex/poll/login': () => {
					polls += 1;
					return polls === 1
						? apiError(429, 'RATE_LIMITED', 'Too many requests')
						: { completed: false, user: null, token: null };
				}
			});
			await renderWithQuery(Login, {});

			await page.getByRole('button', { name: /plex/i }).last().click();

			await expect.poll(() => polls, { timeout: 6000 }).toBeGreaterThanOrEqual(2);
			await expect.element(page.getByText('Plex access denied')).not.toBeInTheDocument();
			await expect.element(page.getByText('Waiting for Plex')).toBeInTheDocument();
		}

		{
			// Says to wait when the start is limited.
			cleanup();
			fakeApi({
				'GET /api/v3/auth/providers': providers,
				'POST /api/v3/auth/plex/start': apiError(429, 'RATE_LIMITED', 'Too many requests')
			});
			await renderWithQuery(Login, {});

			await page.getByRole('button', { name: /plex/i }).last().click();

			await expect
				.element(page.getByText('Too many attempts. Wait a moment and try again.'))
				.toBeInTheDocument();
		}
	});
});
