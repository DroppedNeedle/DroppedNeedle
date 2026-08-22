import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { QueueItem, SourceType } from '$lib/player/types';

async function loadResolver(base: string) {
	vi.doMock('$app/paths', () => ({ base }));
	vi.doMock('$env/dynamic/public', () => ({ env: {} }));
	return await import('./playerSourceResolver');
}

function queueItem(sourceType: SourceType, trackSourceId: string, streamUrl?: string): QueueItem {
	return {
		trackSourceId,
		trackName: 'Track',
		artistName: 'Artist',
		trackNumber: 1,
		albumId: 'album-1',
		albumName: 'Album',
		coverUrl: null,
		sourceType,
		streamUrl
	};
}

describe('resolveSourceUrl', () => {
	beforeEach(() => {
		vi.resetModules();
	});

	it('returns root-absolute stream URLs unchanged when no base path is set', async () => {
		const { resolveSourceUrl } = await loadResolver('');
		expect(resolveSourceUrl(queueItem('local', '77'))).toBe('/api/v1/stream/local/77');
		expect(resolveSourceUrl(queueItem('jellyfin', 'jf-1'))).toBe('/api/v1/stream/jellyfin/jf-1');
	});

	it('prefixes stream URLs with the base path so the audio element resolves them', async () => {
		const { resolveSourceUrl } = await loadResolver('/droppedneedle');
		expect(resolveSourceUrl(queueItem('local', '77'))).toBe(
			'/droppedneedle/api/v1/stream/local/77'
		);
		expect(resolveSourceUrl(queueItem('navidrome', 'nd-1'))).toBe(
			'/droppedneedle/api/v1/stream/navidrome/nd-1'
		);
	});

	it('prefixes a stream URL persisted before the base path was configured', async () => {
		const { resolveSourceUrl } = await loadResolver('/droppedneedle');
		expect(resolveSourceUrl(queueItem('local', '77', '/api/v1/stream/local/77'))).toBe(
			'/droppedneedle/api/v1/stream/local/77'
		);
	});

	it('leaves off-origin stream URLs untouched', async () => {
		const { resolveSourceUrl } = await loadResolver('/droppedneedle');
		expect(resolveSourceUrl(queueItem('youtube', 'yt-1', 'https://youtu.be/yt-1'))).toBe(
			'https://youtu.be/yt-1'
		);
		expect(resolveSourceUrl(queueItem('plex', 'px-1', 'https://plex.example/part/1'))).toBe(
			'https://plex.example/part/1'
		);
	});
});

describe('buildPrefetchUrl', () => {
	beforeEach(() => {
		vi.resetModules();
	});

	it('stays app-relative because the API client applies the base path itself', async () => {
		const { buildPrefetchUrl } = await loadResolver('/droppedneedle');
		expect(buildPrefetchUrl(queueItem('local', '77'))).toBe('/api/v1/stream/local/77');
	});
});
