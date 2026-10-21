import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

import RecycledRemovals from './RecycledRemovals.svelte';

type Removal = RouteData<'GET /api/v3/library/albums/{album_id}/removals'>['items'][number];

const removed: Removal = {
	id: 'removal-1',
	kind: 'album',
	removed_at: Date.UTC(2026, 9, 3),
	files: 3,
	held: false
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'curator-1',
			display_name: 'Curator',
			role: 'trusted',
			email: null,
			avatar_url: null,
			username: 'curator',
			username_display: 'Curator'
		})
	);
});

describe('removed files on the album page', () => {
	it('puts a removal back and names the file that stayed out', async () => {
		const api = fakeApi({
			'GET /api/v3/library/albums/{album_id}/removals': { items: [removed] },
			'POST /api/v3/library/albums/{album_id}/removals/{removal_id}/put-back': {
				missing: ['Band/LP/03 Song.flac']
			}
		});
		await renderWithQuery(RecycledRemovals, { albumId: 'album-1' });

		await expect.element(page.getByText(/Album · 3 files · removed/)).toBeVisible();
		await page.getByRole('button', { name: 'Put back' }).click();

		await expect
			.element(page.getByText('Not in the recycle bin any more: 03 Song.flac'))
			.toBeVisible();
		const [call] = api.calls(
			'POST /api/v3/library/albums/{album_id}/removals/{removal_id}/put-back'
		);
		expect(call?.params).toEqual({ album_id: 'album-1', removal_id: 'removal-1' });
	});
});
