import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { SearchResultItemV3 } from '$lib/features/search/queries/SearchV3Adapters';
import { toSearchSongs } from '$lib/features/search/queries/SearchV3Adapters';

import SearchSongList from './SearchSongList.svelte';

// With the lazy loader off, each cover keeps its URL in data-src and fetches nothing.
vi.mock('$lib/media/lazyImage', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	lazyImage: () => ({ destroy() {} })
}));

const RECORDING = 'b6df9d04-2303-4ec7-a416-367b06a026cb';
const GROUP = '7f6b0669-a989-4fd5-94cf-2ee639ad4329';
const RELEASE = 'f0285f29-8d70-469c-8149-97adbd6af873';

// Song rows as the server sends them: a library song carries its id and
// library details, a MusicBrainz song its recording and what requesting
// it needs, and a row with neither is not a song to show.
const rows: SearchResultItemV3[] = [
	{
		id: 'track-1',
		kind: 'track',
		title: 'Sunflowers',
		artist: 'Seawayz',
		in_library: true,
		requested: false,
		score: 90,
		art_missing: false,
		track: {
			album_id: 'album-1',
			album_title: 'Sunflowers',
			disc_number: 1,
			track_number: 1,
			duration_seconds: 200,
			format: 'flac',
			cover_available: false
		}
	},
	{
		kind: 'track',
		title: 'Sunflower',
		artist: 'Post Malone & Swae Lee',
		year: 2018,
		musicbrainz_id: RECORDING,
		in_library: false,
		requested: false,
		score: 100,
		art_missing: false,
		remote_track: {
			release_group_mbid: GROUP,
			release_mbid: RELEASE,
			album_title: 'Sunflower',
			duration_seconds: 158
		}
	},
	{
		kind: 'track',
		title: 'Sunflower (remix)',
		artist: 'Post Malone',
		musicbrainz_id: 'de08b495-9dd1-40bd-b866-3006b88715a8',
		in_library: false,
		requested: true,
		score: 90,
		art_missing: false,
		remote_track: {
			release_group_mbid: 'a5c1e7c3-0f7d-4a49-9b8b-3f3e2a1f1c11',
			release_mbid: 'a18f2ae8-91d3-4d3b-afd4-d76a6365aceb',
			album_title: 'Sunflower (remix)'
		}
	},
	{
		kind: 'track',
		title: 'Loose',
		in_library: false,
		requested: false,
		score: 60,
		art_missing: false
	}
];

describe('search song list', () => {
	it('plays library songs and requests the rest under the release shown', async () => {
		const api = fakeApi({
			'POST /api/v3/requests/tracks': { status: 'queued', task_id: 'task-1' }
		});
		const songs = toSearchSongs(rows);
		expect(songs.map((song) => song.title)).toEqual([
			'Sunflowers',
			'Sunflower',
			'Sunflower (remix)'
		]);
		await renderWithQuery(SearchSongList, { songs });

		await expect.element(page.getByRole('button', { name: 'Play Sunflowers' })).toBeVisible();
		const albumLink = page.getByRole('link', { name: 'Sunflower', exact: true });
		await expect.element(albumLink).toHaveAttribute('href', expect.stringContaining(GROUP));
		await expect.element(page.getByText('Requested')).toBeVisible();

		await page.getByRole('button', { name: 'Request this track' }).click();
		await expect.poll(() => api.calls('POST /api/v3/requests/tracks').length).toBe(1);
		expect(api.calls('POST /api/v3/requests/tracks')[0].body).toMatchObject({
			recording_mbid: RECORDING,
			artist_name: 'Post Malone & Swae Lee',
			track_title: 'Sunflower',
			release_group_mbid: GROUP,
			release_mbid: RELEASE,
			duration_seconds: 158
		});
	});
});
