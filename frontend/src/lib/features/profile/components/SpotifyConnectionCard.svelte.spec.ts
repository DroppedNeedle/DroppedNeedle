import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: { user: { id: 'u1', providers: [] }, isAuthenticated: true, isAdmin: false }
}));

import SpotifyConnectionCard from './SpotifyConnectionCard.svelte';

const linked = { connections: [{ service: 'spotify', enabled: true, username: 'harvey' }] };

describe('the Spotify card on the profile', () => {
	it('asks a link made before Liked Songs to sign in again', async () => {
		fakeApi({
			'GET /api/v3/me/connections': linked,
			'GET /api/v3/acquire/spotify/link': {
				linked: true,
				username: 'harvey',
				liked_songs: 'needs_reconnect'
			}
		});
		await renderWithQuery(SpotifyConnectionCard, {});

		await expect.element(page.getByText('Reconnect to sync Liked Songs.')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Reconnect' })).toBeVisible();
	});

	it('asks nothing of a link that can read Liked Songs', async () => {
		fakeApi({
			'GET /api/v3/me/connections': linked,
			'GET /api/v3/acquire/spotify/link': { linked: true, username: 'harvey', liked_songs: 'ready' }
		});
		await renderWithQuery(SpotifyConnectionCard, {});

		await expect.element(page.getByRole('button', { name: 'Disconnect' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Reconnect' })).not.toBeInTheDocument();
	});
});
