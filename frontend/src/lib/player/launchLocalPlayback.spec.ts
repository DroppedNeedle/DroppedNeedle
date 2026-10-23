import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ base: '', env: {} as Record<string, string>, playQueue: vi.fn() }));
vi.mock('$app/paths', () => ({
	get base() {
		return h.base;
	}
}));
vi.mock('$env/dynamic/public', () => ({ env: h.env }));
vi.mock('$lib/stores/player.svelte', () => ({ playerStore: { playQueue: h.playQueue } }));

import { launchLocalPlayback } from './launchLocalPlayback';
import { getCoverUrl, getLocalArtworkAlbumId } from '$lib/utils/errorHandling';
import { getApiUrl } from '$lib/api/api-utils';

const local = '11111111-1111-4111-8111-111111111111';
const provider = '22222222-2222-4222-8222-222222222222';
const raw = `/api/v1/library/albums/${local}/artwork/local`;
const tracks = [
	{
		track_file_id: 'file',
		title: 'Song',
		track_number: 1,
		disc_number: 1,
		size_bytes: 1,
		format: 'FLAC'
	}
];
beforeEach(() => {
	h.base = '';
	delete h.env.PUBLIC_API_URL;
	h.playQueue.mockReset();
});

describe('local playback artwork identity through URL normalization', () => {
	it.each([
		['', ''],
		['/music', ''],
		['/music', 'https://api.example.test'],
		['', 'https://api.example.test']
	])('keeps explicit local URLs idempotent at base %s and API origin %s', (base, origin) => {
		h.base = base;
		if (origin) h.env.PUBLIC_API_URL = origin;
		const expected = getApiUrl(raw);
		for (const coverUrl of [raw, `${base}${raw}`, expected]) {
			launchLocalPlayback(tracks, 0, false, {
				albumId: local,
				albumName: 'Local',
				artistName: 'Artist',
				coverUrl
			});
			const item = h.playQueue.mock.calls.at(-1)![0][0];
			expect(item.sourceType).toBe('local');
			expect(item.coverUrl).toBe(expected);
			expect(getCoverUrl(getCoverUrl(item.coverUrl, item.albumId), item.albumId)).toBe(expected);
			expect(getLocalArtworkAlbumId(item.coverUrl)).toBe(local);
		}
	});

	it('does not reinterpret a provider album ID merely because playback uses local audio', () => {
		h.base = '/music';
		h.env.PUBLIC_API_URL = 'https://api.example.test';
		launchLocalPlayback(tracks, 0, false, {
			albumId: provider,
			albumName: 'Provider',
			artistName: 'Artist',
			coverUrl: null
		});
		const item = h.playQueue.mock.calls[0][0][0];
		expect(item.sourceType).toBe('local');
		expect(item.albumId).toBe(provider);
		expect(item.coverUrl).toBe(getApiUrl(`/api/v1/covers/release-group/${provider}?size=250`));
		expect(getLocalArtworkAlbumId(item.coverUrl)).toBeNull();
	});

	it('rejects unrelated origins, wrong bases, unsafe IDs and mismatched album identity', () => {
		h.base = '/music';
		h.env.PUBLIC_API_URL = 'https://api.example.test';
		for (const url of [
			`https://evil.test/music${raw}`,
			`//api.example.test/music${raw}`,
			`/other${raw}`,
			`/api/v1/library/albums/%2Fetc/artwork/local`,
			`/api/v1/library/albums/%ZZ/artwork/local`
		]) {
			expect(getLocalArtworkAlbumId(url)).toBeNull();
		}
		expect(getCoverUrl(raw, provider)).toBe(
			getApiUrl(`/api/v1/covers/release-group/${provider}?size=250`)
		);
	});
});
