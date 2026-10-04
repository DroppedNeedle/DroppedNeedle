import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { PlexTrackInfo } from '$lib/player/types';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { playQueue: vi.fn() }
}));


vi.mock('$lib/utils/errorHandling', () => ({
	getCoverUrl: (url: string | null, albumId: string) => url || `/api/v1/covers/${albumId}`
}));

vi.mock('$lib/player/queueHelpers', () => ({
	normalizeCodec: (codec: string | null | undefined) => (codec ? codec.toLowerCase() : null),
	normalizeDiscNumber: (disc: number | null | undefined) => disc ?? 1
}));

import { playerStore } from '$lib/stores/player.svelte';
import { launchPlexPlayback } from './launchPlexPlayback';

const meta: PlaybackMeta = {
	albumId: 'album-1',
	albumName: 'Test Album',
	artistName: 'Test Artist',
	coverUrl: '/cover.jpg',
	artistId: 'artist-1'
};

describe('launchPlexPlayback', () => {
	beforeEach(() => vi.clearAllMocks());

	it('maps PlexTrackInfo[] to QueueItem[] with sourceType plex', () => {
		expect.assertions(6);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-100',
				title: 'Song A',
				track_number: 1,
				disc_number: 1,
				duration_seconds: 180,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/library/parts/100/1234/file.flac',
				codec: 'FLAC',
				bitrate: 1411
			}
		];

		launchPlexPlayback(tracks, 0, false, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const items: QueueItem[] = call[0];

		expect(items).toHaveLength(1);
		expect(items[0].trackSourceId).toBe('/library/parts/100/1234/file.flac');
		expect(items[0].trackName).toBe('Song A');
		expect(items[0].sourceType).toBe('plex');
		expect(items[0].streamUrl).toBe('/api/v3/stream/plex//library/parts/100/1234/file.flac');
		expect(items[0].plexRatingKey).toBe('pk-100');
	});

	it('does not start playback when all tracks have null part_key', () => {
		expect.assertions(1);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-200',
				title: 'Song B',
				track_number: 2,
				disc_number: 1,
				duration_seconds: 200,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: null,
				codec: null,
				bitrate: null
			}
		];

		launchPlexPlayback(tracks, 0, false, meta);

		expect(vi.mocked(playerStore.playQueue)).not.toHaveBeenCalled();
	});

	it('sets plexRatingKey on all queue items', () => {
		expect.assertions(2);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-1',
				title: 'A',
				track_number: 1,
				disc_number: 1,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/1',
				codec: 'mp3',
				bitrate: 320
			},
			{
				plex_id: 'pk-2',
				title: 'B',
				track_number: 2,
				disc_number: 1,
				duration_seconds: 200,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/2',
				codec: 'flac',
				bitrate: 1411
			}
		];

		launchPlexPlayback(tracks, 0, false, meta);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].plexRatingKey).toBe('pk-1');
		expect(items[1].plexRatingKey).toBe('pk-2');
	});

	it('adjusts startIndex for filtered streamable tracks', () => {
		expect.assertions(2);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-0',
				title: 'No Stream',
				track_number: 1,
				disc_number: 1,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: null,
				codec: null,
				bitrate: null
			},
			{
				plex_id: 'pk-1',
				title: 'A',
				track_number: 2,
				disc_number: 1,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/1',
				codec: 'mp3',
				bitrate: 320
			},
			{
				plex_id: 'pk-2',
				title: 'B',
				track_number: 3,
				disc_number: 1,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/2',
				codec: 'mp3',
				bitrate: 320
			}
		];

		launchPlexPlayback(tracks, 2, true, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		expect(call[1]).toBe(1);
		expect(call[2]).toBe(true);
	});

	it('normalizes codec format', () => {
		expect.assertions(1);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-1',
				title: 'A',
				track_number: 1,
				disc_number: 1,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/1',
				codec: 'FLAC',
				bitrate: 1411
			}
		];

		launchPlexPlayback(tracks, 0, false, meta);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].format).toBe('flac');
	});

	it('handles null coverUrl in meta', () => {
		expect.assertions(1);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-1',
				title: 'A',
				track_number: 1,
				disc_number: 1,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/1',
				codec: 'mp3',
				bitrate: 320
			}
		];
		const metaWithNullCover: PlaybackMeta = { ...meta, coverUrl: null };

		launchPlexPlayback(tracks, 0, false, metaWithNullCover);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].coverUrl).toBeTruthy();
	});

	it('sets disc_number from track data', () => {
		expect.assertions(1);
		const tracks: PlexTrackInfo[] = [
			{
				plex_id: 'pk-1',
				title: 'A',
				track_number: 3,
				disc_number: 2,
				duration_seconds: 100,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				part_key: '/p/1',
				codec: 'mp3',
				bitrate: 320
			}
		];

		launchPlexPlayback(tracks, 0, false, meta);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].discNumber).toBe(2);
	});
});
