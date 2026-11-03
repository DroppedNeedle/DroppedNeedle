import { describe, expect, it } from 'vitest';

import { gatewayStreamUrl } from './playbackGateway';

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
