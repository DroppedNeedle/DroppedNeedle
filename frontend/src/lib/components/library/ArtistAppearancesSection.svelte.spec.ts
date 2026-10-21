import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import ArtistAppearancesSection from './ArtistAppearancesSection.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const ARTIST_MBID = 'af591c1b-4dfe-4708-993e-f6ac6aa6d2ac';
const LIBRARY_ID = '659cab1f-92ab-536b-9398-96a4ea0a4a90';

const artist: RouteData<'GET /api/v3/library/artists/{id}'> = {
	id: LIBRARY_ID,
	name: 'Danny Worsnop',
	artist_mbid: ARTIST_MBID,
	identity_state: 'linked',
	album_count: 0,
	track_count: 1,
	appearance_album_count: 1,
	appearance_track_count: 1,
	date_added: 1,
	favorite: false
};

const album: RouteData<'GET /api/v3/library/albums/{id}'> = {
	id: 'album-1',
	title: 'Guest Spot',
	artist_id: 'artist-lead',
	artist_name: 'Lead Band',
	identity_state: 'linked',
	is_compilation: false,
	track_count: 1,
	total_duration_seconds: 200,
	total_size_bytes: 1024,
	format: 'flac',
	year: 2020,
	cover_available: false,
	favorite: false,
	date_added: 1
};

const track: RouteData<'GET /api/v3/library/tracks'>['items'][number] = {
	id: 'track-1',
	title: 'Featured Verse',
	album_id: album.id,
	album_title: album.title,
	artist_id: 'artist-lead',
	artist_name: 'Lead Band',
	album_artist_id: 'artist-lead',
	album_artist_name: 'Lead Band',
	disc_number: 1,
	track_number: 1,
	year: 2020,
	genre: null,
	duration_seconds: 200,
	format: 'flac',
	bit_rate: 900000,
	sample_rate: 44100,
	bit_depth: 16,
	channels: 2,
	file_size_bytes: 1024,
	date_added: 1,
	cover_available: false,
	favorite: false
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

describe('ArtistAppearancesSection on a provider artist page', () => {
	it("lists the library artist's tracks when given its MusicBrainz id", async () => {
		const known = (id: string) => id === LIBRARY_ID || id === ARTIST_MBID;
		const api = fakeApi({
			'GET /api/v3/library/artists/{id}': ({ params }) =>
				known(params.id) ? artist : apiError(404, 'NOT_FOUND'),
			'GET /api/v3/library/artists/{id}/appearances': ({ params }) =>
				known(params.id) ? { items: [album], total: 1, next: null } : apiError(404, 'NOT_FOUND'),
			'GET /api/v3/library/tracks': ({ query }) => ({
				items: query.get('artist_id') === LIBRARY_ID ? [track] : [],
				total: query.get('artist_id') === LIBRARY_ID ? 1 : 0,
				next: null
			})
		});
		await renderWithQuery(ArtistAppearancesSection, { artistId: ARTIST_MBID });

		await expect.element(page.getByText('Featured Verse')).toBeVisible();
		expect(api.calls('GET /api/v3/library/tracks')[0].query.get('artist_id')).toBe(LIBRARY_ID);
	});

	it('shows no section when no library artist carries the id', async () => {
		fakeApi({
			'GET /api/v3/library/artists/{id}': apiError(404, 'NOT_FOUND'),
			'GET /api/v3/library/artists/{id}/appearances': apiError(404, 'NOT_FOUND')
		});
		const { client } = await renderWithQuery(ArtistAppearancesSection, {
			artistId: ARTIST_MBID
		});

		await expect.poll(() => client.getQueryCache().getAll()[0]?.state.status).toBe('error');
		await expect.element(page.getByText('Appears in your library')).not.toBeInTheDocument();
		await expect
			.element(page.getByText("Couldn't load this artist's local track appearances."))
			.not.toBeInTheDocument();
	});
});
