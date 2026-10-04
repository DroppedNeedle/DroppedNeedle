import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('$lib/constants', () => ({
	API: {
		playlists: {
			requestMissing: (id: string) => `/api/v1/playlists/${id}/request-missing`
		}
	}
}));

const mockGet = vi.fn();
const mockPost = vi.fn();
const mockPut = vi.fn();
const mockPatch = vi.fn();
const mockDelete = vi.fn();
const mockLegacyPost = vi.fn();

vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			v3: {
				GET: (...args: unknown[]) => mockGet(...args),
				POST: (...args: unknown[]) => mockPost(...args),
				PUT: (...args: unknown[]) => mockPut(...args),
				PATCH: (...args: unknown[]) => mockPatch(...args),
				DELETE: (...args: unknown[]) => mockDelete(...args)
			},
			post: (...args: unknown[]) => mockLegacyPost(...args)
		}
	},
	ApiError: class ApiError extends Error {
		status: number;
		code: string;
		details: unknown;
		constructor(status: number, message: string, code = '', details: unknown = null) {
			super(message);
			this.name = 'ApiError';
			this.status = status;
			this.code = code;
			this.details = details;
		}
	}
}));

import {
	fetchPlaylists,
	fetchPlaylist,
	createPlaylist,
	updatePlaylist,
	deletePlaylist,
	addTracksToPlaylist,
	removeTrackFromPlaylist,
	removeTracksFromPlaylist,
	updatePlaylistTrack,
	reorderPlaylistTrack,
	uploadPlaylistCover,
	deletePlaylistCover,
	checkTrackMembership,
	resolvePlaylistSources,
	setPlaylistPublic,
	requestMissingTracks,
	queueItemToTrackData
} from './playlists';
import type { QueueItem } from '$lib/player/types';

function makeTrackV3(overrides: Record<string, unknown> = {}) {
	return {
		id: 't1',
		position: 0,
		track_name: 'Song',
		artist_name: 'Art',
		album_name: 'Alb',
		source_type: 'local',
		created_at: 1767225600,
		...overrides
	};
}

function makeSummaryV3(overrides: Record<string, unknown> = {}) {
	return {
		id: 'p1',
		name: 'My Playlist',
		track_count: 1,
		cover_urls: [],
		created_at: 1767225600,
		updated_at: 1767312000,
		is_public: false,
		is_owner: true,
		is_redacted: false,
		...overrides
	};
}

function makeDetailV3(overrides: Record<string, unknown> = {}) {
	return { ...makeSummaryV3(), tracks: [makeTrackV3()], ...overrides };
}

beforeEach(() => {
	mockGet.mockReset();
	mockPost.mockReset();
	mockPut.mockReset();
	mockPatch.mockReset();
	mockDelete.mockReset();
	mockLegacyPost.mockReset();
});

