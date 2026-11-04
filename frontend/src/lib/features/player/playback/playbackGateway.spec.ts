import { describe, expect, it } from 'vitest';

import { gatewayStreamUrl } from './playbackGateway';

describe('gatewayStreamUrl', () => {
	it('links every source behind one gateway shape, plex part keys raw, hints as query keys', () => {
		const cases: [Parameters<typeof gatewayStreamUrl>, string][] = [
			[['local', 'file-1'], '/api/v3/stream/local/file-1'],
			[['jellyfin', 'item-2'], '/api/v3/stream/jellyfin/item-2'],
			[['navidrome', 'song-3'], '/api/v3/stream/navidrome/song-3'],
			// Plex part-key slashes stay raw for the wildcard route.
			[['plex', 'library/parts/7/file.mp3'], '/api/v3/stream/plex/library/parts/7/file.mp3'],
			[
				['plex', 'part-1', { format: 'mp3', max_bitrate: 320 }],
				'/api/v3/stream/plex/part-1?format=mp3&max_bitrate=320'
			]
		];
		for (const [args, expected] of cases) {
			expect(gatewayStreamUrl(...args)).toBe(expected);
		}
	});
});
