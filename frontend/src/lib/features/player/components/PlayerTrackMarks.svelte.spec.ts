import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

// A Jellyfin track whose library copy is track-1: the heart and stars are the copy's.
vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: {
		nowPlaying: {
			albumId: 'album-1',
			albumName: 'Bloom',
			artistName: 'Beach House',
			coverUrl: null,
			sourceType: 'jellyfin',
			trackSourceId: 'jf-1',
			trackName: 'Myth'
		},
		currentQueueItem: {
			trackSourceId: 'jf-1',
			trackName: 'Myth',
			artistName: 'Beach House',
			trackNumber: 1,
			albumId: 'album-1',
			albumName: 'Bloom',
			coverUrl: null,
			sourceType: 'jellyfin',
			sourceIds: { jellyfin: 'jf-1', local: 'track-1' }
		}
	}
}));

import PlayerTrackMarks from './PlayerTrackMarks.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';

type TrackPage = RouteData<'GET /api/v3/library/tracks'>;

const favorites = (hearted: boolean): TrackPage => ({
	total: hearted ? 1 : 0,
	next: null,
	items: hearted
		? [
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
					favorite: true
				}
			]
		: []
});

beforeEach(() => {
	localStorage.clear();
	queryClient.clear();
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
	it('hearts and rates the library copy of the track playing', async () => {
		let hearted = false;
		let rating = 0;
		const api = fakeApi({
			'GET /api/v3/library/tracks': () => favorites(hearted),
			'POST /api/v3/ratings/lookup': () => {
				const tracks: Record<string, number> = rating ? { 'track-1': rating } : {};
				return { albums: {}, artists: {}, tracks };
			},
			'PUT /api/v3/favorites/{kind}/{item_id}': () => {
				hearted = true;
				return { kind: 'track', item_id: 'track-1', favorited: true };
			},
			'PUT /api/v3/ratings/{kind}/{item_id}': () => {
				rating = 4;
				return { kind: 'track', item_id: 'track-1', rating: 4 };
			}
		});
		await renderWithQuery(PlayerTrackMarks, {}, { client: queryClient });

		await page.getByRole('button', { name: 'Add to favourites' }).click();
		await expect
			.element(page.getByRole('button', { name: 'Remove from favourites' }))
			.toBeVisible();
		expect(api.calls('PUT /api/v3/favorites/{kind}/{item_id}')[0].params.item_id).toBe('track-1');
		// The favourites list is read again, so the heart stays once the save settles.
		await expect.poll(() => api.calls('GET /api/v3/library/tracks').length).toBe(2);
		await expect
			.element(page.getByRole('button', { name: 'Remove from favourites' }))
			.toBeVisible();

		await page.getByRole('button', { name: '4 stars' }).click();
		await expect.element(page.getByRole('group')).toHaveAccessibleName('Myth: 4 of 5 stars');
		const rated = api.calls('PUT /api/v3/ratings/{kind}/{item_id}');
		expect(rated[0].params.item_id).toBe('track-1');
		expect(rated[0].body).toEqual({ rating: 4 });
	});
});