describe('playlists API client', () => {
	describe('fetchPlaylists', () => {
		it('calls v3 list and adapts rows to page shapes', async () => {
			mockGet.mockResolvedValue({
				playlists: [makeSummaryV3({ source_ref: 'spotify:abc' })]
			});

			const result = await fetchPlaylists();

			expect(mockGet).toHaveBeenCalledWith('/api/v3/playlists');
			expect(result).toHaveLength(1);
			expect(result[0]).toMatchObject({
				id: 'p1',
				source_ref: 'spotify:abc',
				created_at: '2026-01-01T00:00:00.000Z'
			});
		});

		it('throws on API error', async () => {
			mockGet.mockRejectedValue(new Error('Server error'));
			await expect(fetchPlaylists()).rejects.toThrow('Server error');
		});
	});

	describe('fetchPlaylist', () => {
		it('calls v3 detail and adapts dates', async () => {
			mockGet.mockResolvedValue(makeDetailV3());

			const result = await fetchPlaylist('p1');

			expect(mockGet).toHaveBeenCalledWith('/api/v3/playlists/p1', { signal: undefined });
			expect(result).toMatchObject({ id: 'p1', created_at: '2026-01-01T00:00:00.000Z' });
		});

		it('forwards AbortSignal when provided', async () => {
			const controller = new AbortController();
			mockGet.mockResolvedValue(makeDetailV3());

			await fetchPlaylist('p1', { signal: controller.signal });

			expect(mockGet).toHaveBeenCalledWith('/api/v3/playlists/p1', {
				signal: controller.signal
			});
		});
	});

	describe('setPlaylistPublic', () => {
		it('patches visibility and adapts the summary', async () => {
			mockPatch.mockResolvedValue(makeSummaryV3({ is_public: true }));

			const result = await setPlaylistPublic('p1', true);

			expect(mockPatch).toHaveBeenCalledWith('/api/v3/playlists/p1/visibility', {
				is_public: true
			});
			expect(result.is_public).toBe(true);
		});
	});

	describe('createPlaylist', () => {
		it('sends POST with { name } body', async () => {
			mockPost.mockResolvedValue(makeDetailV3({ name: 'New' }));

			const result = await createPlaylist('New');

			expect(mockPost).toHaveBeenCalledWith('/api/v3/playlists', { name: 'New' });
			expect(result.name).toBe('New');
		});
	});

	describe('updatePlaylist', () => {
		it('sends PUT with data body', async () => {
			mockPut.mockResolvedValue(makeDetailV3({ name: 'Renamed' }));

			await updatePlaylist('p1', { name: 'Renamed' });

			expect(mockPut).toHaveBeenCalledWith('/api/v3/playlists/p1', { name: 'Renamed' });
		});
	});

	describe('deletePlaylist', () => {
		it('calls v3 DELETE on correct URL', async () => {
			mockDelete.mockResolvedValue(undefined);
			await deletePlaylist('p1');
			expect(mockDelete).toHaveBeenCalledWith('/api/v3/playlists/p1');
		});

		it('throws on error', async () => {
			mockDelete.mockRejectedValue(new Error('Not found'));
			await expect(deletePlaylist('p1')).rejects.toThrow('Not found');
		});
	});

	describe('addTracksToPlaylist', () => {
		it('sends POST with { tracks, position } and adapts .tracks', async () => {
			const tracks = [
				{ track_name: 'Song', artist_name: 'Art', album_name: 'Alb', source_type: 'local' }
			];
			mockPost.mockResolvedValue({ tracks: [makeTrackV3()] });

			const result = await addTracksToPlaylist('p1', tracks, 5);

			const call = mockPost.mock.calls[0];
			expect(call[0]).toBe('/api/v3/playlists/p1/tracks');
			expect(call[1].tracks).toMatchObject(tracks);
			expect(call[1].position).toBe(5);
			expect(result[0]).toMatchObject({
				track_name: 'Song',
				created_at: '2026-01-01T00:00:00.000Z'
			});
		});

		it('omits position when not provided', async () => {
			const tracks = [
				{ track_name: 'Song', artist_name: 'Art', album_name: 'Alb', source_type: 'local' }
			];
			mockPost.mockResolvedValue({ tracks: [] });

			await addTracksToPlaylist('p1', tracks);

			const body = mockPost.mock.calls[0][1];
			expect(body).not.toHaveProperty('position');
		});
	});

	describe('removeTrackFromPlaylist', () => {
		it('calls v3 DELETE on correct URL', async () => {
			mockDelete.mockResolvedValue(undefined);
			await removeTrackFromPlaylist('p1', 't1');
			expect(mockDelete).toHaveBeenCalledWith('/api/v3/playlists/p1/tracks/t1');
		});
	});

	describe('removeTracksFromPlaylist', () => {
		it('posts track ids to the bulk-remove endpoint', async () => {
			mockPost.mockResolvedValue({ status: 'ok', message: 'Removed', removed: 2 });
			await removeTracksFromPlaylist('p1', ['t1', 't2']);
			expect(mockPost).toHaveBeenCalledWith('/api/v3/playlists/p1/tracks/remove', {
				track_ids: ['t1', 't2']
			});
		});
	});

	describe('updatePlaylistTrack', () => {
		it('sends PATCH with data body', async () => {
			mockPatch.mockResolvedValue(makeTrackV3({ source_type: 'local' }));

			await updatePlaylistTrack('p1', 't1', { source_type: 'local' });

			expect(mockPatch).toHaveBeenCalledWith('/api/v3/playlists/p1/tracks/t1', {
				source_type: 'local'
			});
		});
	});

	describe('reorderPlaylistTrack', () => {
		it('sends PATCH with { track_id, new_position }', async () => {
			mockPatch.mockResolvedValue({
				status: 'ok',
				message: 'Track reordered',
				actual_position: 3
			});

			const result = await reorderPlaylistTrack('p1', 't1', 3);

			expect(mockPatch).toHaveBeenCalledWith('/api/v3/playlists/p1/tracks/reorder', {
				track_id: 't1',
				new_position: 3
			});
			expect(result.actual_position).toBe(3);
		});
	});

	describe('uploadPlaylistCover', () => {
		it('posts base64 bytes to the v3 cover endpoint', async () => {
			mockPost.mockResolvedValue({ cover_url: '/api/v3/playlists/p1/cover' });
			const file = new File(['img'], 'cover.jpg', { type: 'image/jpeg' });

			const result = await uploadPlaylistCover('p1', file);

			const call = mockPost.mock.calls[0];
			expect(call[0]).toBe('/api/v3/playlists/p1/cover');
			expect(call[1].content_type).toBe('image/jpeg');
			expect(typeof call[1].image_base64).toBe('string');
			expect(call[1].image_base64.length).toBeGreaterThan(0);
			expect(result.cover_url).toBe('/api/v3/playlists/p1/cover');
		});
	});

	describe('deletePlaylistCover', () => {
		it('calls v3 DELETE on correct URL', async () => {
			mockDelete.mockResolvedValue(undefined);
			await deletePlaylistCover('p1');
			expect(mockDelete).toHaveBeenCalledWith('/api/v3/playlists/p1/cover');
		});
	});

	describe('checkTrackMembership', () => {
		it('transposes the v3 index-first map to playlist-first', async () => {
			mockPost.mockResolvedValue({ membership: { '0': ['p1', 'p2'], '1': ['p1'] } });

			const result = await checkTrackMembership([
				{ track_name: 'A', artist_name: 'Art', album_name: 'Alb' },
				{ track_name: 'B', artist_name: 'Art', album_name: 'Alb' }
			]);

			expect(mockPost).toHaveBeenCalledWith(
				'/api/v3/playlists/check-tracks',
				expect.objectContaining({ tracks: expect.any(Array) })
			);
			expect(result).toEqual({ p1: [0, 1], p2: [0] });
		});
	});

	describe('resolvePlaylistSources', () => {
		it('returns the v3 source map', async () => {
			mockPost.mockResolvedValue({ sources: { t1: ['local'] } });

			const result = await resolvePlaylistSources('p1');

			expect(mockPost).toHaveBeenCalledWith('/api/v3/playlists/p1/resolve-sources');
			expect(result).toEqual({ t1: ['local'] });
		});
	});

	describe('requestMissingTracks', () => {
		function pageTrack(overrides: Record<string, unknown> = {}) {
			return {
				id: 't1',
				position: 0,
				track_name: 'Song',
				artist_name: 'Art',
				album_name: 'Alb',
				album_id: 'rg-1',
				artist_id: null,
				track_source_id: null,
				cover_url: null,
				source_type: '',
				available_sources: null,
				format: null,
				track_number: null,
				disc_number: null,
				duration: null,
				created_at: '2026-01-01T00:00:00Z',
				plex_rating_key: null,
				library_file_id: null,
				...overrides
			};
		}

		it('dedupes by album and posts unresolved albums to the v3 batch intake', async () => {
			mockPost.mockResolvedValue({
				success: true,
				message: 'Queued',
				requested: 1,
				skipped: 0,
				overflow: 0,
				status: 'pending'
			});

			const result = await requestMissingTracks([
				pageTrack({ id: 't1', album_id: 'rg-1' }),
				pageTrack({ id: 't2', album_id: 'rg-1' }),
				pageTrack({ id: 't3', album_id: null }),
				pageTrack({ id: 't4', album_id: 'rg-2', library_file_id: 'f1' }),
				pageTrack({ id: 't5', album_id: 'rg-3', available_sources: ['local'] })
			]);

			expect(mockPost).toHaveBeenCalledWith('/api/v3/requests/batches', {
				items: [{ musicbrainz_id: 'rg-1', artist_name: 'Art', album_title: 'Alb' }]
			});
			expect(result.requested).toBe(1);
		});

		it('short-circuits when every track already has a source', async () => {
			mockPost.mockClear();

			const result = await requestMissingTracks([
				pageTrack({ album_id: 'rg-1', library_file_id: 'f1' })
			]);

			expect(mockPost).not.toHaveBeenCalled();
			expect(result.success).toBe(true);
			expect(result.requested).toBe(0);
		});
	});

	describe('queueItemToTrackData', () => {
		it('maps all fields correctly', () => {
			const item: QueueItem = {
				trackSourceId: 'vid-1',
				trackName: 'My Track',
				artistName: 'My Artist',
				trackNumber: 3,
				albumId: 'alb-1',
				albumName: 'My Album',
				coverUrl: '/cover.jpg',
				sourceType: 'jellyfin',
				artistId: 'art-1',
				streamUrl: '/stream/vid-1',
				format: 'aac',
				availableSources: ['jellyfin', 'local'],
				duration: 240
			};

			const result = queueItemToTrackData(item);

			expect(result).toEqual({
				track_name: 'My Track',
				artist_name: 'My Artist',
				album_name: 'My Album',
				album_id: 'alb-1',
				artist_id: 'art-1',
				track_source_id: 'vid-1',
				cover_url: '/cover.jpg',
				source_type: 'jellyfin',
				available_sources: ['jellyfin', 'local'],
				format: 'aac',
				track_number: 3,
				disc_number: null,
				duration: 240,
				plex_rating_key: null
			});
		});

		it('handles optional/null fields correctly', () => {
			const item: QueueItem = {
				trackSourceId: '',
				trackName: 'Track',
				artistName: 'Artist',
				trackNumber: 1,
				albumId: '',
				albumName: 'Album',
				coverUrl: null,
				sourceType: 'local'
			};

			const result = queueItemToTrackData(item);

			expect(result.album_id).toBeNull();
			expect(result.artist_id).toBeNull();
			expect(result.track_source_id).toBeNull();
			expect(result.cover_url).toBeNull();
			expect(result.available_sources).toBeNull();
			expect(result.format).toBeNull();
			expect(result.track_number).toBe(1);
			expect(result.duration).toBeNull();
		});

		it('excludes streamUrl from output', () => {
			const item: QueueItem = {
				trackSourceId: 'vid-1',
				trackName: 'Track',
				artistName: 'Artist',
				trackNumber: 1,
				albumId: 'alb-1',
				albumName: 'Album',
				coverUrl: null,
				sourceType: 'local',
				streamUrl: '/stream/should-not-appear'
			};

			const result = queueItemToTrackData(item);

			expect(result).not.toHaveProperty('streamUrl');
			expect(result).not.toHaveProperty('stream_url');
		});
	});
});
