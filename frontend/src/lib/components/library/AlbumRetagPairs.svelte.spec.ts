import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';
import type { RetagPair } from '$lib/queries/library/LibraryRetagPairQueries.svelte';

import AlbumRetagPairs from './AlbumRetagPairs.svelte';

function pair(id: string): RetagPair {
	const side = (albumId: string, title: string) => ({
		id: albumId,
		title,
		album_artist_name: 'Artist',
		track_count: 10,
		identity_token: '0',
		edition: null
	});
	return {
		old_album: side('album-1', 'This Album'),
		new_album: side(`${id}-new`, `Other ${id}`),
		reason: 'tracks_elsewhere',
		editions_differ: false,
		recorded_at: 0
	};
}

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Ada',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'Ada'
		})
	);
});

describe("an album's retag pairs", () => {
	it('lists the pairs of every page, asked for this album', async () => {
		const api = fakeApi({
			'GET /api/v3/library/retag-pairs': ({ query }) =>
				query.get('cursor') === 'page-2'
					? { items: [pair('b')], next_cursor: null, total: 2 }
					: { items: [pair('a')], next_cursor: 'page-2', total: 2 }
		});
		await renderWithQuery(AlbumRetagPairs, { albumId: 'album-1' });

		await expect.element(page.getByRole('link', { name: 'Other a' })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Other b' })).toBeVisible();
		const calls = api.calls('GET /api/v3/library/retag-pairs');
		expect(calls.map((call) => call.query.get('album_id'))).toEqual(['album-1', 'album-1']);
	});
});
