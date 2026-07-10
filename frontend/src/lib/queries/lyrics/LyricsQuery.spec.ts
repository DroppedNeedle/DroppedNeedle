import { describe, expect, it, vi, beforeEach } from 'vitest';
import type { NowPlaying } from '$lib/player/types';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn()
}));

vi.mock('$lib/api/client', async () => {
	class ApiErrorMock extends Error {
		readonly status: number;
		readonly code: string;
		readonly details: unknown;
		constructor(status: number, message: string, code = '', details: unknown = null) {
			super(message);
			this.name = 'ApiError';
			this.status = status;
			this.code = code;
			this.details = details;
		}
	}
	return {
		ApiError: ApiErrorMock,
		api: {
			global: {
				get: vi.fn()
			}
		}
	};
});

import { api, ApiError } from '$lib/api/client';
import { fetchLyrics } from '$lib/queries/lyrics/LyricsQuery.svelte';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';

const mockGet = vi.mocked(api.global.get);

function makeNowPlaying(overrides: Partial<NowPlaying> = {}): NowPlaying {
	return {
		albumId: 'album-1',
		albumName: 'Test Album',
		artistName: 'Test Artist',
		coverUrl: null,
		sourceType: 'navidrome',
		trackSourceId: 'track-1',
		trackName: 'Test Track',
		...overrides
	};
}

const signal = new AbortController().signal;

describe('fetchLyrics', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('returns null for unsupported source types', async () => {
		const np = makeNowPlaying({ sourceType: 'youtube' });
		const result = await fetchLyrics(np, signal);
		expect(result).toBeNull();
		expect(mockGet).not.toHaveBeenCalled();
	});

	it('fetches plex lyrics via the metadata LRCLIB lookup', async () => {
		mockGet.mockResolvedValueOnce({ text: 'Plex lyrics', is_synced: false, lines: [] });

		const np = makeNowPlaying({ sourceType: 'plex' });
		const result = await fetchLyrics(np, signal);

		expect(result).toEqual({ text: 'Plex lyrics', is_synced: false, lines: [] });
		expect(mockGet).toHaveBeenCalledOnce();
		expect(mockGet.mock.calls[0][0]).toContain('/api/v1/lyrics/lookup');
	});

	it('returns null for plex tracks without a track name (no lookup possible)', async () => {
		const np = makeNowPlaying({ sourceType: 'plex', trackName: '' });
		const result = await fetchLyrics(np, signal);
		expect(result).toBeNull();
		expect(mockGet).not.toHaveBeenCalled();
	});

	it('fetches local lyrics from the library track endpoint', async () => {
		mockGet.mockResolvedValueOnce({ text: 'Local lyrics', is_synced: true, lines: [] });

		const np = makeNowPlaying({ sourceType: 'local' });
		const result = await fetchLyrics(np, signal);

		expect(result).toEqual({ text: 'Local lyrics', is_synced: true, lines: [] });
		expect(mockGet).toHaveBeenCalledOnce();
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/library/tracks/track-1/lyrics');
	});

	it('normalizes Navidrome response correctly', async () => {
		mockGet.mockResolvedValueOnce({
			text: 'Hello world',
			is_synced: true,
			lines: [
				{ text: 'Hello', start_seconds: 0 },
				{ text: 'world', start_seconds: 5 }
			]
		});

		const np = makeNowPlaying({ sourceType: 'navidrome' });
		const result = await fetchLyrics(np, signal);

		expect(result).toEqual({
			text: 'Hello world',
			is_synced: true,
			lines: [
				{ text: 'Hello', start_seconds: 0 },
				{ text: 'world', start_seconds: 5 }
			]
		});
		expect(mockGet).toHaveBeenCalledOnce();
		expect(mockGet.mock.calls[0][0]).toContain('/api/v1/navidrome/lyrics/track-1');
	});

	it('normalizes Jellyfin response correctly (lyrics_text → text)', async () => {
		mockGet.mockResolvedValueOnce({
			lyrics_text: 'Jellyfin lyrics here',
			is_synced: false,
			lines: [{ text: 'line one', start_seconds: null }]
		});

		const np = makeNowPlaying({ sourceType: 'jellyfin' });
		const result = await fetchLyrics(np, signal);

		expect(result).toEqual({
			text: 'Jellyfin lyrics here',
			is_synced: false,
			lines: [{ text: 'line one', start_seconds: null }]
		});
		expect(mockGet).toHaveBeenCalledOnce();
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/jellyfin/lyrics/track-1');
	});

	it('returns null on 404 (lyrics not available)', async () => {
		mockGet.mockRejectedValueOnce(new ApiError(404, 'Not found'));

		const np = makeNowPlaying({ sourceType: 'navidrome' });
		const result = await fetchLyrics(np, signal);

		expect(result).toBeNull();
	});

	it('re-throws non-404 errors', async () => {
		mockGet.mockRejectedValueOnce(new ApiError(500, 'Server error'));

		const np = makeNowPlaying({ sourceType: 'navidrome' });
		await expect(fetchLyrics(np, signal)).rejects.toThrow('Server error');
	});

	it('re-throws network errors', async () => {
		mockGet.mockRejectedValueOnce(new TypeError('Failed to fetch'));

		const np = makeNowPlaying({ sourceType: 'jellyfin' });
		await expect(fetchLyrics(np, signal)).rejects.toThrow('Failed to fetch');
	});

	it('handles missing fields in Navidrome response gracefully', async () => {
		mockGet.mockResolvedValueOnce({});

		const np = makeNowPlaying({ sourceType: 'navidrome' });
		const result = await fetchLyrics(np, signal);

		expect(result).toEqual({
			text: '',
			is_synced: false,
			lines: []
		});
	});

	it('handles missing fields in Jellyfin response gracefully', async () => {
		mockGet.mockResolvedValueOnce({});

		const np = makeNowPlaying({ sourceType: 'jellyfin' });
		const result = await fetchLyrics(np, signal);

		expect(result).toEqual({
			text: '',
			is_synced: false,
			lines: []
		});
	});

	it('passes signal to api.global.get for Navidrome', async () => {
		mockGet.mockResolvedValueOnce({ text: '', is_synced: false, lines: [] });

		const np = makeNowPlaying({ sourceType: 'navidrome' });
		await fetchLyrics(np, signal);

		expect(mockGet).toHaveBeenCalledWith(expect.any(String), { signal });
	});

	it('passes signal to api.global.get for Jellyfin', async () => {
		mockGet.mockResolvedValueOnce({ lyrics_text: '', is_synced: false, lines: [] });

		const np = makeNowPlaying({ sourceType: 'jellyfin' });
		await fetchLyrics(np, signal);

		expect(mockGet).toHaveBeenCalledWith(expect.any(String), { signal });
	});

	it('includes artist and title in Navidrome URL', async () => {
		mockGet.mockResolvedValueOnce({ text: '', is_synced: false, lines: [] });

		const np = makeNowPlaying({
			sourceType: 'navidrome',
			trackSourceId: 'song-42',
			artistName: 'The Band',
			trackName: 'Cool Song'
		});
		await fetchLyrics(np, signal);

		const url = mockGet.mock.calls[0][0] as string;
		expect(url).toContain('/api/v1/navidrome/lyrics/song-42');
		expect(url).toContain('artist=The%20Band');
		expect(url).toContain('title=Cool%20Song');
	});
});

