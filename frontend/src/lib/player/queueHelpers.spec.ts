import { describe, it, expect, vi } from 'vitest';

vi.mock('$lib/constants', () => ({
	API: {
		stream: {
			local: (id: number | string) => `/api/v1/stream/local/${id}`,
			jellyfin: (id: string) => `/api/v1/stream/jellyfin/${id}`
		}
	}
}));

vi.mock('$lib/utils/errorHandling', () => ({
	getCoverUrl: (url: string | null, albumId: string) => url ?? `/cover/${albumId}`
}));

import type { JellyfinTrackInfo, LocalTrackInfo, NativeTrackListItem } from '$lib/types';
import type { PlaylistTrack } from '$lib/api/playlists';
import type { TrackMeta, TrackSourceData } from './queueHelpers';
import {
	selectBestSource,
	getAvailableSources,
	buildQueueItem,
	buildQueueItemsFromJellyfin,
	buildQueueItemsFromLocal,
	buildDiscoveryQueueFromLocal,
	buildQueueItemFromYouTube,
	compareDiscTrack,
	getDiscTrackKey,
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
	it('returns local source when localTrack is available', () => {
		expect.assertions(3);
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'Track',
			localTrack,
			jellyfinTrack
		};
		const result = selectBestSource(data);
		expect(result).not.toBeNull();
		expect(result!.sourceType).toBe('local');
		expect(result!.streamUrl).toBe('/api/v1/stream/local/42');
	});

	it('returns jellyfin source when only jellyfinTrack is available', () => {
		expect.assertions(3);
		const data: TrackSourceData = {
			trackPosition: 2,
			trackTitle: 'Track',
			jellyfinTrack
		};
		const result = selectBestSource(data);
		expect(result).not.toBeNull();
		expect(result!.sourceType).toBe('jellyfin');
		expect(result!.trackSourceId).toBe('jf-123');
	});

	it('returns null when no source is available', () => {
		expect.assertions(1);
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'Track'
		};
		expect(selectBestSource(data)).toBeNull();
	});

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

