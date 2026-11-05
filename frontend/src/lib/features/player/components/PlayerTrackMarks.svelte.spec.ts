import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

const RELEASE_GROUP = 'f1390eec-a192-4d96-9943-bbc42a819588';

vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: {
		nowPlaying: {
			albumId: 'f1390eec-a192-4d96-9943-bbc42a819588',
			albumName: 'Bloom',
			artistName: 'Beach House',
			coverUrl: null,
			sourceType: 'local',
			trackSourceId: 'track-1'
		}
	}
}));

import PlayerTrackMarks from './PlayerTrackMarks.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

type Status = RouteData<'GET /api/v3/library/albums/{id}/status'>;

const status = (favorite: boolean): Status => ({
	in_library: true,
	album_id: 'album-1',
	track_count: 1,
	tracks: [
		{
			id: 'track-1',
			title: 'Myth',
			album_id: 'album-1',
			album_title: 'Bloom',
			artist_id: 'artist-1',
			artist_name: 'Beach House',
			album_artist_id: 'artist-1',
			album_artist_name: 'Beach House',
			row_revision: 1,
			disc_number: 1,
			track_number: 1,
			year: 2012,
			genre: null,
			duration_seconds: 258,
			format: 'mp3',
			bit_rate: 320,
			sample_rate: 44100,
			bit_depth: null,
			channels: 2,
			file_size_bytes: 1,
			date_added: 1,
			cover_available: true,
			art_missing: false,
			favorite,
			current_tier: 'mp3_320',
			below_cutoff: true
		}
	]
});

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

describe('the player heart and stars', () => {
	it('reads the heart through the album named by its release group', async () => {
		const api = fakeApi({
			'GET /api/v3/library/albums/{id}/status': status(true),
			'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: { 'track-1': 3 } }
		});
		await renderWithQuery(PlayerTrackMarks, {});

		await expect
			.element(page.getByRole('button', { name: 'Remove from favourites' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Your rating: 3 of 5 stars' }))
			.toBeVisible();
		expect(api.calls('GET /api/v3/library/albums/{id}/status')[0].params.id).toBe(RELEASE_GROUP);
	});

	it('hearts the track and clears its rating with the bin', async () => {
		const api = fakeApi({
			'GET /api/v3/library/albums/{id}/status': status(false),
			'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: { 'track-1': 4 } },
			'PUT /api/v3/favorites/{kind}/{item_id}': {
				kind: 'track',
				item_id: 'track-1',
				favorited: true
			},
			'PUT /api/v3/ratings/{kind}/{item_id}': { kind: 'track', item_id: 'track-1', rating: 0 }
		});
		await renderWithQuery(PlayerTrackMarks, {});

		await page.getByRole('button', { name: 'Add to favourites' }).click();
		await expect.poll(() => api.calls('PUT /api/v3/favorites/{kind}/{item_id}').length).toBe(1);

		await page.getByRole('button', { name: 'Your rating: 4 of 5 stars' }).click();
		await page.getByRole('button', { name: 'Remove rating' }).click();
		await expect.poll(() => api.calls('PUT /api/v3/ratings/{kind}/{item_id}').length).toBe(1);
		expect(api.calls('PUT /api/v3/ratings/{kind}/{item_id}')[0].body).toEqual({ rating: 0 });
	});
});
