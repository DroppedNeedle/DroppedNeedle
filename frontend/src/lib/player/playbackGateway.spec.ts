import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn() } } }
}));

import { api } from '$lib/api/client';
import {
	GATEWAY_ENDPOINTS,
	fetchNowPlayingSnapshot,
	gatewayStreamUrl,
	reportPlaybackProgress,
	sendScrobbleNowPlaying,
	startPlaybackSession,
	stopPlaybackSession,
	submitScrobble
} from './playbackGateway';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

beforeEach(() => {
	vi.clearAllMocks();
});

describe('gatewayStreamUrl', () => {
	it('deep-links every source behind one gateway shape', () => {
		expect(gatewayStreamUrl('local', 'file-1')).toBe('/api/v3/stream/local/file-1');
		expect(gatewayStreamUrl('jellyfin', 'item-2')).toBe('/api/v3/stream/jellyfin/item-2');
		expect(gatewayStreamUrl('navidrome', 'song-3')).toBe('/api/v3/stream/navidrome/song-3');
	});

	it('keeps plex part-key slashes raw for the wildcard route', () => {
		expect(gatewayStreamUrl('plex', 'library/parts/7/file.mp3')).toBe(
			'/api/v3/stream/plex/library/parts/7/file.mp3'
		);
	});

	it('appends transcode hints as query keys', () => {
		expect(gatewayStreamUrl('plex', 'part-1', { format: 'mp3', max_bitrate: 320 })).toBe(
			'/api/v3/stream/plex/part-1?format=mp3&max_bitrate=320'
		);
	});
});

describe('playback reporting', () => {
	it('starts a session on the gateway start route', async () => {
		mockPost.mockResolvedValue({ accepted: true, session: 'u:web:track-1' });
		await startPlaybackSession({ source: 'plex', track_id: 'track-1' });

		expect(mockPost).toHaveBeenCalledWith(GATEWAY_ENDPOINTS.playbackStart(), {
			source: 'plex',
			track_id: 'track-1'
		});
	});

	it('reports progress on the gateway progress route', async () => {
		mockPost.mockResolvedValue({ accepted: true });
		await reportPlaybackProgress({ track_id: 'track-1', position_ms: 12000 });

		expect(mockPost).toHaveBeenCalledWith(GATEWAY_ENDPOINTS.playbackProgress(), {
			track_id: 'track-1',
			position_ms: 12000
		});
	});

	it('stops a session on the gateway stop route', async () => {
		mockPost.mockResolvedValue({ accepted: true, scrobbled: true });
		await stopPlaybackSession({ track_id: 'track-1', position_ms: 12000 });

		expect(mockPost).toHaveBeenCalledWith(GATEWAY_ENDPOINTS.playbackStop(), {
			track_id: 'track-1',
			position_ms: 12000
		});
	});
});

describe('scrobble reporting', () => {
	it('sends now-playing to the scrobble route', async () => {
		mockPost.mockResolvedValue({ accepted: true });
		await sendScrobbleNowPlaying({
			track_name: 'T',
			artist_name: 'A',
			album_name: 'B',
			duration_ms: 1000
		});

		expect(mockPost).toHaveBeenCalledWith(GATEWAY_ENDPOINTS.scrobbleNowPlaying(), {
			track_name: 'T',
			artist_name: 'A',
			album_name: 'B',
			duration_ms: 1000
		});
	});

	it('submits plays to the scrobble route', async () => {
		mockPost.mockResolvedValue({ accepted: true, services: {} });
		await submitScrobble({
			track_name: 'T',
			artist_name: 'A',
			album_name: 'B',
			timestamp: 1,
			duration_ms: 1000
		});

		expect(mockPost).toHaveBeenCalledWith(GATEWAY_ENDPOINTS.scrobbleSubmit(), {
			track_name: 'T',
			artist_name: 'A',
			album_name: 'B',
			timestamp: 1,
			duration_ms: 1000
		});
	});
});

describe('fetchNowPlayingSnapshot', () => {
	it('reads the gateway now-playing snapshot with the abort signal', async () => {
		mockGet.mockResolvedValue({ sessions: [] });
		const controller = new AbortController();
		await fetchNowPlayingSnapshot(controller.signal);

		expect(mockGet).toHaveBeenCalledWith(GATEWAY_ENDPOINTS.nowPlaying(), {
			signal: controller.signal
		});
	});
});
