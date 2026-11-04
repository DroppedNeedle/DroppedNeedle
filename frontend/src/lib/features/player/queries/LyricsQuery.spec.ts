import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';
import type { NowPlaying } from '$lib/features/player/playback/types';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn()
}));

vi.mock('$lib/api/client', () => ({ api: { v3: { GET: vi.fn() } } }));

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { fetchLyrics } from '$lib/features/player/queries/lyrics.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock;

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

	it('answers null when no lyrics exist and throws on any other failure', async () => {
		mockGet.mockRejectedValueOnce(new ApiError(404, 'Not found'));
		expect(await fetchLyrics(makeNowPlaying({ sourceType: 'navidrome' }), signal)).toBeNull();

		mockGet.mockRejectedValueOnce(new ApiError(500, 'Server error'));
		await expect(fetchLyrics(makeNowPlaying({ sourceType: 'navidrome' }), signal)).rejects.toThrow(
			'Server error'
		);
	});
});
