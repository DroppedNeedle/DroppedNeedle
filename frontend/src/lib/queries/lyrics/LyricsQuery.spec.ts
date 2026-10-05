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
});

describe('LyricsQueryKeyFactory', () => {
	it('isolates users and Navidrome folder scopes', () => {
		const alice = LyricsQueryKeyFactory.lyrics(
			'alice',
			'selected-a',
			'navidrome',
			'track-1',
			'Artist',
			'Song'
		);
		const bob = LyricsQueryKeyFactory.lyrics(
			'bob',
			'selected-b',
			'navidrome',
			'track-1',
			'Artist',
			'Song'
		);
		expect(alice).not.toEqual(bob);
	});
});
