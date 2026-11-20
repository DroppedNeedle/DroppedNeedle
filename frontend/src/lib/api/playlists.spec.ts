import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('$lib/api/client', () => ({
	api: { v3: { POST: vi.fn() } }
}));

import { api } from '$lib/api/client';
import { requestMissingTracks, type PlaylistTrack } from './playlists';

const mockPost = vi.mocked(api.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

function track(overrides: Partial<PlaylistTrack> = {}): PlaylistTrack {
	return {
		id: 'trk-1',
		position: 0,
		track_name: 'Opener',
		artist_name: 'Aurora',
		album_name: 'First Light',
		album_id: null,
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
		recording_mbid: null,
		...overrides
	};
}

beforeEach(() => {
	vi.clearAllMocks();
});

describe('requestMissingTracks', () => {
	it('requests the albums it can and counts the songs it left out', async () => {
		mockPost.mockResolvedValue({
			success: true,
			message: 'Batch request accepted: 1 started',
			requested: 1,
			skipped: 0,
			overflow: 0,
			status: 'pending'
		});
		const result = await requestMissingTracks([
			track({ album_id: 'rg-first-light' }),
			track({ id: 'trk-2', track_name: 'Second Song' }),
			track({ id: 'trk-3', track_name: 'Nightfall', library_file_id: 'lib-1' })
		]);

		expect(result.message).toBe(
			'Batch request accepted: 1 started. 1 song has no album to request yet.'
		);
		const [, body] = mockPost.mock.calls[0] ?? [];
		expect((body as { items: { musicbrainz_id: string }[] }).items).toEqual([
			expect.objectContaining({ musicbrainz_id: 'rg-first-light' })
		]);
	});

	it('counts several songs left out after a request that waits for approval', async () => {
		mockPost.mockResolvedValue({
			success: true,
			message: 'Batch request submitted, awaiting admin approval',
			requested: 1,
			skipped: 0,
			overflow: 0,
			status: 'awaiting_approval'
		});
		const result = await requestMissingTracks([
			track({ album_id: 'rg-first-light' }),
			track({ id: 'trk-2', track_name: 'Second Song' }),
			track({ id: 'trk-3', track_name: 'Nightfall' })
		]);

		expect(result.message).toBe(
			'Batch request submitted, awaiting admin approval. 2 songs have no album to request yet.'
		);
	});
});
