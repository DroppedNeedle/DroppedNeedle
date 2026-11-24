import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type FakeRequest, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

const toast = vi.hoisted(() => vi.fn());
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: toast } }));

import PlaylistSyncBand from './PlaylistSyncBand.svelte';
import { SYNC_FAILURES } from './syncStates';

type Sync = RouteData<'GET /api/v3/playlists/{playlist_id}/sync'>;

const OFF: Sync = {
	syncable: true,
	enabled: false,
	whole_albums: false,
	grant: 'waiting',
	progress: 'off',
	songs_left: 0,
	hold: null,
	failure: null,
	spotify_off: false,
	checked_at: null
};

const ON: Sync = { ...OFF, enabled: true, progress: 'starting' };

beforeEach(() => {
	localStorage.clear();
	toast.mockClear();
	authStore.setUser(
		toAuthUser({
			id: 'owner-1',
			display_name: 'Owner',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'owner',
			username_display: 'Owner'
		})
	);
});

describe('the sync band', () => {
	it('turns sync on, then asks for whole albums, and says a plain user waits for an admin', async () => {
		let current = OFF;
		const api = fakeApi({
			'GET /api/v3/playlists/{playlist_id}/sync': () => current,
			'PUT /api/v3/playlists/{playlist_id}/sync': (request: FakeRequest) => {
				const body = request.body as { enabled: boolean; whole_albums: boolean };
				current = { ...ON, progress: 'catching_up', whole_albums: body.whole_albums };
				return current;
			}
		});
		await renderWithQuery(PlaylistSyncBand, { playlistId: 'pl-1', isOwner: true });

		await expect
			.element(page.getByText('Add new songs from Spotify here and request the missing ones.'))
			.toBeVisible();
		await expect.element(page.getByRole('radiogroup', { name: 'Request' })).not.toBeInTheDocument();
		await page.getByRole('checkbox', { name: 'Keep in sync' }).click();

		await expect
			.poll(() => api.calls('PUT /api/v3/playlists/{playlist_id}/sync')[0]?.body)
			.toEqual({
				enabled: true,
				whole_albums: false
			});
		await expect
			.element(
				page.getByText('Waiting for an admin to approve requests. New songs are still added.')
			)
			.toBeVisible();
		await expect.element(page.getByRole('radio', { name: 'Songs' })).toBeChecked();
		await page.getByRole('radio', { name: 'Whole albums' }).click();
		await expect
			.poll(() => api.calls('PUT /api/v3/playlists/{playlist_id}/sync')[1]?.body)
			.toEqual({
				enabled: true,
				whole_albums: true
			});
		await expect.element(page.getByRole('radio', { name: 'Whole albums' })).toBeChecked();
	});

	it.each(Object.entries(SYNC_FAILURES))('says %s in its own words', async (failure, text) => {
		fakeApi({
			'GET /api/v3/playlists/{playlist_id}/sync': {
				...ON,
				failure: failure as Sync['failure']
			}
		});
		await renderWithQuery(PlaylistSyncBand, { playlistId: 'pl-1', isOwner: true });

		await expect.element(page.getByText(text)).toBeVisible();
	});

	it('shows an editor the line and no switches, and nothing while sync is off', async () => {
		fakeApi({
			'GET /api/v3/playlists/{playlist_id}/sync': {
				...ON,
				grant: 'not_needed',
				progress: 'catching_up',
				songs_left: 120
			}
		});
		await renderWithQuery(PlaylistSyncBand, { playlistId: 'pl-1', isOwner: false });

		await expect
			.element(page.getByText('Synced with Spotify. Requesting missing songs: 120 left.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('checkbox', { name: 'Keep in sync' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('radiogroup', { name: 'Request' })).not.toBeInTheDocument();
	});

	it("points an editor at the owner's link, not their own Profile", async () => {
		fakeApi({
			'GET /api/v3/playlists/{playlist_id}/sync': { ...ON, failure: 'not_linked' }
		});
		await renderWithQuery(PlaylistSyncBand, { playlistId: 'pl-1', isOwner: false });

		await expect
			.element(page.getByText("Synced with Spotify. The owner's Spotify isn't connected."))
			.toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Open Profile' })).not.toBeInTheDocument();
	});

	it('tells the owner why the switch was refused, in its own words', async () => {
		fakeApi({
			'GET /api/v3/playlists/{playlist_id}/sync': OFF,
			'PUT /api/v3/playlists/{playlist_id}/sync': apiError(
				409,
				'SPOTIFY_NOT_LINKED',
				'The owner has no Spotify link stored.'
			)
		});
		await renderWithQuery(PlaylistSyncBand, { playlistId: 'pl-1', isOwner: true });

		await page.getByRole('checkbox', { name: 'Keep in sync' }).click();

		await expect
			.poll(() => toast.mock.calls.at(-1)?.[0])
			.toEqual({ message: "Spotify isn't connected. Connect it on your Profile.", type: 'error' });
	});
});
