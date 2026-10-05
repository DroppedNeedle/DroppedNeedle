import { describe, it, expect, vi } from 'vitest';

vi.mock('$lib/utils/errorHandling', () => ({
	getCoverUrl: (url: string | null, albumId: string) => url ?? `/cover/${albumId}`
}));

import type { JellyfinTrackInfo, LocalTrackInfo, NativeTrackListItem } from '$lib/player/types';
import type { PlaylistTrack } from '$lib/api/playlists';
import type { TrackMeta, TrackSourceData } from './queueHelpers';
import {
	selectBestSource,
	buildQueueItem,
	buildQueueItemsFromJellyfin,
	buildQueueItemsFromLocal,
	buildDiscoveryQueueFromLocal,
	buildQueueItemFromYouTube,
	compareDiscTrack,
	getDiscTrackKey,
	normalizeCodec,
	playlistTrackToQueueItem
} from './queueHelpers';

const baseMeta: TrackMeta = {
	albumId: 'album-1',
	albumName: 'Test Album',
	artistName: 'Artist A',
	coverUrl: '/cover.jpg',
	artistId: 'artist-1'
};

const localTrack: LocalTrackInfo = {
	track_file_id: '42',
	title: 'Local Song',
	track_number: 1,
	format: 'FLAC',
	size_bytes: 30_000_000,
	duration_seconds: 240
};

const jellyfinTrack: JellyfinTrackInfo = {
	jellyfin_id: 'jf-123',
	title: 'JF Song',
	track_number: 2,
	duration_seconds: 180,
	album_name: 'Test Album',
	artist_name: 'Artist A',
	codec: 'opus'
};

describe('selectBestSource', () => {
	it('prefers local over jellyfin (Local > Jellyfin priority)', () => {
		expect.assertions(1);
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'Track',
			localTrack,
			jellyfinTrack
		};
		expect(selectBestSource(data)!.sourceType).toBe('local');
	});
});

describe('buildQueueItem', () => {
	it('builds a queue item from local track data', () => {
		expect.assertions(6);
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'Local Song',
			trackLength: 240,
			localTrack
		};
		const item = buildQueueItem(baseMeta, data);
		expect(item).not.toBeNull();
		expect(item!.trackName).toBe('Local Song');
		expect(item!.sourceType).toBe('local');
		expect(item!.albumId).toBe('album-1');
		expect(item!.availableSources).toEqual(['local']);
		expect(item!.duration).toBe(240);
	});

	it('preserves disc number on queue items', () => {
		expect.assertions(1);
		const item = buildQueueItem(baseMeta, {
			trackPosition: 1,
			discNumber: 2,
			trackTitle: 'Disc Two Song',
			localTrack
		});
		expect(item!.discNumber).toBe(2);
	});
});

describe('disc-aware track helpers', () => {
	it('builds a stable composite key from disc and track number', () => {
		expect.assertions(2);
		expect(getDiscTrackKey({ disc_number: 2, position: 5 })).toBe('2:5');
		expect(getDiscTrackKey({ track_number: 3 })).toBe('1:3');
	});

	it('sorts tracks by disc before track number', () => {
		expect.assertions(1);
		const sorted = [
			{ disc_number: 2, track_number: 1 },
			{ disc_number: 1, track_number: 3 },
			{ disc_number: 1, track_number: 1 }
		].sort(compareDiscTrack);
		expect(sorted.map((track) => getDiscTrackKey(track))).toEqual(['1:1', '1:3', '2:1']);
	});

	it('carries disc number through youtube queue items', () => {
		expect.assertions(1);
		const item = buildQueueItemFromYouTube(
			{
				album_id: 'album-1',
				album_name: 'Test Album',
				artist_name: 'Artist A',
				track_name: 'Disc Two Song',
				track_number: 1,
				disc_number: 2,
				video_id: 'video-1',
				embed_url: 'https://example.com/embed/video-1',
				created_at: '2024-01-01T00:00:00Z'
			},
			baseMeta
		);
		expect(item.discNumber).toBe(2);
	});
});

