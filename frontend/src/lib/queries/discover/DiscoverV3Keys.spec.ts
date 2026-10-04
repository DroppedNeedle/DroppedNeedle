import { describe, expect, it } from 'vitest';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

const sourceKey = {
	user_id: 'user-a',
	source_mode: 'brainzmash',
	source_id: '',
	generation: 0
};

describe('DiscoverQueryKeyFactory.v3', () => {
	it('roots user-scoped v3 keys under the discover prefix', () => {
		expect(DiscoverQueryKeyFactory.v3.root('user-a')).toEqual(['discover', 'v3', 'user-a']);
	});

	it('normalizes a missing userId to null', () => {
		expect(DiscoverQueryKeyFactory.v3.root(undefined)).toEqual(['discover', 'v3', null]);
	});

	it('differs per user (no cross-user collision)', () => {
		expect(DiscoverQueryKeyFactory.v3.home('user-a')).not.toEqual(
			DiscoverQueryKeyFactory.v3.home('user-b')
		);
	});

	it('carries provider source identity on provider-backed keys only', () => {
		expect(DiscoverQueryKeyFactory.v3.home('user-a')[3]).toEqual(sourceKey);
		expect(DiscoverQueryKeyFactory.v3.queue('user-a', 10)[3]).toEqual(sourceKey);
		expect(DiscoverQueryKeyFactory.v3.radio('user-a', 'artist', 'mbid-1', {})[3]).toEqual(
			sourceKey
		);
		const batches = DiscoverQueryKeyFactory.v3.batches('user-a');
		expect(batches.some((part) => typeof part === 'object' && part !== null)).toBe(false);
		const ignored = DiscoverQueryKeyFactory.v3.ignored('user-a');
		expect(ignored.some((part) => typeof part === 'object' && part !== null)).toBe(false);
	});

	it('nests every v3 key under the versioned user root for prefix invalidation', () => {
		const v3 = DiscoverQueryKeyFactory.v3;
		const keys = [
			v3.home('user-a'),
			v3.queue('user-a', 10),
			v3.queueStatus('user-a'),
			v3.queueEnrich('user-a', 'rg-1'),
			v3.ignored('user-a'),
			v3.batches('user-a'),
			v3.batch('user-a', 'batch-1'),
			v3.radio('user-a', 'artist', 'mbid-1', {}),
			v3.radioPlan('user-a', { mode: 'library', seedType: 'artist', seedId: 'mbid-1' }),
			v3.playlistSuggestions('user-a', 'pl-1', 15),
			v3.albumPreview('user-a', 'Artist', 'Album', 4),
			v3.trackPreview('user-a', 'Artist', 'Track'),
			v3.youtubeSearch('user-a', 'Artist', 'Album'),
			v3.youtubeTrackSearch('user-a', 'Artist', 'Track'),
			v3.youtubeQuota('user-a'),
			v3.youtubeCacheCheck('user-a', [{ artist: 'Artist', track: 'Track' }]),
			v3.queueValidate('user-a', ['rg-1'])
		];
		expect(keys.length).toBe(17);
		for (const key of keys) {
			expect([...key].slice(0, 3)).toEqual(['discover', 'v3', 'user-a']);
		}
	});
});