describe('LyricsQueryKeyFactory', () => {
	it('generates key with all parameters', () => {
		const key = LyricsQueryKeyFactory.lyrics('navidrome', 'track-1', 'Artist', 'Song');
		expect(key).toEqual(['lyrics', 'navidrome', 'track-1', 'Artist', 'Song']);
	});

	it('generates key with undefined parameters', () => {
		const key = LyricsQueryKeyFactory.lyrics(undefined, undefined, undefined, undefined);
		expect(key).toEqual(['lyrics', undefined, undefined, undefined, undefined]);
	});

	it('includes prefix', () => {
		expect(LyricsQueryKeyFactory.prefix).toEqual(['lyrics']);
	});

	it('generates different keys for different tracks', () => {
		const key1 = LyricsQueryKeyFactory.lyrics('navidrome', 'track-1', 'Artist', 'Song A');
		const key2 = LyricsQueryKeyFactory.lyrics('navidrome', 'track-2', 'Artist', 'Song B');
		expect(key1).not.toEqual(key2);
	});

	it('generates different keys for same track ID but different metadata', () => {
		const key1 = LyricsQueryKeyFactory.lyrics('navidrome', 'track-1', 'Artist A', 'Song');
		const key2 = LyricsQueryKeyFactory.lyrics('navidrome', 'track-1', 'Artist B', 'Song');
		expect(key1).not.toEqual(key2);
	});
});
