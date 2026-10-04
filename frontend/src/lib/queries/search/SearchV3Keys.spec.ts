import { describe, expect, it } from 'vitest';
import { SearchQueryKeyFactory } from './SearchQueryKeyFactory';

describe('SearchQueryKeyFactory.v3', () => {
	it('roots user-scoped v3 keys under the search prefix', () => {
		expect(SearchQueryKeyFactory.v3.root('user-a')).toEqual(['search', 'v3', 'user-a']);
	});

	it('normalizes a missing userId to null', () => {
		expect(SearchQueryKeyFactory.v3.root(undefined)).toEqual(['search', 'v3', null]);
	});

	it('differs per user (no cross-user collision)', () => {
		expect(SearchQueryKeyFactory.v3.suggest('user-a', 'ab', 5)).not.toEqual(
			SearchQueryKeyFactory.v3.suggest('user-b', 'ab', 5)
		);
	});

	it('folds the query text so case variants share one cache entry', () => {
		expect(SearchQueryKeyFactory.v3.suggest('user-a', '  ABBA ', 5)).toEqual(
			SearchQueryKeyFactory.v3.suggest('user-a', 'abba', 5)
		);
	});

	it('carries provider source identity only on the enrichment key', () => {
		const enrich = SearchQueryKeyFactory.v3.enrich('user-a', {
			artists: ['mbid-a'],
			albums: []
		});
		expect(enrich[3]).toEqual({
			user_id: 'user-a',
			source_mode: 'brainzmash',
			source_id: '',
			generation: 0
		});
		const suggest = SearchQueryKeyFactory.v3.suggest('user-a', 'abba', 5);
		expect(suggest.some((part) => typeof part === 'object' && part !== null)).toBe(false);
	});

	it('nests every v3 key under the versioned user root for prefix invalidation', () => {
		const v3 = SearchQueryKeyFactory.v3;
		const keys = [
			v3.unified('user-a', 'abba', { artists: 6, albums: 6, tracks: 6 }, null),
			v3.bucket('user-a', 'artists', 'abba', 24, 0),
			v3.suggest('user-a', 'abba', 5),
			v3.enrich('user-a', { artists: ['mbid-a'], albums: ['rg-1'] })
		];
		for (const key of keys) {
			expect([...key].slice(0, 3)).toEqual(['search', 'v3', 'user-a']);
		}
	});
});
