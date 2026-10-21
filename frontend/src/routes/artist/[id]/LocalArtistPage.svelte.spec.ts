import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import LocalArtistPage from './LocalArtistPage.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const artist: RouteData<'GET /api/v3/library/artists/{id}'> = {
	id: 'artist-guest',
	name: 'Guest Singer',
	artist_mbid: null,
	identity_state: 'local_only',
	album_count: 1,
	track_count: 4,
	appearance_album_count: 1,
	appearance_track_count: 2,
	date_added: 1,
	favorite: false
};

const album: RouteData<'GET /api/v3/library/albums/{id}'> = {
	id: 'album-lead',
	title: 'Someone Else',
	artist_id: 'artist-lead',
	artist_name: 'Lead Band',
	identity_state: 'local_only',
	is_compilation: false,
	track_count: 2,
	total_duration_seconds: 400,
	total_size_bytes: 1024,
	format: 'flac',
	year: 2020,
	cover_available: false,
	favorite: false,
	date_added: 1
};

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

describe('local artist page', () => {
	it('shows the appearance line and the appearances section for a guest artist', async () => {
		fakeApi({
			'GET /api/v3/library/artists/{id}': artist,
			'GET /api/v3/library/artists/{id}/albums': { items: [], total: 0, next: null },
			'GET /api/v3/library/artists/{id}/appearances': {
				items: [album],
				total: 1,
				next: null
			},
			'GET /api/v3/library/tracks': { items: [], total: 0, next: null },
			'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: {} }
		});
		await renderWithQuery(LocalArtistPage, { artistId: artist.id });

		await expect.element(page.getByText('Appears on 2 local tracks')).toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Appears in your library' }))
			.toBeVisible();
	});
});
