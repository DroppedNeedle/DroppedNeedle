import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import AutoDownloadTab from './AutoDownloadTab.svelte';

const ARTIST = 'a74b1b7f-71a5-4011-9441-d0b5e4122711';

beforeEach(() => {
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'Admin'
		})
	);
});

describe('requests Auto-downloads tab', () => {
	it('revokes a granted artist and a granted mix', async () => {
		const api = fakeApi({
			'GET /api/v3/requests/auto-download-approvals': {
				items: [],
				count: 0,
				granted: [
					{
						user_id: 'user-1',
						user_name: 'Ada',
						artist_mbid: ARTIST,
						artist_name: 'Radiohead',
						requested_at: 1700000000
					}
				]
			},
			'GET /api/v3/requests/auto-download-approval-batches': { batches: [], count: 0 },
			'GET /api/v3/requests/personal-mix-approvals': {
				items: [],
				count: 0,
				granted: [{ user_id: 'user-2', user_name: 'Bob', requested_at: 1700000000 }]
			},
			'GET /api/v3/requests/playlist-sync-approvals': { items: [], count: 0, granted: [] },
			'POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke': {
				success: true,
				message: 'Auto-download revoked'
			},
			'POST /api/v3/requests/personal-mix-approvals/{user_id}/revoke': {
				success: true,
				message: 'Weekly Mix auto-request revoked'
			}
		});
		await renderWithQuery(AutoDownloadTab, { active: true });

		await expect.element(page.getByText('Radiohead')).toBeInTheDocument();
		await page.getByRole('button', { name: 'Revoke' }).first().click();
		await expect
			.poll(
				() =>
					api.calls('POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke')
						.length
			)
			.toBe(1);
		const [artist] = api.calls(
			'POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke'
		);
		expect(artist?.params).toEqual({ user_id: 'user-1', artist_mbid: ARTIST });

		await page.getByRole('button', { name: 'Revoke' }).last().click();
		await expect
			.poll(() => api.calls('POST /api/v3/requests/personal-mix-approvals/{user_id}/revoke').length)
			.toBe(1);
	});

	it('lists a synced playlist asking for its requests, counts it and approves it', async () => {
		const counts: number[] = [];
		const api = fakeApi({
			'GET /api/v3/requests/auto-download-approvals': { items: [], count: 0, granted: [] },
			'GET /api/v3/requests/auto-download-approval-batches': { batches: [], count: 0 },
			'GET /api/v3/requests/personal-mix-approvals': { items: [], count: 0, granted: [] },
			'GET /api/v3/requests/playlist-sync-approvals': {
				count: 1,
				items: [
					{
						playlist_id: 'pl-road',
						playlist_name: 'Road Trip',
						user_id: 'user-3',
						user_name: 'Plain',
						requested_at: 1700000000,
						song_count: 142,
						whole_albums: true
					}
				],
				granted: []
			},
			'POST /api/v3/requests/playlist-sync-approvals/{playlist_id}/approve': {
				success: true,
				message: 'Playlist sync approved'
			}
		});
		await renderWithQuery(AutoDownloadTab, {
			active: true,
			oncount: (n: number) => counts.push(n)
		});

		await expect.element(page.getByText('Keep in sync: Road Trip')).toBeVisible();
		await expect.element(page.getByText('142 songs')).toBeVisible();
		await expect.element(page.getByText('whole albums')).toBeVisible();
		await expect.poll(() => counts.at(-1)).toBe(1);
		await page.getByRole('button', { name: 'Approve' }).click();
		await expect
			.poll(
				() =>
					api.calls('POST /api/v3/requests/playlist-sync-approvals/{playlist_id}/approve')[0]
						?.params
			)
			.toEqual({ playlist_id: 'pl-road' });
	});
});
