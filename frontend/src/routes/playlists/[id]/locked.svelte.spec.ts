import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import PlaylistPage from './+page.svelte';
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

describe('a private playlist of someone else', () => {
	it('shows the locked view with its owner and track count', async () => {
		fakeApi({
			'GET /api/v3/playlists/{playlist_id}': apiError(
				403,
				'PLAYLIST_PRIVATE',
				'This playlist is private.',
				{ id: 'p-1', is_redacted: true, can_delete: false, owner_name: 'Cara', track_count: 4 }
			)
		});
		await renderWithQuery(PlaylistPage, {
			data: { playlistId: 'p-1', user: null, primarySource: 'listenbrainz' }
		});

		await expect.element(page.getByRole('heading', { name: 'Private playlist' })).toBeVisible();
		await expect.element(page.getByText('4 tracks · owned by Cara')).toBeVisible();
		await expect.element(page.getByText("Couldn't load this playlist")).not.toBeInTheDocument();
	});
});