describe('getAvailableSources', () => {
	it('returns both sources when both are available', () => {
		expect.assertions(2);
		const sources = getAvailableSources({
			trackPosition: 1,
			trackTitle: 'Track',
			localTrack,
			jellyfinTrack
		});
		expect(sources).toContain('local');
		expect(sources).toContain('jellyfin');
	});

	it('returns only local when only local is available', () => {
		expect.assertions(1);
		const sources = getAvailableSources({
			trackPosition: 1,
			trackTitle: 'Track',
			localTrack
		});
		expect(sources).toEqual(['local']);
	});

	it('returns only jellyfin when only jellyfin is available', () => {
		expect.assertions(1);
		const sources = getAvailableSources({
			trackPosition: 1,
			trackTitle: 'Track',
			jellyfinTrack
		});
		expect(sources).toEqual(['jellyfin']);
	});

	it('returns empty array when no sources are available', () => {
		expect.assertions(1);
		const sources = getAvailableSources({
			trackPosition: 1,
			trackTitle: 'Track'
		});
		expect(sources).toEqual([]);
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

	it('returns null when no source is available', () => {
		expect.assertions(1);
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'No Source'
		};
		expect(buildQueueItem(baseMeta, data)).toBeNull();
	});

	it('populates availableSources with both when both exist', () => {
		expect.assertions(2);
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'Dual Source',
			localTrack,
			jellyfinTrack
		};
		const item = buildQueueItem(baseMeta, data);
		expect(item!.availableSources).toContain('local');
		expect(item!.availableSources).toContain('jellyfin');
	});

	it('uses getCoverUrl to normalize cover URL', () => {
		expect.assertions(1);
		const meta: TrackMeta = { ...baseMeta, coverUrl: null };
		const data: TrackSourceData = {
			trackPosition: 1,
			trackTitle: 'Track',
			localTrack
		};
		const item = buildQueueItem(meta, data);
		expect(item!.coverUrl).toBe('/cover/album-1');
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
	it('maps JellyfinTrackInfo array to QueueItem array', () => {
		expect.assertions(5);
		const tracks: JellyfinTrackInfo[] = [jellyfinTrack];
		const items = buildQueueItemsFromJellyfin(tracks, baseMeta);
		expect(items).toHaveLength(1);
		expect(items[0].sourceType).toBe('jellyfin');
		expect(items[0].trackName).toBe('JF Song');
		expect(items[0].availableSources).toEqual(['jellyfin']);
		expect(items[0].duration).toBe(180);
	});

	it('normalizes codec for stream URL', () => {
		expect.assertions(1);
		const track: JellyfinTrackInfo = { ...jellyfinTrack, codec: 'ALAC' };
		const items = buildQueueItemsFromJellyfin([track], baseMeta);
		expect(items[0].streamUrl).toBe('/api/v1/stream/jellyfin/jf-123');
	});

	it('defaults to aac for unknown codecs', () => {
		expect.assertions(1);
		const track: JellyfinTrackInfo = { ...jellyfinTrack, codec: 'unknown_codec' };
		const items = buildQueueItemsFromJellyfin([track], baseMeta);
		expect(items[0].streamUrl).toBe('/api/v1/stream/jellyfin/jf-123');
	});

	it('defaults to aac for null codec', () => {
		expect.assertions(1);
		const track: JellyfinTrackInfo = { ...jellyfinTrack, codec: null };
		const items = buildQueueItemsFromJellyfin([track], baseMeta);
		expect(items[0].streamUrl).toBe('/api/v1/stream/jellyfin/jf-123');
	});
});

describe('buildQueueItemsFromLocal', () => {
	it('maps LocalTrackInfo array to QueueItem array', () => {
		expect.assertions(5);
		const items = buildQueueItemsFromLocal([localTrack], baseMeta);
		expect(items).toHaveLength(1);
		expect(items[0].sourceType).toBe('local');
		expect(items[0].trackName).toBe('Local Song');
		expect(items[0].availableSources).toEqual(['local']);
		expect(items[0].streamUrl).toBe('/api/v1/stream/local/42');
	});

	it('lowercases format', () => {
		expect.assertions(1);
		const items = buildQueueItemsFromLocal([localTrack], baseMeta);
		expect(items[0].format).toBe('flac');
	});

	it('handles undefined duration_seconds', () => {
		expect.assertions(1);
		const track: LocalTrackInfo = { ...localTrack, duration_seconds: undefined };
		const items = buildQueueItemsFromLocal([track], baseMeta);
		expect(items[0].duration).toBeUndefined();
	});

	it('handles null duration_seconds', () => {
		expect.assertions(1);
		const track: LocalTrackInfo = { ...localTrack, duration_seconds: null };
		const items = buildQueueItemsFromLocal([track], baseMeta);
		expect(items[0].duration).toBeUndefined();
	});
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
		expect(item.streamUrl).toBe('/api/v1/stream/local/file-7');
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
		expect(item.streamUrl).toBe('/api/v1/stream/local/42');
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
		expect(item.streamUrl).toBe('/api/v1/stream/jellyfin/jf-123');
		expect(item.format).toBe('opus');
	});

	it('maps youtube track with undefined streamUrl', () => {
		expect.assertions(2);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'youtube',
			track_source_id: 'yt-abc'
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item.sourceType).toBe('youtube');
		expect(item.streamUrl).toBeUndefined();
	});

	it('returns null for tracks with null track_source_id', () => {
		expect.assertions(1);
		const track: PlaylistTrack = { ...basePlaylistTrack, track_source_id: null };
		expect(playlistTrackToQueueItem(track)).toBeNull();
	});

	it('defaults available_sources to [sourceType] when null', () => {
		expect.assertions(1);
		const track: PlaylistTrack = { ...basePlaylistTrack, available_sources: null };
		const item = playlistTrackToQueueItem(track)!;
		expect(item.availableSources).toEqual(['local']);
	});

	it('maps all fields correctly', () => {
		expect.assertions(9);
		const item = playlistTrackToQueueItem(basePlaylistTrack)!;
		expect(item.trackSourceId).toBe('42');
		expect(item.artistName).toBe('Test Artist');
		expect(item.trackNumber).toBe(1);
		expect(item.discNumber).toBe(2);
		expect(item.albumId).toBe('album-1');
		expect(item.albumName).toBe('Test Album');
		expect(item.coverUrl).toBe('/cover.jpg');
		expect(item.artistId).toBe('artist-1');
		expect(item.availableSources).toEqual(['local', 'jellyfin']);
	});

	it('handles null album_id by defaulting to empty string', () => {
		expect.assertions(1);
		const track: PlaylistTrack = { ...basePlaylistTrack, album_id: null };
		const item = playlistTrackToQueueItem(track)!;
		expect(item.albumId).toBe('');
	});

	it('falls back to position when track_number is null', () => {
		expect.assertions(1);
		const track: PlaylistTrack = { ...basePlaylistTrack, track_number: null, position: 5 };
		const item = playlistTrackToQueueItem(track)!;
		expect(item.trackNumber).toBe(5);
	});

	it('uses aac as default format for jellyfin when format is null', () => {
		expect.assertions(1);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'jellyfin',
			track_source_id: 'jf-1',
			format: null
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item.streamUrl).toBe('/api/v1/stream/jellyfin/jf-1');
	});

	it('populates playlistTrackId from playlist track id', () => {
		expect.assertions(1);
		const item = playlistTrackToQueueItem(basePlaylistTrack)!;
		expect(item.playlistTrackId).toBe('pt-1');
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
		expect(item.streamUrl).toBe('/api/v1/stream/local/77');
		expect(item.sourceIds).toEqual({ jellyfin: 'jf-123', local: '77' });
	});

	it('keeps jellyfin when library_file_id is null', () => {
		expect.assertions(3);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'jellyfin',
			track_source_id: 'jf-123',
			available_sources: ['jellyfin', 'local'],
			library_file_id: null
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item.sourceType).toBe('jellyfin');
		expect(item.trackSourceId).toBe('jf-123');
		expect(item.sourceIds).toEqual({ jellyfin: 'jf-123' });
	});

	it('keeps jellyfin when local is not in available_sources', () => {
		expect.assertions(2);
		const track: PlaylistTrack = {
			...basePlaylistTrack,
			source_type: 'jellyfin',
			track_source_id: 'jf-123',
			available_sources: ['jellyfin'],
			library_file_id: '77'
		};
		const item = playlistTrackToQueueItem(track)!;
		expect(item.sourceType).toBe('jellyfin');
		expect(item.streamUrl).toBe('/api/v1/stream/jellyfin/jf-123');
	});
});
