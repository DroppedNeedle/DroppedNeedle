import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockGet = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: (...args: unknown[]) => mockGet(...args), POST: vi.fn() } }
}));

import {
	scrobbleManager,
	makeNowPlayingSubmission,
	makeScrobbleSubmission
} from './scrobble.svelte';
import { buildNowPlayingMetadata } from './playerSourceResolver';
import {
	buildQueueItemFromYouTube,
	playlistTrackToQueueItem
} from '$lib/features/player/playback/queueHelpers';
import type { QueueItem, YouTubeTrackLink } from '$lib/features/player/playback/types';
import type { PlaylistTrack } from '$lib/api/playlists';
import {
	makeTrackKey,
	shouldAccumulate,
	isLoopReset,
	shouldScrobble,
	SCROBBLE_TIME_THRESHOLD_MS,
	MIN_TRACK_DURATION_MS
} from './scrobbleHelpers';
import { authStore } from '$lib/stores/authStore.svelte';

describe('native scrobble payloads', () => {
	it('carry the source, name the track on a local play and send the recording on any other', () => {
		expect(makeNowPlayingSubmission('Artist', 'Song', 'Album', 180_000, 'navidrome')).toEqual({
			track_name: 'Song',
			artist_name: 'Artist',
			album_name: 'Album',
			duration_ms: 180_000,
			source: 'navidrome'
		});

		const play = {
			artistName: 'Artist',
			trackName: 'Song',
			albumName: 'Album',
			trackSourceId: 'item-1',
			recordingMbid: 'rec-1'
		};
		const sent = {
			track_name: 'Song',
			artist_name: 'Artist',
			album_name: 'Album',
			duration_ms: 180_000,
			timestamp: 1234
		};
		expect(makeScrobbleSubmission({ ...play, sourceType: 'local' }, 180_000, 1234)).toStrictEqual({
			...sent,
			source: 'local',
			track_id: 'item-1'
		});
		for (const source of ['navidrome', 'jellyfin', 'plex', 'youtube'] as const) {
			expect(
				makeScrobbleSubmission({ ...play, sourceType: source }, 180_000, 1234),
				source
			).toStrictEqual({ ...sent, source, mbid: 'rec-1' });
		}
	});
});

describe('a queue item reaches the scrobble', () => {
	const RECORDING = '98255a8c-017a-4bc7-8dd6-1fa36124572b';

	function submitted(item: QueueItem) {
		const playing = buildNowPlayingMetadata(item);
		return makeScrobbleSubmission(
			{
				artistName: item.artistName,
				trackName: item.trackName,
				albumName: item.albumName,
				sourceType: playing.sourceType,
				trackSourceId: playing.trackSourceId,
				recordingMbid: playing.recordingMbid
			},
			180_000,
			1234
		);
	}

	const link: YouTubeTrackLink = {
		album_id: 'album-1',
		album_name: 'Album',
		artist_name: 'Artist',
		track_name: 'Song',
		track_number: 1,
		disc_number: 1,
		video_id: 'video-1',
		embed_url: 'https://www.youtube.com/embed/video-1',
		created_at: '2026-01-01T00:00:00Z'
	};
	const meta = { albumId: 'album-1', albumName: 'Album', artistName: 'Artist', coverUrl: null };

	it('sends the recording a YouTube link or a remote entry gave, and names a library track', () => {
		{
			// A YouTube link with a recording sends it.
			const item = buildQueueItemFromYouTube({ ...link, recording_mbid: RECORDING }, meta);
			expect(submitted(item).mbid).toBe(RECORDING);
		}

		{
			// A YouTube link without one sends none.
			const item = buildQueueItemFromYouTube({ ...link, recording_mbid: null }, meta);
			expect(submitted(item)).not.toHaveProperty('mbid');
		}

		{
			// A playlist entry playing from the library names its track and sends no recording.
			const entry: PlaylistTrack = {
				id: 'entry-1',
				position: 0,
				track_name: 'Song',
				artist_name: 'Artist',
				album_name: 'Album',
				album_id: 'album-1',
				artist_id: null,
				track_source_id: 'jf-item-1',
				cover_url: null,
				source_type: 'jellyfin',
				available_sources: ['jellyfin', 'local'],
				format: null,
				track_number: 1,
				disc_number: 1,
				duration: 180,
				created_at: '2026-01-01T00:00:00Z',
				plex_rating_key: null,
				library_file_id: 'track-1',
				recording_mbid: RECORDING
			};
			const local = playlistTrackToQueueItem(entry);
			expect(local && submitted(local)).toMatchObject({ source: 'local', track_id: 'track-1' });
			expect(local && submitted(local)).not.toHaveProperty('mbid');

			const remote = playlistTrackToQueueItem({ ...entry, library_file_id: null });
			expect(remote && submitted(remote)).toMatchObject({ source: 'jellyfin', mbid: RECORDING });
		}
	});
});

describe('scrobbleManager settings', () => {
	beforeEach(() => {
		mockGet.mockReset();
		authStore.setUser({
			id: 'user-1',
			display_name: 'User',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'user',
			username_display: 'User',
			providers: ['local']
		});
	});

	it.each([
		[true, false, true],
		[false, true, true],
		[false, false, false]
	])('lastfm=%s listenbrainz=%s enables=%s', async (lastfm, listenbrainz, enabled) => {
		mockGet.mockResolvedValueOnce({
			scrobble_to_lastfm: lastfm,
			scrobble_to_listenbrainz: listenbrainz
		});
		await scrobbleManager.refreshSettings();
		expect(scrobbleManager.enabled).toBe(enabled);
	});
});

describe('scrobble rules', () => {
	it.each([
		// listened ms, duration ms, expected
		[150_000, 300_000, true], // half of a 5 minute track
		[149_999, 300_000, false],
		[SCROBBLE_TIME_THRESHOLD_MS, 600_000, true], // 4 minute cap on long tracks
		[SCROBBLE_TIME_THRESHOLD_MS - 1, 600_000, false],
		[MIN_TRACK_DURATION_MS / 2, MIN_TRACK_DURATION_MS, true], // shortest accepted track
		[MIN_TRACK_DURATION_MS - 1, MIN_TRACK_DURATION_MS - 1, false] // too short to count
	])('listening %i ms of a %i ms track scrobbles: %s', (listened, duration, expected) => {
		expect(shouldScrobble(listened, duration, false)).toBe(expected);
	});

	it('never scrobbles the same play twice', () => {
		expect(shouldScrobble(200_000, 300_000, true)).toBe(false);
	});

	it.each([
		[1, true],
		[2.5, true],
		[0, false],
		[-5, false], // seek backward
		[3, false], // seek forward at the tolerance
		[10, false]
	])('a %s second progress step counts toward listening time: %s', (delta, expected) => {
		expect(shouldAccumulate(delta)).toBe(expected);
	});

	it('treats a jump from the end back to the start as a new play', () => {
		expect(isLoopReset(179, 0.5, 180_000)).toBe(true);
		expect(isLoopReset(90, 0.5, 180_000)).toBe(false);
		expect(isLoopReset(179, 5, 180_000)).toBe(false);
	});

	it('keys a track by artist and title, ignoring case', () => {
		expect(makeTrackKey('MUSE', 'Hysteria')).toBe(makeTrackKey('muse', 'hysteria'));
		expect(makeTrackKey('Muse', 'Hysteria')).not.toBe(makeTrackKey('Muse', 'Starlight'));
	});
});
