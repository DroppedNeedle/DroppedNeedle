import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
// Keep the image on its first source: the test has no server to answer it.
vi.mock('$lib/utils/lazyImage', () => ({
	lazyImage: () => ({ destroy() {} }),
	resetLazyImage: () => {}
}));

import SearchPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { playerStore } from '$lib/stores/player.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

type SearchResponse = RouteData<'GET /api/v3/search'>;

const empty: SearchResponse = {
	artists: [],
	albums: [],
	tracks: [],
	top_artist: null,
	top_album: null,
	top_track: null,
	artist_status: 'ok',
	album_status: 'ok',
	track_status: 'ok'
};

const withSong: SearchResponse = {
	...empty,
	tracks: [
		{
			kind: 'track',
			id: 'track-1',
			title: 'Alien Superstar',
			artist: 'Beyoncé',
			year: 2022,
			musicbrainz_id: null,
			in_library: true,
			requested: false,
			score: 100,
			disambiguation: null,
			type_info: null,
			track: {
				album_id: 'album-1',
				album_title: 'Renaissance',
				artist_id: null,
				release_group_mbid: null,
				disc_number: 1,
				track_number: 3,
				duration_seconds: 215,
				format: 'FLAC',
				cover_available: false
			}
		}
	]
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

afterEach(() => {
	vi.restoreAllMocks();
});

describe('search page songs', () => {
	it('shows a library song and plays it on click', async () => {
		fakeApi({ 'GET /api/v3/search': withSong });
		const play = vi.spyOn(playerStore, 'playQueue').mockImplementation(() => {});
		await renderWithQuery(SearchPage, { data: { query: 'alien' } });

		const row = page.getByRole('button', { name: 'Play Alien Superstar' });
		await expect.element(row).toBeVisible();
		await expect.element(page.getByText('3:35')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Renaissance' }))
			.toHaveAttribute('href', '/album/album-1');

		await row.click();
		expect(play).toHaveBeenCalledOnce();
		const [items, index] = play.mock.calls[0];
		expect(index).toBe(0);
		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({
			trackSourceId: 'track-1',
			sourceType: 'local',
			albumId: 'album-1',
			trackNumber: 3
		});
	});

	it('hides the songs shelf when no song matches', async () => {
		fakeApi({ 'GET /api/v3/search': empty });
		await renderWithQuery(SearchPage, { data: { query: 'alien' } });

		await expect.element(page.getByText('No artists found')).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Songs' })).not.toBeInTheDocument();
	});
});
