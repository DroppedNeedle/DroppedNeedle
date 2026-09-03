import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { LocalTrackInfo, JellyfinTrackInfo } from '$lib/types';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { playQueue: vi.fn() }
}));

vi.mock('$lib/constants', () => ({
	API: {
		stream: {
			local: (id: number | string) => `/api/v1/stream/local/${id}`,
			jellyfin: (id: string) => `/api/v1/stream/jellyfin/${id}`
		}
	}
}));

import { playerStore } from '$lib/stores/player.svelte';
import { launchLocalPlayback } from './launchLocalPlayback';
import { launchJellyfinPlayback } from './launchJellyfinPlayback';

const meta: PlaybackMeta = {
	albumId: 'album-1',
	albumName: 'Test Album',
	artistName: 'Test Artist',
	coverUrl: '/cover.jpg',
	artistId: 'artist-1'
};

describe('launchLocalPlayback', () => {
	beforeEach(() => vi.clearAllMocks());

	it('maps LocalTrackInfo[] to QueueItem[] with sourceType local', () => {
		const tracks: LocalTrackInfo[] = [
			{
				track_file_id: '42',
				title: 'Song A',
				track_number: 1,
				format: 'FLAC',
				size_bytes: 30_000_000
			}
		];

		launchLocalPlayback(tracks, 0, false, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const items: QueueItem[] = call[0];

		expect(items).toHaveLength(1);
		expect(items[0]).toEqual(
			expect.objectContaining({
				trackSourceId: '42',
				trackName: 'Song A',
				sourceType: 'local',
				streamUrl: '/api/v1/stream/local/42',
				format: 'flac'
			})
		);
	});

	it('always sets a non-null coverUrl on queue items', () => {
		const tracks: LocalTrackInfo[] = [
			{ track_file_id: '1', title: 'A', track_number: 1, format: 'flac', size_bytes: 1000 }
		];
		const metaWithNullCover: PlaybackMeta = { ...meta, coverUrl: null };

		launchLocalPlayback(tracks, 0, false, metaWithNullCover);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].coverUrl).toBeTruthy();
		expect(typeof items[0].coverUrl).toBe('string');
	});
	it('nulls coverRemoteUrl for local proxy paths but keeps the proxied coverUrl', () => {
		const tracks: LocalTrackInfo[] = [
			{ track_file_id: '1', title: 'A', track_number: 1, format: 'flac', size_bytes: 1000 }
		];
		const proxyCover = '/api/v1/covers/release-group/album-1?size=250';

		launchLocalPlayback(tracks, 0, false, { ...meta, coverUrl: proxyCover });

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].coverRemoteUrl).toBeNull();
		expect(items[0].coverUrl).toBeTruthy();
	});

	it('preserves coverRemoteUrl for https remote covers', () => {
		const tracks: LocalTrackInfo[] = [
			{ track_file_id: '1', title: 'A', track_number: 1, format: 'flac', size_bytes: 1000 }
		];
		const remoteCover = 'https://r2.theaudiodb.com/images/media/album/thumb/abc123.jpg';

		launchLocalPlayback(tracks, 0, false, { ...meta, coverUrl: remoteCover });

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].coverRemoteUrl).toBe(remoteCover);
	});

	it('passes startIndex and shuffle through to playerStore', () => {
		const tracks: LocalTrackInfo[] = [
			{
				track_file_id: '1',
				title: 'A',
				track_number: 1,
				format: 'mp3',
				size_bytes: 5_000_000
			},
			{
				track_file_id: '2',
				title: 'B',
				track_number: 2,
				format: 'mp3',
				size_bytes: 5_000_000
			}
		];

		launchLocalPlayback(tracks, 1, true, meta);

		expect(playerStore.playQueue).toHaveBeenCalledWith(expect.any(Array), 1, true);
	});
});

describe('launchJellyfinPlayback', () => {
	beforeEach(() => vi.clearAllMocks());

	it('maps JellyfinTrackInfo[] to QueueItem[] with sourceType jellyfin', () => {
		const tracks: JellyfinTrackInfo[] = [
			{
				jellyfin_id: 'jf-abc',
				title: 'Jelly Song',
				track_number: 3,
				duration_seconds: 240,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				codec: 'FLAC'
			}
		];

		launchJellyfinPlayback(tracks, 0, false, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const items: QueueItem[] = call[0];

		expect(items).toHaveLength(1);
		expect(items[0]).toEqual(
			expect.objectContaining({
				trackSourceId: 'jf-abc',
				trackName: 'Jelly Song',
				sourceType: 'jellyfin',
				format: 'flac'
			})
		);
	});

	it('always sets a non-null coverUrl on queue items', () => {
		const tracks: JellyfinTrackInfo[] = [
			{
				jellyfin_id: 'jf-abc',
				title: 'Song',
				track_number: 1,
				duration_seconds: 120,
				album_name: 'Album',
				artist_name: 'Artist',
				codec: 'mp3'
			}
		];
		const metaWithNullCover: PlaybackMeta = { ...meta, coverUrl: null };

		launchJellyfinPlayback(tracks, 0, false, metaWithNullCover);

		const items: QueueItem[] = vi.mocked(playerStore.playQueue).mock.calls[0][0];
		expect(items[0].coverUrl).toBeTruthy();
		expect(typeof items[0].coverUrl).toBe('string');
	});

	it('aligns streamUrl format parameter with QueueItem format', () => {
		const tracks: JellyfinTrackInfo[] = [
			{
				jellyfin_id: 'jf-abc',
				title: 'Jelly Song',
				track_number: 3,
				duration_seconds: 240,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				codec: 'FLAC'
			}
		];

		launchJellyfinPlayback(tracks, 0, false, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const item: QueueItem = call[0][0];

		expect(item.streamUrl).toBe('/api/v1/stream/jellyfin/jf-abc');
		expect(item.format).toBe('flac');
	});

	it('falls back to aac when codec is null', () => {
		const tracks: JellyfinTrackInfo[] = [
			{
				jellyfin_id: 'jf-xyz',
				title: 'No Codec',
				track_number: 1,
				duration_seconds: 180,
				album_name: 'Test Album',
				artist_name: 'Test Artist',
				codec: null
			}
		];

		launchJellyfinPlayback(tracks, 0, false, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const items: QueueItem[] = call[0];

		expect(items[0].format).toBe('aac');
	});

	it('falls back to aac when codec is undefined', () => {
		const tracks: JellyfinTrackInfo[] = [
			{
				jellyfin_id: 'jf-xyz',
				title: 'No Codec',
				track_number: 1,
				duration_seconds: 180,
				album_name: 'Test Album',
				artist_name: 'Test Artist'
			}
		];

		launchJellyfinPlayback(tracks, 0, false, meta);

		const call = vi.mocked(playerStore.playQueue).mock.calls[0];
		const items: QueueItem[] = call[0];

		expect(items[0].format).toBe('aac');
	});
});