describe('buildQueueItemsFromJellyfin', () => {
	it('normalizes codec for stream URL', () => {
		expect.assertions(1);
		const track: JellyfinTrackInfo = { ...jellyfinTrack, codec: 'ALAC' };
		const items = buildQueueItemsFromJellyfin([track], baseMeta);
		expect(items[0].streamUrl).toBe('/api/v3/stream/jellyfin/jf-123');
	});

	it('defaults to aac for unknown codecs', () => {
		expect.assertions(1);
		const track: JellyfinTrackInfo = { ...jellyfinTrack, codec: 'unknown_codec' };
		const items = buildQueueItemsFromJellyfin([track], baseMeta);
		expect(items[0].streamUrl).toBe('/api/v3/stream/jellyfin/jf-123');
	});
});

describe('normalizeCodec', () => {
	it('lowercases known codecs and folds alac to flac', () => {
		expect.assertions(2);
		expect(normalizeCodec('FLAC')).toBe('flac');
		expect(normalizeCodec('ALAC')).toBe('flac');
	});

	it('keeps wma labeled as wma instead of relabeling it aac', () => {
		expect.assertions(2);
		expect(normalizeCodec('wma')).toBe('wma');
		expect(normalizeCodec('WMA')).toBe('wma');
	});

	it('passes unknown codecs through instead of calling them aac', () => {
		expect.assertions(1);
		expect(normalizeCodec('unknown_codec')).toBe('unknown_codec');
	});

	it('defaults a missing codec to aac', () => {
		expect.assertions(3);
		expect(normalizeCodec(null)).toBe('aac');
		expect(normalizeCodec(undefined)).toBe('aac');
		expect(normalizeCodec('')).toBe('aac');
	});

	it('carries the honest codec onto queue items', () => {
		expect.assertions(1);
		const track: JellyfinTrackInfo = { ...jellyfinTrack, codec: 'wma' };
		const items = buildQueueItemsFromJellyfin([track], baseMeta);
		expect(items[0].format).toBe('wma');
	});
});

describe('buildQueueItemsFromLocal', () => {
	it('nulls coverRemoteUrl for local proxy paths', () => {
		expect.assertions(1);
		const items = buildQueueItemsFromLocal([localTrack], {
			...baseMeta,
			coverUrl: '/api/v1/covers/release-group/album-1?size=250'
		});
		expect(items[0].coverRemoteUrl).toBeNull();
	});

	it('preserves coverRemoteUrl for https remote covers', () => {
		expect.assertions(1);
		const remoteCover = 'https://r2.theaudiodb.com/images/media/album/thumb/abc123.jpg';
		const items = buildQueueItemsFromLocal([localTrack], { ...baseMeta, coverUrl: remoteCover });
		expect(items[0].coverRemoteUrl).toBe(remoteCover);
	});
});

describe('buildDiscoveryQueueFromLocal', () => {
	const nativeTrack: NativeTrackListItem = {
		id: 'file-7',
		title: 'Flat Song',
		album_id: 'local-album-9',
		album_title: 'Cross Album',
		artist_id: 'local-artist-9',
		artist_name: 'Flat Artist',
		album_artist_id: 'local-artist-9',
		album_artist_name: 'Flat Artist',
		musicbrainz_recording_id: null,
		musicbrainz_release_group_id: 'rg-9',
		musicbrainz_artist_id: null,
		musicbrainz_album_artist_id: null,
		format: 'FLAC',
		track_number: 3,
		disc_number: 2,
		year: null,
		genre: null,
		duration_seconds: 200,
		bit_rate: null,
		sample_rate: null,
		bit_depth: null,
		channels: null,
		file_size_bytes: 1,
		date_added: 1,
		cover_available: true,
		current_tier: null,
		below_cutoff: false
	};

	it('carries per-row album/artist/cover context and a local stream url', () => {
		expect.assertions(11);
		const [item] = buildDiscoveryQueueFromLocal([nativeTrack]);
		expect(item.trackSourceId).toBe('file-7');
		expect(item.trackName).toBe('Flat Song');
		expect(item.artistName).toBe('Flat Artist');
		expect(item.albumName).toBe('Cross Album');
		expect(item.albumId).toBe('local-album-9');
		expect(item.sourceType).toBe('local');
		expect(item.streamUrl).toBe('/api/v3/stream/local/file-7');
		expect(item.coverUrl).toBe('/cover/local-album-9');
		expect(item.format).toBe('flac');
		expect(item.discNumber).toBe(2);
		expect(item.duration).toBe(200);
	});

	it('keeps the local album ID and normalizes an invalid disc number', () => {
		expect.assertions(3);
		const [item] = buildDiscoveryQueueFromLocal([{ ...nativeTrack, disc_number: 0 }]);
		expect(item.albumId).toBe('local-album-9');
		expect(item.coverUrl).toBe('/cover/local-album-9');
		expect(item.discNumber).toBe(1);
	});
});

