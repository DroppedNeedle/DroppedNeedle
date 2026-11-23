import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

const toast = vi.hoisted(() => vi.fn());
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: toast } }));

import PlaylistRequestBanner from './PlaylistRequestBanner.svelte';

type State = RouteData<'GET /api/v3/playlists/{playlist_id}/request-state'>;

const STATE: State = {
	import: { state: 'failed', reason: 'not_linked' },
	finding_albums: false,
	requestable_albums: 3,
	no_album_songs: 2,
	entries: []
};

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

describe('the request banner', () => {
	it('counts what is missing and says what a press filed and left out', async () => {
		const api = fakeApi({
			'GET /api/v3/playlists/{playlist_id}/request-state': STATE,
			'POST /api/v3/playlists/{playlist_id}/request-missing': {
				batch_id: '5c2a1b7e-0000-4000-8000-000000000001',
				requested: 1,
				in_library: 1,
				already_requested: 0,
				rejected_before: 0,
				over_cap: 0,
				over_quota: 2,
				needs_approval: true
			}
		});
		await renderWithQuery(PlaylistRequestBanner, { playlistId: 'pl-1' });

		await expect.element(page.getByText('3 albums to request')).toBeVisible();
		await expect.element(page.getByText('2 songs have no album found yet')).toBeVisible();
		await expect
			.element(page.getByText('The Spotify import failed: link Spotify again in your profile.'))
			.toBeVisible();

		await page.getByRole('button', { name: 'Request 3 albums' }).click();

		await expect
			.poll(() => toast.mock.calls.at(-1)?.[0])
			.toEqual({
				message:
					'Asked for 1 album, waiting for approval. Left out: 1 in library, 2 over your quota.',
				type: 'success'
			});
		const [call] = api.calls('POST /api/v3/playlists/{playlist_id}/request-missing');
		expect((call?.body as { idempotency_key: string }).idempotency_key).toMatch(/^[0-9a-f-]{36}$/);
	});

	it('says albums are being found while the job runs', async () => {
		fakeApi({
			'GET /api/v3/playlists/{playlist_id}/request-state': {
				...STATE,
				import: null,
				finding_albums: true,
				requestable_albums: 0,
				no_album_songs: 0
			}
		});
		await renderWithQuery(PlaylistRequestBanner, { playlistId: 'pl-1' });

		await expect.element(page.getByText('Finding albums...')).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Request/ })).not.toBeInTheDocument();
	});
});
