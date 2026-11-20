import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi, type FakeRoutes } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import AddToPlaylistModal from './AddToPlaylistModal.svelte';
import type { TrackData } from '$lib/api/playlists';
import type { QueueItem } from '$lib/features/player/playback/types';

const ADD = 'POST /api/v3/playlists/{playlist_id}/tracks';
const CHECK = 'POST /api/v3/playlists/check-tracks';

function makeTrack(overrides: Partial<QueueItem> = {}): QueueItem {
	return {
		trackSourceId: 'v1',
		trackName: 'Test Track',
		artistName: 'Test Artist',
		trackNumber: 1,
		albumId: 'a1',
		albumName: 'Test Album',
		coverUrl: null,
		sourceType: 'local',
		...overrides
	};
}

function playlist(id: string, name: string) {
	return {
		id,
		name,
		track_count: 5,
		total_duration: 600,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: 1767225600,
		updated_at: 1767225600,
		is_public: false,
		is_owner: true,
		is_shared: false,
		can_edit: true,
		can_delete: true,
		owner_name: null,
		is_redacted: false
	};
}

/** The list and an empty membership answer, plus `routes`. */
function serve(routes: FakeRoutes = {}) {
	return fakeApi({
		'GET /api/v3/playlists': {
			playlists: [playlist('p1', 'My Playlist'), playlist('p2', 'Another')]
		},
		[CHECK]: { membership: {} },
		[ADD]: { tracks: [] },
		...routes
	});
}

type ModalRef = { open: (tracks: (QueueItem | TrackData)[]) => void };

async function openWith(tracks: (QueueItem | TrackData)[]) {
	const view = await renderWithQuery(AddToPlaylistModal, {});
	(view.component as unknown as ModalRef).open(tracks);
	await expect.element(page.getByText('My Playlist')).toBeVisible();
	return view;
}

function addedNames(api: ReturnType<typeof serve>): string[] {
	const [call] = api.calls(ADD);
	const body = call?.body as { tracks: { track_name: string }[] } | undefined;
	return body?.tracks.map((track) => track.track_name) ?? [];
}

describe('AddToPlaylistModal.svelte', () => {
	beforeEach(() => {
		localStorage.clear();
	});

	it('sends no track twice: a second click adds nothing and a partial add skips the ones there', async () => {
		let api = serve();
		const view = await openWith([makeTrack()]);
		await page.getByLabelText('Add to My Playlist').click();
		await expect.element(page.getByLabelText('Already added').first()).toBeVisible();
		expect(api.calls(ADD)).toHaveLength(1);
		view.unmount();

		api = serve({ [CHECK]: { membership: { '0': ['p1'] } } });
		await openWith([
			makeTrack({ trackName: 'Track 1' }),
			makeTrack({ trackName: 'Track 2', trackSourceId: 'v2' })
		]);
		await page.getByLabelText('Add the remaining tracks to My Playlist').click();
		await expect.poll(() => api.calls(ADD).length).toBe(1);
		expect(addedNames(api)).toEqual(['Track 2']);
	});

	it('checks more than 1000 tracks in chunks and merges their membership', async () => {
		// Each chunk answers that its first track is already in My Playlist:
		// tracks 0 and 1000 of the whole list.
		const api = serve({ [CHECK]: { membership: { '0': ['p1'] } } });
		const tracks = Array.from({ length: 1001 }, (_, i) =>
			makeTrack({ trackName: `Track ${i}`, trackSourceId: `v${i}` })
		);
		await openWith(tracks);

		const sizes = api
			.calls(CHECK)
			.map((call) => (call.body as { tracks: unknown[] }).tracks.length);
		expect(sizes).toEqual([1000, 1]);

		await page.getByLabelText('Add the remaining tracks to My Playlist').click();

		await expect.poll(() => api.calls(ADD).length).toBe(1);
		const added = addedNames(api);
		expect(added).toHaveLength(999);
		expect(added).not.toContain('Track 0');
		expect(added).not.toContain('Track 1000');
	});

	it('adds a song the library lacks as an entry that waits for its album', async () => {
		const api = serve();
		const entry: TrackData = {
			track_name: 'Opener',
			artist_name: 'Aurora',
			album_name: 'First Light',
			album_id: 'rg-first-light',
			artist_id: null,
			track_source_id: null,
			cover_url: '/api/v3/covers/release-group/rg-first-light?size=500',
			source_type: '',
			available_sources: null,
			format: null,
			track_number: null,
			disc_number: null,
			duration: 248,
			plex_rating_key: null,
			recording_mbid: 'rec-opener'
		};
		await openWith([entry]);
		await page.getByLabelText('Add to My Playlist').click();

		await expect.poll(() => api.calls(ADD).length).toBe(1);
		const [call] = api.calls(ADD);
		expect((call?.body as { tracks: TrackData[] }).tracks).toEqual([entry]);
	});
});
