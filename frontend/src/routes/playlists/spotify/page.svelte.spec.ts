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

	it('lists Liked Songs first and shows a synced playlist as Synced, not Re-import', async () => {
		const api = fakeApi({
			'GET /api/v3/acquire/spotify/playlists': {
				liked_songs: {
					access: 'ready',
					track_count: 1284,
					imported_playlist_id: null,
					synced: false
				},
				playlists: [
					{
						id: 'sp-road',
						name: 'Road Trip',
						description: '',
						track_count: 142,
						cover_url: null,
						owner: 'Someone',
						imported_playlist_id: 'pl-road',
						synced: true
					}
				]
			},
			'POST /api/v3/acquire/spotify/liked-songs/import': { playlist_id: 'pl-liked' }
		});
		await renderWithQuery(SpotifyImportPage, {});

		await expect.element(page.getByText('1284 songs')).toBeVisible();
		await expect.element(page.getByText('Synced')).toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Re-import' })).not.toBeInTheDocument();
		(
			page.getByRole('button', { name: 'Import Liked Songs' }).element() as HTMLButtonElement
		).click();
		await expect
			.poll(() => api.calls('POST /api/v3/acquire/spotify/liked-songs/import').length)
			.toBe(1);
	});

	it('asks a person linked before Liked Songs to reconnect', async () => {
		fakeApi({
			'GET /api/v3/acquire/spotify/playlists': {
				liked_songs: {
					access: 'needs_reconnect',
					track_count: null,
					imported_playlist_id: null,
					synced: false
				},
				playlists: []
			}
		});
		await renderWithQuery(SpotifyImportPage, {});

		await expect.element(page.getByText('Reconnect Spotify to import Liked Songs.')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Reconnect' })).toBeInTheDocument();
	});
});
