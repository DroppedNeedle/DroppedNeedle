import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

import RecycleBin from './RecycleBin.svelte';

type Removal = RouteData<'GET /api/v3/library/recycle-bin'>['items'][number];

const AS_OF = Date.UTC(2026, 9, 21, 12);

const removed: Removal = {
	id: 'removal-1',
	kind: 'album',
	album: {
		id: 'album-1',
		title: 'Low',
		artist: 'David Bowie',
		has_page: true,
		release_group_mbid: null,
		cover_available: false
	},
	removed_at: Date.UTC(2026, 9, 3),
	expires_at: null,
	files: 2,
	bytes: 2 * 1024 * 1024,
	not_found: 0,
	unchecked: 0,
	held: 0,
	can_put_back: true
};

beforeEach(() => {
	localStorage.clear();
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

function binApi(summary: Partial<RouteData<'GET /api/v3/library/recycle-bin/summary'>> = {}) {
	return fakeApi({
		'GET /api/v3/library/recycle-bin': {
			items: [removed],
			next_cursor: null,
			retention_days: 30,
			bins_known: true
		},
		'GET /api/v3/library/recycle-bin/summary': {
			files: 2,
			bytes: 2 * 1024 * 1024,
			staying: 1,
			older_folders: 0,
			as_of: AS_OF,
			...summary
		},
		'GET /api/v3/library/recycle-bin/status': { state: 'idle', last_emptied: null },
		'POST /api/v3/library/recycle-bin/empty': { joined: false },
		'POST /api/v3/library/albums/{album_id}/removals/{removal_id}/put-back': { missing: [] }
	});
}

describe('the recycle bin page', () => {
	it('lists a removal and puts it back through its album', async () => {
		const api = binApi();
		await renderWithQuery(RecycleBin, {});

		await expect.element(page.getByText('Removed files stay here for 30 days.')).toBeVisible();
		await expect.element(page.getByText('Album · 2 files · 2.0 MB')).toBeVisible();
		await page.getByRole('button', { name: 'Put back' }).click();

		await expect
			.poll(
				() =>
					api.calls('POST /api/v3/library/albums/{album_id}/removals/{removal_id}/put-back').length
			)
			.toBe(1);
		const [call] = api.calls(
			'POST /api/v3/library/albums/{album_id}/removals/{removal_id}/put-back'
		);
		expect(call?.params).toEqual({ album_id: 'album-1', removal_id: 'removal-1' });
	});

	it('empties what the bin held when the confirm was read', async () => {
		const api = binApi();
		await renderWithQuery(RecycleBin, {});

		await page.getByRole('button', { name: 'Empty recycle bin' }).click();
		await expect.element(page.getByText('Delete 2 files (2.0 MB) for good?')).toBeVisible();
		await expect.element(page.getByText('1 file stays: held, in use or not found.')).toBeVisible();
		await page.getByRole('button', { name: 'Empty', exact: true }).click();

		await expect.poll(() => api.calls('POST /api/v3/library/recycle-bin/empty').length).toBe(1);
		const [call] = api.calls('POST /api/v3/library/recycle-bin/empty');
		expect(call?.body).toEqual({ before: AS_OF });
	});

	it('offers no Empty when a fresh count finds nothing to delete', async () => {
		binApi({ files: 0, bytes: 0, staying: 2 });
		await renderWithQuery(RecycleBin, {});

		await page.getByRole('button', { name: 'Empty recycle bin' }).click();
		await expect.element(page.getByText('Nothing to delete right now.')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Empty', exact: true }))
			.not.toBeInTheDocument();
	});
});
