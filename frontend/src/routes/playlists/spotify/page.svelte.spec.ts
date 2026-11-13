import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import SpotifyImportPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
});

describe('Import from Spotify', () => {
	it('sends a person with no Spotify link to their profile, not to a retry', async () => {
		fakeApi({
			'GET /api/v3/acquire/spotify/playlists': apiError(
				400,
				'IMPORT_NOT_CONFIGURED',
				'Spotify account not linked'
			)
		});
		await renderWithQuery(SpotifyImportPage, {});

		await expect
			.element(page.getByText('Spotify is not connected. Go to your profile to link your account.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Go to Profile' }))
			.toHaveAttribute('href', '/profile');
		await expect.element(page.getByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
	});
});