describe('playlistTrackToQueueItem', () => {
	const basePlaylistTrack: PlaylistTrack = {
		id: 'pt-1',
		position: 0,
		track_name: 'Test Track',
		artist_name: 'Test Artist',
		album_name: 'Test Album',
		album_id: 'album-1',
		artist_id: 'artist-1',
		track_source_id: '42',
		cover_url: '/cover.jpg',
		source_type: 'local',
		available_sources: ['local', 'jellyfin'],
		format: 'flac',
		track_number: 1,
		disc_number: 2,
		duration: 240,
		created_at: '2026-01-01T00:00:00Z',
		plex_rating_key: null,
		library_file_id: null
	};

	it('maps local track to QueueItem with correct streamUrl', () => {
		expect.assertions(4);
		const item = playlistTrackToQueueItem(basePlaylistTrack)!;
		expect(item).not.toBeNull();
		expect(item.sourceType).toBe('local');
		expect(item.streamUrl).toBe('/api/v3/stream/local/42');
		expect(item.trackName).toBe('Test Track');
	});

	it('maps jellyfin track to QueueItem with correct streamUrl', () => {
		expect.assertions(3);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'jellyfin',
			track_source_id: 'jf-123',
			format: 'opus'
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item.sourceType).toBe('jellyfin');
		expect(item.streamUrl).toBe('/api/v3/stream/jellyfin/jf-123');
		expect(item.format).toBe('opus');
	});

	it('returns null for tracks with null track_source_id', () => {
		expect.assertions(1);
		const track: PlaylistTrack = { ...basePlaylistTrack, track_source_id: null };
		expect(playlistTrackToQueueItem(track)).toBeNull();
	});

	it('prefers local when library_file_id set and available_sources includes local', () => {
		expect.assertions(4);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'jellyfin',
			track_source_id: 'jf-123',
			available_sources: ['jellyfin', 'local'],
			library_file_id: '77'
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item.sourceType).toBe('local');
		expect(item.trackSourceId).toBe('77');
		expect(item.streamUrl).toBe('/api/v3/stream/local/77');
		expect(item.sourceIds).toEqual({ jellyfin: 'jf-123', local: '77' });
	});

	it('plays linked row with empty track_source_id via library_file_id local fallback', () => {
		expect.assertions(5);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'spotify',
			track_source_id: null,
			available_sources: ['local'],
			library_file_id: '42'
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item).not.toBeNull();
		expect(item.sourceType).toBe('local');
		expect(item.trackSourceId).toBe('42');
		expect(item.streamUrl).toBe('/api/v3/stream/local/42');
		expect(item.sourceIds).toEqual({ local: '42' });
	});

	it('returns null for empty row with no track_source_id and no local link', () => {
		expect.assertions(1);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			track_source_id: null,
			available_sources: [],
			library_file_id: null
		};
		expect(playlistTrackToQueueItem(track)).toBeNull();
	});
});
